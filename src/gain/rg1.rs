//! ReplayGain 1.0: filter one track at a time and pool complete RMS windows.
use std::{fs::File, io::{BufReader, Read}, path::{Path, PathBuf}};

use anyhow::{Context, Result, ensure};
use vorbis_rs::VorbisDecoder;

use super::{Level, coefficients::{Coefficients, FILTERS}, mpeg};
use crate::media;

const BINS: usize = 12000;

trait Sample: Copy + Default + Into<f64> + std::ops::Mul<Output=Self> + std::ops::Sub<Output=Self> {
    fn narrow(x: f64) -> Self;
}
impl Sample for f32 { fn narrow(x: f64) -> Self { x as f32 } }
impl Sample for f64 { fn narrow(x: f64) -> Self { x } }

#[derive(Default)]
struct History<T> {
    input: [T; 16],
    output: [T; 16],
    position: usize,
}

impl<T: Sample> History<T> {
    // A power-of-two ring holds even the 10-pole Yule history without moving samples.
    // The reference multiplies and subtracts in float, but accumulates terms in double.
    #[inline]
    fn filter<const N: usize>(&mut self, sample: T, b: &[T; N], a: &[T; N]) -> T {
        let pos = self.position;
        let mut y: f64 = (b[0] * sample).into();
        for tap in 1..N {
            let i = pos.wrapping_sub(tap) & 15;
            y += (self.input[i] * b[tap] - self.output[i] * a[tap]).into();
        }
        self.input[pos] = sample;
        self.output[pos] = T::narrow(y);
        self.position = (pos + 1) & 15;
        self.output[pos]
    }
}

#[derive(Default)]
struct Channel<T> { yule: History<T>, butter: History<T> }
impl<T: Sample> Channel<T> {
    #[inline]
    fn energy(&mut self, x: T, c: &Coefficients<T>) -> f64 {
        let y = self.yule.filter(x, &c.yule_b, &c.yule_a);
        let z = self.butter.filter(y, &c.butter_b, &c.butter_a);
        (z * z).into()
    }
}

struct Meter<T: Sample> {
    coeff: Coefficients<T>,
    channels: [Channel<T>; 2],
    hist: Vec<u64>,
    window: u32,
    frames: u32,
    energy: f64,
    peak: f64,
    step: u32,
    skip: u32,
}

impl<T: Sample> Meter<T> {
    fn new(mut rate: u32, format: &str) -> Result<Self> {
        let mut step = 1;
        if format == "flac" {
            while rate > 48000 { rate /= 2; step *= 2; }
        } else {
            let high_aac = format == "m4a" && matches!(rate, 64000|88200|96000);
            ensure!(high_aac || matches!(rate, 8000|11025|12000|16000|22050|24000|32000|44100|48000), "unsupported {format} ReplayGain rate {rate} Hz");
        }
        let c = FILTERS.iter().find(|c| c.rate == rate).context("unsupported ReplayGain sample rate")?;
        let coeff = Coefficients { rate, yule_a: c.yule_a.map(T::narrow), yule_b: c.yule_b.map(T::narrow),
            butter_a: c.butter_a.map(T::narrow), butter_b: c.butter_b.map(T::narrow) };
        Ok(Self { coeff, channels: Default::default(), hist: vec![0; BINS], window: rate.div_ceil(20),
            frames: 0, energy: 0., peak: 0., step, skip: 0 })
    }

    #[inline]
    fn frame(&mut self, l: T, r: Option<T>) {
        if self.skip != 0 { self.skip -= 1; return; }
        self.skip = self.step - 1;
        let l = self.channels[0].energy(l, &self.coeff);
        let r = r.map_or(l, |v| self.channels[1].energy(v, &self.coeff));
        self.energy += l + r;
        self.frames += 1;
        if self.frames == self.window {
            let mean = self.energy / (2. * f64::from(self.window));
            let bin = ((mean + 1e-37).log10() * 1000.) as usize;
            self.hist[bin.min(BINS-1)] += 1;
            self.energy = 0.; self.frames = 0;
        }
    }
}

