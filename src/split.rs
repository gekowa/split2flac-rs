use anyhow::{bail, ensure, Context, Result};
use std::{collections::HashSet, fs, io::{self, Read, Seek, SeekFrom, Write}, path::{Path, PathBuf}, process::{Child, Command, Stdio}};
use tempfile::NamedTempFile;

use crate::{media::{self, PcmFormat}, tools::binary};

pub struct Output {
    pub path: PathBuf,
    pub number: usize,
    /// First decoded PCM byte. Legacy CD sectors can split a multichannel frame.
    pub start_byte: u64,
    /// Floor of the same offset in sample frames.
    pub start: u64,
    pub tags: Vec<(String, String)>,
}

/// A decoder with no declared sample count exited successfully exactly at
/// the last boundary. All preceding segments were encoded and published.
#[derive(Debug)]
pub struct EmptyFinalSegment;

impl std::fmt::Display for EmptyFinalSegment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("last track starts at end of audio")
    }
}
impl std::error::Error for EmptyFinalSegment {}

/// Only scalar encoder settings belong in -e. Never forward input/output,
/// muxer, filter, metadata, stream selection, or indirect `-/option file` options.
pub fn validate_args(format: &str, args: &[String]) -> Result<()> {
    media::encoder(format)?;
    ensure!(args.len() % 2 == 0, "-e expects FFmpeg encoder option/value pairs; migrate legacy encoder arguments");
    for pair in args.chunks_exact(2) {
        let (key, value) = (pair[0].as_str(), pair[1].as_str());
        let ok = match key {
            "-b:a" => matches!(format, "m4a" | "mp3" | "ogg" | "opus"),
            "-q:a" => matches!(format, "m4a" | "mp3" | "ogg"),
            "-compression_level" => matches!(format, "flac" | "opus" | "mp3"),
            "-cutoff" => matches!(format, "m4a" | "mp3" | "opus"),
            "-lpc_type" | "-lpc_passes" | "-lpc_coeff_precision" | "-min_partition_order" | "-max_partition_order" |
            "-min_prediction_order" | "-max_prediction_order" | "-prediction_order_method" | "-ch_mode" | "-exact_rice_parameters" => format == "flac",
            "-aac_coder" | "-aac_ms" | "-aac_is" | "-aac_tns" | "-aac_pns" => format == "m4a",
            "-reservoir" | "-joint_stereo" | "-abr" => format == "mp3",
            "-vbr" | "-application" | "-frame_duration" | "-packet_loss" | "-fec" => format == "opus",
            _ => false,
        };
        ensure!(ok, "unsupported -e option {key:?} for {format}; use scalar FFmpeg codec settings, not legacy encoder or I/O arguments");
        ensure!(!value.is_empty() && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"-+._".contains(&b))
            && (!value.starts_with('-') || value[1..].starts_with(|c: char| c.is_ascii_digit() || c == '.')),
            "invalid scalar encoder value {value:?} for {key}");
    }
    Ok(())
}

// stderr is inherited, never an undrained pipe. Early return while a sibling is
// blocked on stdin/stdout must terminate *and reap* it on every platform.
struct Process(Option<Child>);
impl Process {
    fn start(cmd: &mut Command) -> Result<Self> { Ok(Self(Some(cmd.spawn().context("starting FFmpeg")?))) }
    fn child(&mut self) -> &mut Child { self.0.as_mut().expect("active FFmpeg process") }
    fn finish(&mut self, what: &str) -> Result<()> {
        let status = self.child().wait().context("waiting for FFmpeg")?;
        self.0.take();
        ensure!(status.success(), "FFmpeg {what} failed: {status}");
        Ok(())
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() { let _ = c.kill(); let _ = c.wait(); }
    }
}

fn temporary(path: &Path) -> Result<NamedTempFile> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    NamedTempFile::new_in(dir).with_context(|| format!("creating temporary file in {}", dir.display()))
}

