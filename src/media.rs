use anyhow::{bail, ensure, Context, Result};
use serde::Deserialize;
use std::{fs::File, io::{Read, Seek, SeekFrom}, path::Path, process::{Command, Stdio}};

use crate::tools::binary;

#[derive(Clone, Debug)]
pub struct PcmFormat {
    pub rate: u32,
    pub channels: u16,
    /// Significant sample bits (not the width of FFmpeg's decoded AVFrame).
    pub bits: u16,
    pub frame_bytes: u32,
    pub total_frames: Option<u64>,
    pub(crate) raw: &'static str,
    pub(crate) codec: &'static str,
    pub(crate) input_codec: Option<&'static str>,
    pub(crate) layout: Option<String>,
    pub(crate) float: bool,
}

pub fn encoder(format: &str) -> Result<&'static str> {
    Ok(match format {
        "flac" => "flac", "m4a" => "aac", "mp3" => "libmp3lame", "ogg" => "libvorbis",
        "opus" => "libopus", "wav" => "pcm_s16le", _ => bail!("unknown format {format}"),
    })
}

pub fn encoder_help(format: &str) -> Result<()> {
    let encoder = encoder(format)?;
    let status = Command::new(binary("ffmpeg")).args(["-hide_banner", "-h", &format!("encoder={encoder}")]).status().context("starting FFmpeg encoder help")?;
    ensure!(status.success(), "FFmpeg encoder help failed: {status}");
    Ok(())
}

pub(crate) fn ffmpeg() -> Command {
    let mut cmd = Command::new(binary("ffmpeg"));
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-xerror"]);
    cmd
}

pub(crate) const INPUT_FORMATS: &str = "ape,flac,tta,wv,wav,w64";

#[derive(Deserialize)]
struct Audio {
    codec_name: String, sample_rate: String, channels: u16, sample_fmt: String,
    channel_layout: Option<String>, bits_per_sample: Option<u16>, bits_per_raw_sample: Option<String>,
    duration_ts: Option<u64>, time_base: Option<String>,
}
#[derive(Deserialize)]
struct Probed { streams: Vec<Audio>, format: Container }
#[derive(Deserialize)]
struct Container { format_name: String }

/// Locations and PCM attributes that a demuxer duration estimate cannot tell us.
/// Chunk sizes are byte counts, including for RF64 (where the real size is in ds64).
pub(crate) struct Wave {
    pub data_at: u64,
    pub data_bytes: u64,
    pub frame_bytes: u16,
    pub bits: u16,
    pub valid_bits: u16,
    pub valid_bits_at: Option<u64>,
    extensible_integer_le: bool,
    pub fact_at: Option<u64>,
    pub ds64_at: Option<u64>,
}

