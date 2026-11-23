//! Inspect encoded side information without loading a complete track or retaining
//! a per-frame index. FFprobe supplies MP3 packet boundaries; for AAC, MP4 packets
//! are streamed as ADTS (stream copy, not an encode) to mp3rgain's raw AAC parser.
use std::{fs::File, io::{BufRead, BufReader, Read, Seek, SeekFrom}, path::Path, process::{Command, Stdio}};
use anyhow::{Context, Result, ensure};
use mp3rgain::MpegVersion;
use crate::{media, tools::binary};

pub(super) fn stream<T>(mut cmd: Command, read: impl FnOnce(&mut BufReader<std::process::ChildStdout>) -> Result<T>) -> Result<T> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).spawn()?;
    let result = read(&mut BufReader::new(child.stdout.take().context("missing codec stdout")?));
    if result.is_err() { let _ = child.kill(); }
    let status = child.wait()?;
    let value = result?;
    ensure!(status.success(), "gain helper exited with {status}");
    Ok(value)
}

pub(super) struct Mp3Info { pub rate: u32, pub channels: usize, pub frames: u64, pub range: (u8,u8) }

pub(super) fn mp3(path: &Path) -> Result<Mp3Info> {
    let mut cmd = Command::new(binary("ffprobe"));
    cmd.args(["-v", "error", "-select_streams", "a:0", "-show_entries", "packet=size,pos:packet_side_data=", "-of", "csv=p=0", "-i"]).arg(path);
    let mut source = BufReader::new(File::open(path)?);
    stream(cmd, |packets| {
        let mut line = String::new();
        let mut buf = [0u8; 4096]; // Largest non-free-format Layer III frame is < 3 KiB.
        let mut end = 0;
        let mut samples = 0;
        let mut info = Mp3Info { rate: 0, channels: 0, frames: 0, range: (255, 0) };
        let mut previous = None;
        loop {
            line.clear();
            if packets.read_line(&mut line)? == 0 { break; }
            if line.trim().is_empty() { continue; }
            let mut fields = line.trim().split(',');
            let size: usize = fields.next().context("missing packet size")?.parse()?;
            let pos: u64 = fields.next().context("missing packet offset")?.parse()?;
            ensure!((4..=buf.len()).contains(&size), "invalid MPEG packet size {size}");
            if pos != end { source.seek(SeekFrom::Start(pos))?; }
            source.read_exact(&mut buf[..size])?;
            end = pos.checked_add(size as u64).context("MPEG packet offset overflow")?;
            let frame = mp3rgain::analyze_data(&buf[..size])?;
            if info.rate == 0 {
                let (div, count) = match frame.mpeg_version() { MpegVersion::Mpeg1 => (1,1152), MpegVersion::Mpeg2 => (2,576), MpegVersion::Mpeg25 => (4,576), _ => anyhow::bail!("unsupported MPEG version") };
                samples = count;
                let rate = [44100,48000,32000].get(usize::from((buf[2] >> 2) & 3)).context("invalid MPEG rate")?;
                info.rate = rate / div;
                info.channels = frame.channel_mode().channel_count();
            }
            // mpglib waits for a following frame on startup and is never flushed by
            // mp3gain 1.5.2. Include neither the last frame's PCM nor its global_gain.
            if let Some((lo, hi)) = previous.replace((frame.min_gain(), frame.max_gain())) {
                info.range.0 = info.range.0.min(lo); info.range.1 = info.range.1.max(hi);
                info.frames += samples;
            }
        }
        ensure!(info.frames > 0, "not enough MPEG frames");
        Ok(info)
    })
}

pub(super) fn aac_range(path: &Path) -> Result<(u8, u8)> {
    let mut cmd = media::ffmpeg();
    cmd.arg("-i").arg(path).args(["-map", "0:a:0", "-c:a", "copy", "-f", "adts", "-"]);
    stream(cmd, |input| {
        // The library collects all gain locations for its frame-editing API. Bound
        // that vector by giving it batches of complete access units, not a whole CD.
        let mut batch = Vec::with_capacity(65536+8192);
        let mut range: Option<(u8, u8)> = None;
        loop {
            let eof = input.fill_buf()?.is_empty();
            if (eof || batch.len() >= 65536) && !batch.is_empty() {
                let r = mp3rgain::adts::analyze_adts_gains_from_data(&batch)?;
                ensure!(r.parse_warnings() == 0, "unrecognized AAC side information");
                let (min,max) = range.unwrap_or((255,0));
                range = Some((min.min(r.min_gain()), max.max(r.max_gain())));
                batch.clear();
            }
            if eof { break; }
            let mut h = [0u8; 7]; input.read_exact(&mut h)?;
            ensure!(h[0] == 255 && h[1] & 0xf6 == 0xf0, "invalid ADTS header");
            let size = usize::from(h[3] & 3) << 11 | usize::from(h[4]) << 3 | usize::from(h[5] >> 5);
            ensure!(size >= h.len(), "invalid ADTS length");
            batch.extend_from_slice(&h);
            let start = batch.len();
            batch.resize(start + size - h.len(), 0);
            input.read_exact(&mut batch[start..]).context("incomplete AAC packet")?;
        }
        range.context("no AAC packets")
    })
}