fn checked_dest(temp: NamedTempFile, path: &Path) -> Result<()> {
    temp.persist_noclobber(path).with_context(|| format!("publishing {} without overwriting", path.display()))?;
    Ok(())
}

fn decoder(input: &Path, pcm: &PcmFormat) -> Result<Process> {
    let mut c = media::ffmpeg();
    if let Some(codec) = pcm.input_codec { c.args(["-c:a", codec]); }
    c.args(["-err_detect", "crccheck+explode", "-protocol_whitelist", "file", "-format_whitelist", media::INPUT_FORMATS, "-i"])
        .arg(std::path::absolute(input)?)
        .args(["-map", "0:a:0", "-map_metadata", "-1", "-c:a", pcm.codec, "-f", pcm.raw, "pipe:1"])
        .stdin(Stdio::null()).stdout(Stdio::piped());
    Process::start(&mut c)
}

fn start_encoder(pcm: &PcmFormat, output: &Output, path: &Path, format: &str, args: &[String], cover: Option<&Path>) -> Result<Process> {
    let mut c = media::ffmpeg();
    c.args(["-y", "-f", pcm.raw, "-ar"]).arg(pcm.rate.to_string()).arg("-ac").arg(pcm.channels.to_string());
    if let Some(layout) = &pcm.layout { c.args(["-ch_layout", layout]); }
    c.args(["-i", "pipe:0"]);
    let picture = cover.filter(|_| output.number != 0 && matches!(format, "flac" | "m4a"));
    if let Some(p) = picture {
        // The prepared cover is always one JPEG, regardless of temporary name.
        c.args(["-f", "mjpeg", "-protocol_whitelist", "file", "-i"]).arg(std::path::absolute(p)?);
    }
    c.args(["-map", "0:a:0", "-map_metadata", "-1", "-map_chapters", "-1"]);
    if picture.is_some() {
        c.args(["-map", "1:v:0", "-c:v", "copy", "-disposition:v:0", "attached_pic",
            "-metadata:s:v:0", "title=Album cover", "-metadata:s:v:0", "comment=Cover (front)"]);
    }
    let codec = if format == "wav" { pcm.codec } else { media::encoder(format)? };
    c.args(["-c:a", codec, "-fflags", "+bitexact", "-metadata:s:a:0", "encoder="]);
    if format == "flac" {
        ensure!(!pcm.float && matches!(pcm.bits, 16 | 24 | 32) && pcm.channels <= 8 && pcm.rate < 1_048_576,
            "FFmpeg FLAC cannot preserve {}-bit {}-channel PCM at {} Hz (floating point: {})", pcm.bits, pcm.channels, pcm.rate, pcm.float);
        c.args(["-sample_fmt:a", if pcm.bits == 16 { "s16" } else { "s32" }, "-bits_per_raw_sample:a"]).arg(pcm.bits.to_string());
        if pcm.bits == 32 { c.args(["-strict:a", "experimental"]); }
    }
    c.args(args);
    if output.number != 0 && format != "wav" {
        for (key, value) in &output.tags { c.arg("-metadata").arg(format!("{key}={value}")); }
    }
    if format == "wav" { c.args(["-rf64", "auto"]); }
    let muxer = match format { "m4a" => "ipod", "opus" => "opus", f => f };
    c.args(["-f", muxer]).arg(std::path::absolute(path)?).stdin(Stdio::piped()).stdout(Stdio::null());
    Process::start(&mut c)
}