fn gain(hist: &[u64], flac: bool) -> f64 {
    let count = hist.iter().sum::<u64>();
    if count == 0 { return -24601.; } // Literal written by both tools for <50 ms.
    // metaflac fixed the floating-point ceil issue for multiples of 20. VorbisGain
    // 0.37 still evaluates (1.0 - 0.95) in double, which is slightly above 0.05.
    let mut remaining = if flac { count.div_ceil(20) } else { (count as f64 * (1.0 - 0.95)).ceil() as u64 };
    for (bin, &n) in hist.iter().enumerate().rev() {
        if remaining <= n { return 64.82 - bin as f64 / 100.; }
        remaining -= n;
    }
    unreachable!("rank cannot exceed number of windows")
}

impl Meter<f32> {
    fn xiph_frame(&mut self, left: f32, right: Option<f32>, scale: f32) {
        let l = left * scale;
        let r = right.map(|s| s * scale);
        self.peak = self.peak.max(f64::from(l.abs().max(r.unwrap_or(0.).abs())));
        self.frame(l, r);
    }
}

fn ogg_meter(path: &Path) -> Result<Meter<f32>> {
    // libvorbisfile's end trimming differs from the FFmpeg Vorbis demuxer, which can
    // lose enough samples to remove a whole RMS window. Use the same decoder family
    // as vorbisgain (linked via Cargo, no external Vorbis tool or DLL to locate).
    let mut decoder = VorbisDecoder::<BufReader<File>>::new(BufReader::new(File::open(path)?))?;
    ensure!(decoder.channels().get() <= 2, "vorbisgain requires mono or stereo");
    let mut meter = Meter::new(decoder.sampling_frequency().get(), "ogg")?;
    while let Some(block) = decoder.decode_audio_block()? {
        let samples = block.samples();
        if let [left, right] = samples {
            for (&l, &r) in left.iter().zip(*right) { meter.xiph_frame(l, Some(r), 32767.); }
        } else {
            for &l in samples[0] { meter.xiph_frame(l, None, 32767.); }
        }
    }
    Ok(meter)
}

fn pcm_frames(path: &Path, channels: usize, input_options: &[&str], mut consume: impl FnMut(&[u8])) -> Result<()> {
    ensure!((1..=64).contains(&channels), "invalid PCM channel count {channels}");
    let mut cmd = media::ffmpeg();
    cmd.args(input_options).arg("-i").arg(path).args(["-map", "0:a:0", "-f", "f32le", "-"]);
    let width = channels * 4;
    mpeg::stream(cmd, |pcm_stream| {
        let mut buf = [0u8; 32768]; let mut carry = 0;
        loop {
            let n = pcm_stream.read(&mut buf[carry..])?;
            if n == 0 { ensure!(carry == 0, "partial decoded PCM frame"); return Ok(()); }
            let bytes = n + carry;
            let frames = buf[..bytes].chunks_exact(width);
            carry = frames.remainder().len();
            for frame in frames { consume(frame); }
            buf.copy_within(bytes-carry..bytes, 0);
        }
    })
}

fn sample(bytes: &[u8]) -> f32 { f32::from_le_bytes(bytes.try_into().unwrap()) }

fn flac_meter(path: &Path) -> Result<Meter<f32>> {
    let pcm = media::probe(path)?;
    ensure!((1..=2).contains(&pcm.channels), "metaflac ReplayGain requires mono or stereo");
    let mut meter = Meter::new(pcm.rate, "flac")?;
    pcm_frames(path, usize::from(pcm.channels), &[], |frame| {
        meter.xiph_frame(sample(&frame[..4]), (frame.len() == 8).then(|| sample(&frame[4..])), 32768.);
    })?;
    Ok(meter)
}