pub(crate) fn wave_info(file: &mut File) -> Result<Wave> {
    let file_len = file.metadata()?.len();
    let mut hdr = [0u8; 12];
    file.read_exact(&mut hdr)?;
    let be = &hdr[..4] == b"RIFX";
    let wide = matches!(&hdr[..4], b"RF64" | b"BW64");
    ensure!((be || wide || &hdr[..4] == b"RIFF") && &hdr[8..] == b"WAVE", "invalid RIFF/WAVE header");
    let word = |b: &[u8]| if be { u16::from_be_bytes(b.try_into().unwrap()) } else { u16::from_le_bytes(b.try_into().unwrap()) };
    let dword = |b: &[u8]| if be { u32::from_be_bytes(b.try_into().unwrap()) } else { u32::from_le_bytes(b.try_into().unwrap()) };
    let riff = dword(&hdr[4..8]);
    if !wide && riff != u32::MAX && riff != 0 {
        ensure!(u64::from(riff) + 8 <= file_len, "truncated WAV: declared RIFF length exceeds the file");
    }
    let mut fmt = None;
    let mut data64 = None;
    let mut fact_at = None;
    let mut ds64_at = None;
    loop {
        let mut chunk = [0u8; 8];
        file.read_exact(&mut chunk).context("missing WAV data chunk")?;
        let at = file.stream_position()?;
        let size = u64::from(dword(&chunk[4..]));
        if &chunk[..4] == b"data" {
            let size = if wide { data64.context("RF64 data without ds64")? }
                else if size == u64::from(u32::MAX) || (size == 0 && riff == u32::MAX) { file_len - at } else { size };
            ensure!(size <= file_len - at, "short WAV data: header declares {size} bytes, only {} available", file_len - at);
            let (frame_bytes, bits, valid_bits, valid_bits_at, extensible_integer_le) = fmt.context("WAV data before format")?;
            return Ok(Wave { data_at: at, data_bytes: size, frame_bytes, bits, valid_bits, valid_bits_at, extensible_integer_le, fact_at, ds64_at });
        }
        ensure!(size <= file_len - at, "truncated WAV chunk");
        if &chunk[..4] == b"fmt " && fmt.is_none() {
            ensure!(size >= 16, "invalid WAV format chunk");
            let mut b = [0u8; 40];
            let n = size.min(40) as usize;
            file.read_exact(&mut b[..n])?;
            let bits = word(&b[14..16]);
            let ext = word(&b[..2]) == 0xfffe;
            ensure!(!ext || (n >= 40 && word(&b[16..18]) >= 22), "short WAVEFORMATEXTENSIBLE header");
            let valid = if ext { word(&b[18..20]) } else { bits };
            let integer_le = ext && !be && b[24..40] == [1,0,0,0,0,0,16,0,128,0,0,170,0,56,155,113];
            fmt = Some((word(&b[12..14]), bits, if valid == 0 { bits } else { valid }, ext.then_some(at + 18), integer_le));
        } else if &chunk[..4] == b"ds64" && wide {
            ensure!(size >= 24, "invalid RF64 ds64");
            let mut sizes = [0; 24];
            file.read_exact(&mut sizes)?;
            ensure!(u64::from_le_bytes(sizes[..8].try_into().unwrap()) <= file_len - 8, "truncated RF64 container");
            data64 = Some(u64::from_le_bytes(sizes[8..16].try_into().unwrap()));
            ds64_at = Some(at);
        } else if &chunk[..4] == b"fact" && size >= 4 { fact_at = Some(at); }
        file.seek(SeekFrom::Start(at + size + (size & 1)))?;
    }
}

fn pcm_representation(bytes: u32, float: bool) -> Result<(&'static str, &'static str)> {
    Ok(match (bytes, float) {
        (1, false) => ("u8", "pcm_u8"), (2, false) => ("s16le", "pcm_s16le"),
        (3, false) => ("s24le", "pcm_s24le"), (4, false) => ("s32le", "pcm_s32le"),
        (8, false) => ("s64le", "pcm_s64le"), (4, true) => ("f32le", "pcm_f32le"),
        (8, true) => ("f64le", "pcm_f64le"), _ => bail!("unsupported {bytes}-byte PCM sample (float={float})"),
    })
}

fn duration(stream: &Audio, rate: u32) -> Option<u64> {
    let (num, den) = stream.time_base.as_ref()?.split_once('/')?;
    let den = den.parse::<u128>().ok().filter(|n| *n != 0)?;
    let n = u128::from(stream.duration_ts?).checked_mul(num.parse().ok()?)?.checked_mul(u128::from(rate))?;
    (n % den == 0).then(|| u64::try_from(n / den).ok()).flatten()
}

fn flac_length(path: &Path) -> Result<Option<u64>> {
    // FFmpeg may estimate a duration when STREAMINFO's 36-bit count is zero.
    // An ID3-prefixed FLAC also works; when not at offset zero, just count at EOF.
    let mut info = [0u8; 26];
    File::open(path)?.read_exact(&mut info)?;
    if &info[..4] != b"fLaC" || info[4] & 0x7f != 0 { return Ok(None); }
    let count = u64::from_be_bytes(info[18..26].try_into().unwrap()) & 0xf_ffff_ffff;
    Ok((count != 0).then_some(count))
}