fn fill(reader: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match reader.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(count) => n += count,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// FFmpeg's WAV muxer writes the channel mask/subtype correctly but only
/// accepts whole frames. For legacy byte cuts remove the zero fill after muxing,
/// and correct all declared lengths (data chunks are *word* padded, not frame padded).
fn finish_wave(temp: &mut NamedTempFile, pcm: &PcmFormat, bytes: u64) -> Result<()> {
    let frame = u64::from(pcm.frame_bytes);
    let stored_bits = pcm.frame_bytes / u32::from(pcm.channels) * 8;
    if bytes % frame == 0 && stored_bits == u32::from(pcm.bits) { return Ok(()); }
    let file = temp.as_file_mut();
    file.rewind()?;
    let w = media::wave_info(file)?;
    ensure!(u64::from(w.frame_bytes) == frame && w.data_bytes == bytes.div_ceil(frame) * frame, "unexpected WAV encoder length");
    if stored_bits != u32::from(pcm.bits) {
        let at = w.valid_bits_at.context("WAV encoder cannot represent this effective bit depth")?;
        file.seek(SeekFrom::Start(at))?;
        file.write_all(&pcm.bits.to_le_bytes())?;
    }
    if bytes % frame == 0 { return Ok(()); }
    let end = w.data_at + bytes;
    file.seek(SeekFrom::Start(end))?;
    if bytes & 1 != 0 { file.write_all(&[0])?; }
    let len = end + (bytes & 1);
    file.set_len(len)?;
    if let Some(at) = w.ds64_at {
        file.seek(SeekFrom::Start(at))?;
        for x in [len - 8, bytes, bytes / frame] { file.write_all(&x.to_le_bytes())?; }
    } else {
        file.seek(SeekFrom::Start(4))?;
        file.write_all(&u32::try_from(len - 8)?.to_le_bytes())?;
        file.seek(SeekFrom::Start(w.data_at - 4))?;
        file.write_all(&u32::try_from(bytes)?.to_le_bytes())?;
    }
    if let Some(at) = w.fact_at {
        file.seek(SeekFrom::Start(at))?;
        file.write_all(&u32::try_from(bytes / frame).unwrap_or(u32::MAX).to_le_bytes())?;
    }
    Ok(())
}

pub fn run(input: &Path, pcm: &PcmFormat, outputs: &[Output], format: &str, args: &[String], cover: Option<&Path>) -> Result<()> {
    validate_args(format, args)?;
    ensure!(pcm.frame_bytes != 0 && outputs.first().is_some_and(|o| o.start_byte == 0), "invalid first split");
    let frame = pcm.frame_bytes as usize;
    let mut names = HashSet::new();
    for (i, o) in outputs.iter().enumerate() {
        ensure!(o.start == o.start_byte / frame as u64 && (i == 0 || outputs[i-1].start_byte < o.start_byte), "invalid split order");
        ensure!(names.insert(&o.path), "duplicate output: {}", o.path.display());
        match fs::symlink_metadata(&o.path) {
            Ok(_) => bail!("output already exists: {}", o.path.display()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (), Err(e) => return Err(e.into()),
        }
    }
    let expected = pcm.total_frames.map(|n| n.checked_mul(frame as u64).context("PCM byte count overflow")).transpose()?;
    if let Some(n) = expected { ensure!(outputs.last().unwrap().start_byte <= n, "split beyond end of audio"); }
    let mut decode = decoder(input, pcm)?;
    let mut source = decode.child().stdout.take().context("no decoder stdout")?;
    // All complete blocks end on track-relative frame boundaries, regardless of
    // pipe packet size. No lookahead or extra copy between consecutive tracks.
    let mut buffer = vec![0u8; (65536 / frame).max(1) * frame];
    for (i, out) in outputs.iter().enumerate() {
        let limit = outputs.get(i+1).map(|next| next.start_byte - out.start_byte);
        let mut tmp = temporary(&out.path)?;
        let mut enc = start_encoder(pcm, out, tmp.path(), format, args, cover)?;
        let mut pipe = enc.child().stdin.take().context("no encoder stdin")?;
        let mut sent = 0u64;
        loop {
            let request = limit.map_or(buffer.len(), |n| (n-sent).min(buffer.len() as u64) as usize);
            if request == 0 { break; }
            let n = fill(&mut source, &mut buffer[..request]).context("reading decoded PCM")?;
            if n < request && limit.is_some() { bail!("decoder ended before split at byte {}", out.start_byte + limit.unwrap()); }
            let whole = n / frame * frame;
            let write = if n != whole && format == "wav" {
                buffer[n..whole+frame].fill(0);
                whole + frame
            } else { whole };
            // Compressed codecs accept only whole frames; each unaligned byte
            // slice loses its own incomplete suffix, as in the reference FLAC.
            pipe.write_all(&buffer[..write]).with_context(|| format!("FFmpeg encoder stopped reading track {}", out.number))?;
            sent = sent.checked_add(n as u64).context("PCM length overflow")?;
            if n < request { break; }
        }
        drop(pipe);
        if limit.is_none() {
            decode.finish("decoding")?;
            let total = out.start_byte.checked_add(sent).context("PCM length overflow")?;
            ensure!(total % frame as u64 == 0, "decoder returned a partial source frame");
            if let Some(n) = expected { ensure!(n == total, "decoded {total} bytes; source declares {n}"); }
            if expected.is_none() && sent == 0 && i > 0 {
                return Err(EmptyFinalSegment.into());
            }
        }
        ensure!(sent != 0 && (format == "wav" || sent >= frame as u64), "empty track {} at end of input", out.number);
        enc.finish("encoding")?;
        if format == "wav" { finish_wave(&mut tmp, pcm, sent)?; }
        checked_dest(tmp, &out.path)?;
    }
    Ok(())
}

fn embedded_cover(input: &Path) -> Result<Option<usize>> {
    if !input.extension().is_some_and(|e| e.eq_ignore_ascii_case("flac")) { return Ok(None); }
    let out = Command::new(binary("ffprobe")).args(["-v", "error", "-f", "flac", "-protocol_whitelist", "file", "-select_streams", "v", "-show_entries",
        "stream=index:stream_disposition=attached_pic:stream_tags=comment", "-of", "json", "-i"]).arg(std::path::absolute(input)?).stdin(Stdio::null()).output()?;
    ensure!(out.status.success(), "probing embedded FLAC picture failed: {}", String::from_utf8_lossy(&out.stderr));
    let data: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let streams = data["streams"].as_array().context("missing picture stream list")?;
    let mut first = None;
    for s in streams {
        if s["disposition"]["attached_pic"].as_i64() != Some(1) { continue; }
        let index = usize::try_from(s["index"].as_u64().context("invalid picture stream index")?)?;
        if s["tags"]["comment"].as_str() == Some("Cover (front)") { return Ok(Some(index)); }
        first.get_or_insert(index);
    }
    Ok(first)
}

pub fn cover(input: &Path, explicit_or_discovered: Option<&Path>, size: &str, dest: &Path) -> Result<bool> {
    let (w, h) = size.split_once('x').context("cover size must be WxH")?;
    let (w, h) = (w.parse::<u32>()?, h.parse::<u32>()?);
    ensure!(w > 0 && h > 0 && w <= 65535 && h <= 65535, "invalid JPEG dimensions");
    let (source, stream) = match explicit_or_discovered {
        Some(p) => (p, "0:v:0".to_owned()),
        None => match embedded_cover(input)? { Some(i) => (input, format!("0:{i}")), None => return Ok(false) },
    };
    let tmp = temporary(dest)?;
    let result = media::ffmpeg().args(["-y", "-protocol_whitelist", "file", "-i"]).arg(std::path::absolute(source)?)
        .args(["-map", &stream, "-frames:v", "1", "-vf", &format!("scale={w}:{h}:force_original_aspect_ratio=decrease"),
            "-c:v", "mjpeg", "-pix_fmt", "yuvj444p", "-f", "image2pipe"]).arg(std::path::absolute(tmp.path())?).stdin(Stdio::null()).status()?;
    ensure!(result.success() && tmp.as_file().metadata()?.len() != 0, "cover conversion failed for {}", source.display());
    checked_dest(tmp, dest)?;
    Ok(true)
}
