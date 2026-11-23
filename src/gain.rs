//! ReplayGain is measured on the encoded tracks. Nothing here applies gain to samples
//! or patches MPEG global_gain. The four reference programs disagree on their tag
//! containers, precision, and whether one file is an album.
mod coefficients;
mod mpeg;
mod rg1;
mod ogg_comments;

use std::{fs::File, path::{Path, PathBuf}};

use anyhow::{Context, Result, bail, ensure};
use lofty::{file::AudioFile, mp4::{Atom, AtomData, AtomIdent, Mp4File}, tag::TagExt};
use tempfile::NamedTempFile;

#[derive(Clone, Copy)]
struct Level { gain: f64, peak: f64 }

pub fn apply(format: &str, paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() { return Ok(()); }
    match format {
        "wav" | "opus" => Ok(()),
        "flac" | "ogg" => {
            let (tracks, album) = rg1::measure(paths, format)?;
            for (path, level) in paths.iter().zip(tracks) {
                xiph_tags(path, format, level, album).with_context(|| format!("tagging {}", path.display()))?;
            }
            Ok(())
        }
        "mp3" | "m4a" => mpeg_gain(paths, format),
        _ => bail!("unsupported ReplayGain format: {format}"),
    }
}

fn xiph_tags(path: &Path, format: &str, track: Level, album: Level) -> Result<()> {
    if format == "ogg" {
        return ogg_comments::replace(path, track, album);
    }
    // Stream-copy all packets, including FLAC pictures. Tag libraries that assemble
    // an entire FLAC in memory are unsuitable for a one-track image of a whole CD.
    let tmp = NamedTempFile::new_in(path.parent().context("missing output directory")?)?.into_temp_path();
    let mut cmd = crate::media::ffmpeg();
    cmd.arg("-i").arg(path).args(["-map", "0", "-c", "copy"]);
    let meta = "-metadata";
    cmd.args([meta, "REPLAYGAIN_REFERENCE_LOUDNESS=89.0 dB"]);
    for (name, level) in [("TRACK", track), ("ALBUM", album)] {
        cmd.arg(meta).arg(format!("REPLAYGAIN_{name}_GAIN={:+.2} dB", level.gain));
        cmd.arg(meta).arg(format!("REPLAYGAIN_{name}_PEAK={:.8}", level.peak));
    }
    let status = cmd.args(["-f", format, "-y"]).arg(&tmp).status()?;
    ensure!(status.success(), "FFmpeg metadata stream copy failed ({status})");
    tmp.persist(path)?;
    Ok(())
}

#[derive(Clone, Copy)]
struct MpegLevel { level: Level, min: u8, max: u8 }

fn mpeg_gain(paths: &[PathBuf], format: &str) -> Result<()> {
    let mut infos = Vec::new();
    let mut ranges = Vec::with_capacity(paths.len());
    for path in paths {
        let range = if format == "mp3" {
            let info = mpeg::mp3(path).with_context(|| format!("scanning {}", path.display()))?;
            let range = info.range;
            infos.push(info);
            range
        } else { mpeg::aac_range(path).with_context(|| format!("scanning {}", path.display()))? };
        ranges.push(range);
    }
    let (levels, album_level) = rg1::measure_mpeg(paths, format, &infos)?;
    let (min, max) = ranges.iter().fold((255u8, 0u8), |(a,b), &(lo,hi)| (a.min(lo), b.max(hi)));
    let album = (paths.len() > 1).then_some(MpegLevel { level: album_level, min, max });
    for ((path, level), (min,max)) in paths.iter().zip(levels).zip(ranges) {
        let track = MpegLevel { level, min, max };
        (if format == "mp3" { ape_tags(path, track, album) } else { mp4_tags(path, track, album) })
            .with_context(|| format!("tagging {}", path.display()))?;
    }
    Ok(())
}

fn ape_tags(path: &Path, track: MpegLevel, album: Option<MpegLevel>) -> Result<()> {
    let mut tag = mp3rgain::read_ape_tag_from_file(path)?.unwrap_or_default();
    for (name, range, measured) in [("TRACK", "MP3GAIN_MINMAX", Some(track)), ("ALBUM", "MP3GAIN_ALBUM_MINMAX", album)] {
        let Some(m) = measured else { continue };
        // mp3gain 1.5.2 reserves nine characters for the signed decimal, e.g.
        // +6.900000 but +26.90000, then appends the three characters " dB".
        let mut gain = format!("{:+.6}", m.level.gain);
        gain.truncate(9); gain.push_str(" dB");
        tag.set(&format!("REPLAYGAIN_{name}_GAIN"), &gain);
        tag.set(&format!("REPLAYGAIN_{name}_PEAK"), &format!("{:.6}", m.level.peak));
        tag.set(range, &format!("{:03},{:03}", m.min, m.max));
    }
    mp3rgain::write_ape_tag(path, &tag)?;
    Ok(())
}

fn mp4_tags(path: &Path, track: MpegLevel, album: Option<MpegLevel>) -> Result<()> {
    let mut mp4 = Mp4File::read_from(&mut File::open(path)?, Default::default())?;
    let mut ilst = mp4.remove_ilst().unwrap_or_default();
    for (name, measured) in [("track", Some(track)), ("album", album)] {
        let Some(m) = measured else { continue };
        // aacgain 1.9 uses bare two-place values even for a linear peak, not the
        // six-decimal APE or eight-decimal Vorbis convention. Names are lowercase.
        for (suffix, value) in [("gain", format!("{:.2}", m.level.gain)), ("peak", format!("{:.2}", m.level.peak)),
            ("minmax", format!("{},{}", m.min, m.max))] {
            let key = AtomIdent::Freeform { mean: "com.apple.iTunes".into(), name: format!("replaygain_{name}_{suffix}").into() };
            ilst.replace_atom(Atom::new(key, AtomData::UTF8(value)));
        }
    }
    ilst.save_to_path(path, Default::default())?;
    Ok(())
}