fn aac_meter(path: &Path) -> Result<Meter<f64>> {
    use lofty::{config::ParseOptions, file::AudioFile, mp4::Mp4File};
    let mp4 = Mp4File::read_from(&mut File::open(path)?, ParseOptions::new().read_tags(false))?;
    let channels = usize::from(mp4.properties().channels());
    let mut meter = Meter::new(mp4.properties().sample_rate(), "m4a")?;
    // FAAD discards its first AAC-LC transform, ignores the MP4 edit list, and
    // decodes the final padded access unit. Those choices set the window phase.
    let mut priming = 1024;
    pcm_frames(path, channels, &["-ignore_editlist", "1", "-flags2", "+skip_manual"], |frame| {
        if priming != 0 { priming -= 1; return; }
        let mut values = frame.chunks_exact(4).map(|b| {
            let v = f64::from(sample(b));
            meter.peak = meter.peak.max(v.abs());
            v * 32768.
        });
        let (left, right) = match channels {
            1 => (values.next().unwrap(), None),
            2 => (values.next().unwrap(), values.next()),
            6 => {
                // FFmpeg native order: L, R, C, LFE, surround-L, surround-R.
                let [l, r, c, lfe, sl, sr] = std::array::from_fn(|_| values.next().unwrap());
                let k = std::f64::consts::FRAC_1_SQRT_2;
                (l + (c+sl)*k + lfe, Some(r + (c+sr)*k + lfe))
            }
            _ => (values.sum::<f64>() / channels as f64, None),
        };
        meter.frame(left, right);
    })?;
    ensure!(meter.hist.iter().any(|&n| n != 0), "not enough AAC samples for ReplayGain");
    Ok(meter)
}

fn mp3_meter(path: &Path, info: &mpeg::Mp3Info) -> Result<Meter<f64>> {
    let mut meter = Meter::new(info.rate, "mp3")?;
    let mut remaining = info.frames;
    pcm_frames(path, info.channels, &["-flags2", "+skip_manual", "-c:a", "mp3float"], |frame| {
        if remaining == 0 { return; }
        remaining -= 1;
        let l = f64::from(sample(&frame[..4])) * 32767.;
        let r = (frame.len() == 8).then(|| f64::from(sample(&frame[4..])) * 32767.);
        meter.peak = meter.peak.max(l.abs()).max(r.unwrap_or(0.).abs());
        meter.frame(l, r);
    })?;
    ensure!(remaining == 0 && meter.hist.iter().any(|&n| n != 0), "not enough decoded MP3 samples");
    meter.peak /= 32768.;
    Ok(meter)
}

pub(super) fn measure_mpeg(paths: &[PathBuf], format: &str, infos: &[mpeg::Mp3Info]) -> Result<(Vec<Level>, Level)> {
    album(paths, false, |i, path| {
        let m = if format == "mp3" { mp3_meter(path, &infos[i])? } else { aac_meter(path)? };
        Ok((m.hist, m.peak))
    })
}

pub(super) fn measure(paths: &[PathBuf], format: &str) -> Result<(Vec<Level>, Level)> {
    let flac = format == "flac";
    album(paths, flac, |_, path| {
        let m = if flac { flac_meter(path) } else { ogg_meter(path) }?;
        let peak = f64::from(m.peak as f32 / if flac { 32768. } else { 32767. });
        Ok((m.hist, peak))
    })
}

fn album(paths: &[PathBuf], flac: bool, mut scan: impl FnMut(usize, &Path) -> Result<(Vec<u64>, f64)>) -> Result<(Vec<Level>, Level)> {
    let mut album_hist = vec![0; BINS];
    let mut tracks = Vec::with_capacity(paths.len());
    let mut album_peak: f64 = 0.;
    for (i, path) in paths.iter().enumerate() {
        let (hist, peak) = scan(i, path).with_context(|| format!("measuring {}", path.display()))?;
        tracks.push(Level { gain: gain(&hist, flac), peak });
        album_peak = album_peak.max(peak);
        for (album, track) in album_hist.iter_mut().zip(hist) { *album += track; }
    }
    Ok((tracks, Level { gain: gain(&album_hist, flac), peak: album_peak }))
}
