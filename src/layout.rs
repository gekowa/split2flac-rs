use anyhow::{Context, Result, bail, ensure};
use crate::{cue::{Offset, Sheet}, media::PcmFormat};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub number: usize,
    /// Complete source sample frames preceding the start (floor for broken alignment).
    pub start: u64,
    /// Authoritative physical start: shntool can cut halfway through a PCM frame.
    pub start_byte: u64,
}

pub fn plan(sheet: &Sheet, frame_bytes: u32, total_frames: Option<u64>) -> Result<Vec<Segment>> {
    boundaries(sheet, frame_bytes, total_frames, None)
}

pub fn plan_pcm(sheet: &Sheet, pcm: &PcmFormat) -> Result<Vec<Segment>> {
    boundaries(sheet, pcm.frame_bytes, pcm.total_frames, Some((pcm.rate, pcm.rate == 44100 && pcm.channels == 2 && pcm.bits == 16)))
}

fn boundaries(sheet: &Sheet, width: u32, frames: Option<u64>, clock: Option<(u32, bool)>) -> Result<Vec<Segment>> {
    ensure!(width > 0, "invalid zero-width PCM frames");
    let width = u64::from(width);
    let eof = frames.map(|n| n.checked_mul(width).context("PCM size overflow")).transpose()?;
    ensure!(eof != Some(0), "empty input has no split points");
    // v122 feeds shnsplit the *byte length of the image* from `shncue -s image image`
    // in the single-track case. No CUE INDEX (not even a leading nonzero one) is used.
    if sheet.tracks.len() == 1 { return Ok(vec![Segment { number: 1, start: 0, start_byte: 0 }]); }
    ensure!(!sheet.tracks.is_empty(), "no tracks");
    let mut pieces = Vec::with_capacity(sheet.tracks.len() + 1);
    let mut last = 0;
    let mut first_number = 1;
    for p in &sheet.points {
        let byte = match p.offset {
            Offset::Invalid => bail!("invalid split point on CUE line {}", p.line),
            Offset::Bytes(b) => b,
            Offset::Cd(cd) => cd.checked_mul(2352).context("CD byte offset overflow")?,
            Offset::Seconds(s) | Offset::Millis(s, _) => {
                let (rate, is_cd) = clock.context("second-based split point requires the PCM sample rate")?;
                let bytes_per_second = u64::from(rate) * width;
                let whole = s.checked_mul(bytes_per_second).context("split point overflow")?;
                if let Offset::Millis(_, millis) = p.offset {
                    let nearest = (u128::from(millis) * u128::from(bytes_per_second) + 500) / 1000;
                    let nearest = u64::try_from(nearest)?;
                    // shntool 3.0.10's is_m_ss_nnn adds the sector count only
                    // in its *rounding* branch. Exact matches leave `whole` alone.
                    let extra = if is_cd {
                        let sector = ((millis * 75 + 500) / 1000) * 2352;
                        let sector = if sector == 0 && whole == 0 { 2352 } else { sector };
                        if sector == nearest { 0 } else { sector }
                    } else { nearest };
                    whole.checked_add(extra).context("split point overflow")?
                } else { whole }
            }
        };
        if byte == 0 && last == 0 { continue; }
        ensure!(byte > last, "split point on CUE line {} is not greater than its predecessor", p.line);
        if !sheet.raw && pieces.is_empty() && p.track == 1 { first_number = 0; }
        ensure!(pieces.len() < 255, "too many split points");
        pieces.push(Segment { number: first_number + pieces.len(), start: last / width, start_byte: last });
        last = byte;
    }
    ensure!(!pieces.is_empty(), "no split points given");
    if !sheet.raw {
        let named = sheet.shn_tracks + usize::from(first_number == 0);
        ensure!(pieces.len() < named, "more physical split points than tracks in CUE sheet");
    }
    ensure!(eof.is_none_or(|end| last <= end), "split points go beyond input data size");
    // No zero-length trailing output. Consumers rename/tag existing segments
    // first; a subsequent missing track must NOT erase those completed files.
    if Some(last) != eof {
        pieces.push(Segment { number: first_number + pieces.len(), start: last / width, start_byte: last });
    }
    Ok(pieces)
}