pub fn probe(input: &Path) -> Result<PcmFormat> {
    let out = Command::new(binary("ffprobe")).args(["-v", "error", "-protocol_whitelist", "file", "-format_whitelist", INPUT_FORMATS,
        "-select_streams", "a:0", "-show_entries", "format=format_name:stream=codec_name,sample_rate,channels,sample_fmt,channel_layout,bits_per_sample,bits_per_raw_sample,duration_ts,time_base",
        "-of", "json", "-i"]).arg(std::path::absolute(input)?).stdin(Stdio::null()).output().context("starting ffprobe")?;
    ensure!(out.status.success(), "ffprobe failed for {}: {}", input.display(), String::from_utf8_lossy(&out.stderr).trim());
    let probe: Probed = serde_json::from_slice(&out.stdout).context("invalid ffprobe audio information")?;
    let s = probe.streams.into_iter().next().context("no audio stream")?;
    let rate: u32 = s.sample_rate.parse()?;
    ensure!(rate > 0 && s.channels > 0, "invalid PCM rate/channels");
    let mut input_codec = None;
    let mut float = s.sample_fmt.starts_with("flt") || s.sample_fmt.starts_with("dbl");
    let (bits, bytes, total_frames) = if probe.format.format_name == "wav" {
        let w = wave_info(&mut File::open(input)?)?;
        ensure!(w.frame_bytes != 0 && w.frame_bytes % s.channels == 0, "invalid WAV block alignment");
        let bytes = u32::from(w.frame_bytes / s.channels);
        ensure!(u32::from(w.bits).div_ceil(8) == bytes && w.valid_bits > 0 && w.valid_bits <= w.bits, "invalid WAV bit depth");
        ensure!(w.data_bytes % u64::from(w.frame_bytes) == 0, "WAV ends in a partial PCM frame");
        // FFmpeg also uses 24 bits in a 32-bit word for Adobe's legacy
        // floating-point WAV. The extensible PCM subtype is unambiguous: decode
        // as the container word and preserve its left-aligned significant bits.
        if w.extensible_integer_le && w.valid_bits < w.bits {
            input_codec = Some(pcm_representation(bytes, false)?.1);
            float = false;
        }
        (w.valid_bits, bytes, Some(w.data_bytes / u64::from(w.frame_bytes)))
    } else {
        let bits = s.bits_per_raw_sample.as_deref().and_then(|v| v.parse::<u16>().ok()).filter(|&v| v > 0)
            .or(s.bits_per_sample.filter(|&v| v > 0)).or_else(|| match s.sample_fmt.trim_end_matches('p') {
                "flt" => Some(32), "dbl" => Some(64), _ => None,
            }).context("ffprobe did not identify the lossless source bit depth")?;
        let total = if s.codec_name == "flac" { flac_length(input)? } else { duration(&s, rate) };
        (bits, u32::from(bits).div_ceil(8), total)
    };
    ensure!(input_codec.is_some() || matches!(s.codec_name.as_str(), "ape" | "tta" | "wavpack" | "flac" | "pcm_u8" | "pcm_s16le" | "pcm_s16be" | "pcm_s24le" | "pcm_s24be" | "pcm_s32le" | "pcm_s32be" | "pcm_s64le" | "pcm_s64be" | "pcm_f32le" | "pcm_f32be" | "pcm_f64le" | "pcm_f64be"), "not a supported lossless PCM source: {}", s.codec_name);
    let (raw, codec) = pcm_representation(bytes, float)?;
    Ok(PcmFormat { rate, channels: s.channels, bits, frame_bytes: bytes * u32::from(s.channels), total_frames,
        raw, codec, input_codec, layout: s.channel_layout.filter(|s| s != "unknown"), float })
}
