use split2flac::{media, split, tools};
use std::{fs, path::{Path, PathBuf}, process::Command};
use tempfile::tempdir;

fn tool(name: &str) -> Command {
    let mut c = Command::new(tools::binary(name));
    c.args(["-v", "error"]);
    c
}

fn checked(c: &mut Command) -> Vec<u8> {
    let out = c.output().unwrap();
    assert!(out.status.success(), "{c:?}: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

// tests/fixtures/ramp.ape was made from this PCM using Monkey's Audio 4.11
// (`mac input.wav ramp.ape -c2000` in the locked reference image). No music samples.
fn ramp() -> Vec<u8> {
    let mut bytes = Vec::new();
    for i in 0..16631 {
        bytes.extend_from_slice(&(((i * 13) % 17011 - 8505) as i16).to_le_bytes());
        bytes.extend_from_slice(&(if i % 139 < 69 { 2300i16 } else { -2301 }).to_le_bytes());
    }
    bytes
}

fn raw_input(path: &Path, fmt: &str, rate: &str) -> Command {
    let mut c = tool("ffmpeg");
    c.args(["-f", fmt, "-ar", rate, "-ac", "2", "-i"]).arg(path);
    c
}

fn decoded(path: &Path, fmt: &str) -> Vec<u8> {
    checked(tool("ffmpeg").arg("-i").arg(path).args(["-map", "0:a:0", "-f", fmt, "pipe:1"]))
}

fn track(path: PathBuf, start: u64, number: usize) -> split::Output {
    split::Output { path, start, start_byte: start * 4, number, tags: vec![("title".into(), format!("曲目 {number}"))] }
}

fn inspect(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&checked(tool("ffprobe").args(["-show_streams", "-show_format", "-of", "json"]).arg(path))).unwrap()
}

#[test]
fn five_real_lossless_inputs_preserve_every_frame_at_an_unaligned_cut() {
    let d = tempdir().unwrap();
    let raw = d.path().join("reference.pcm");
    let bytes = ramp();
    fs::write(&raw, &bytes).unwrap();
    for (ext, codec) in [("wav", "pcm_s16le"), ("flac", "flac"), ("wv", "wavpack"), ("tta", "tta"), ("ape", "ape")] {
        let input = d.path().join(format!("源 image.{ext}"));
        if ext == "ape" { fs::copy("tests/fixtures/ramp.ape", &input).unwrap(); }
        else { checked(raw_input(&raw, "s16le", "44100").args(["-c:a", codec]).arg(&input)); }
        let pcm = media::probe(&input).unwrap();
        assert_eq!((pcm.rate, pcm.channels, pcm.bits, pcm.frame_bytes), (44100, 2, 16, 4), "{ext}");
        for output in ["wav", "flac"] {
            let tracks = [track(d.path().join(format!("{ext}-0.{output}")), 0, 0), track(d.path().join(format!("{ext}-1.{output}")), 7057, 1)];
            split::run(&input, &pcm, &tracks, output, &[], None).unwrap();
            assert_eq!(decoded(&tracks[0].path, "s16le"), &bytes[..7057*4]);
            assert_eq!(decoded(&tracks[1].path, "s16le"), &bytes[7057*4..]);
        }
    }
}

#[test]
fn lossy_tracks_match_independently_encoded_pcm_slices_and_only_tag_named_tracks() {
    let d = tempdir().unwrap();
    let input = Path::new("tests/fixtures/ramp.ape");
    let pcm = media::probe(input).unwrap();
    let bytes = ramp();
    for (ext, codec) in [("mp3", "libmp3lame"), ("m4a", "aac"), ("opus", "libopus"), ("ogg", "libvorbis")] {
        let tracks = [track(d.path().join(format!("0.{ext}")), 0, 0), track(d.path().join(format!("1.{ext}")), 7057, 1)];
        split::run(input, &pcm, &tracks, ext, &[], None).unwrap();
        for (idx, (output, expect)) in tracks.iter().zip([&bytes[..7057*4], &bytes[7057*4..]]).enumerate() {
            let raw = d.path().join("slice.pcm");
            fs::write(&raw, expect).unwrap();
            let reference = d.path().join(format!("reference-{idx}.{ext}"));
            checked(raw_input(&raw, "s16le", "44100").args(["-c:a", codec]).arg(&reference));
            let actual = decoded(&output.path, "s16le");
            assert!(!actual.is_empty());
            assert_eq!(actual, decoded(&reference, "s16le"), "{ext} track {idx} boundary/priming");
            let data = inspect(&output.path);
            let title = [ &data["format"]["tags"], &data["streams"][0]["tags"] ].iter().filter_map(|tags| tags.as_object()).flat_map(|tags| tags.iter()).find(|(k,_)| k.eq_ignore_ascii_case("title")).map(|(_, v)| v.as_str().unwrap());
            assert_eq!(title, (idx == 1).then_some("曲目 1"));
        }
    }
}

#[test]
fn lossless_24_and_32_bit_keep_low_bits_and_non_cd_rate() {
    let d = tempdir().unwrap();
    for (bits, fmt) in [(24, "s24le"), (32, "s32le")] {
        let bytes: Vec<u8> = (0..50003 * 2).flat_map(|i| {
            let x = (i as i32).wrapping_mul(1706543).wrapping_add(773);
            x.to_le_bytes().into_iter().take(bits / 8)
        }).collect();
        let raw = d.path().join("high.pcm");
        fs::write(&raw, &bytes).unwrap();
        let input = d.path().join(format!("{bits}.wav"));
        checked(raw_input(&raw, fmt, "96000").args(["-c:a", &format!("pcm_{fmt}")]).arg(&input));
        let pcm = media::probe(&input).unwrap();
        assert_eq!(pcm.total_frames, Some(50003));
        for ext in ["wav", "flac"] {
            let tracks = [track(d.path().join(format!("{bits}-out.{ext}")), 0, 1)];
            split::run(&input, &pcm, &tracks, ext, &[], None).unwrap();
            let out = media::probe(&tracks[0].path).unwrap();
            assert_eq!((out.bits as usize, out.rate, out.channels), (bits, 96000, 2));
            assert_eq!(decoded(&tracks[0].path, fmt), bytes);
        }
    }
}

#[test]
fn cover_scales_with_aspect_ratio_and_embeds_only_in_named_flac_and_m4a() {
    let d = tempdir().unwrap();
    let ppm = d.path().join("front.ppm");
    let mut image = b"P6\n16 8\n255\n".to_vec();
    image.extend([228, 43, 114].repeat(16*8));
    fs::write(&ppm, image).unwrap();
    let input = Path::new("tests/fixtures/ramp.ape");
    let cover = d.path().join("cover.jpg");
    assert!(split::cover(input, Some(&ppm), "10x10", &cover).unwrap());
    let pcm = media::probe(input).unwrap();
    for format in ["flac", "m4a", "mp3", "wav"] {
        let tracks = [track(d.path().join(format!("0.{format}")), 0, 0), track(d.path().join(format!("1.{format}")), 7057, 1)];
        split::run(input, &pcm, &tracks, format, &[], Some(&cover)).unwrap();
        for t in &tracks {
            let data = inspect(&t.path);
            let picture = data["streams"].as_array().unwrap().iter().find(|s| s["disposition"]["attached_pic"] == 1);
            if t.number == 1 && matches!(format, "flac" | "m4a") {
                let s = picture.unwrap();
                assert_eq!((s["width"].as_u64(), s["height"].as_u64()), (Some(10), Some(5)));
                assert_eq!(s["codec_name"], "mjpeg");
            } else { assert!(picture.is_none()); }
        }
        if format == "flac" {
            let embedded = d.path().join("extracted.jpg");
            assert!(split::cover(&tracks[1].path, None, "20x20", &embedded).unwrap());
            assert_eq!(inspect(&embedded)["streams"][0]["width"], 20);
            assert!(!split::cover(&tracks[0].path, None, "10x10", &embedded).unwrap());
        }
    }
}

#[test]
fn partial_or_corrupt_inputs_and_encoder_failures_never_publish_unfinished_tracks() {
    let d = tempdir().unwrap();
    let input = d.path().join("input.wav");
    let raw = d.path().join("input.pcm");
    fs::write(&raw, ramp()).unwrap();
    checked(raw_input(&raw, "s16le", "44100").arg(&input));
    let pcm = media::probe(&input).unwrap();
    let tracks = [track(d.path().join("early.wav"), 0, 1), track(d.path().join("empty.wav"), 16631, 2)];
    assert!(split::run(&input, &pcm, &tracks, "wav", &[], None).is_err());
    assert_eq!(decoded(&tracks[0].path, "s16le"), ramp());
    assert!(!tracks[1].path.exists());
    let t = [track(d.path().join("bad.flac"), 0, 1)];
    assert!(split::run(&input, &pcm, &t, "flac", &["-lpc_type".into(), "unavailable".into()], None).is_err());
    assert!(!t[0].path.exists());
    let file = fs::OpenOptions::new().write(true).open(&input).unwrap();
    file.set_len(file.metadata().unwrap().len()-12).unwrap();
    drop(file);
    assert!(media::probe(&input).is_err(), "RIFF data length exceeds physical length");
    assert!(split::run(&input, &pcm, &t, "flac", &[], None).is_err());
    assert!(!t[0].path.exists());
    assert!(input.exists());
}

#[test]
fn unknown_length_flac_reports_only_clean_final_boundary_as_recoverable() {
    let d = tempdir().unwrap();
    let input = d.path().join("stream.flac");
    checked(tool("ffmpeg").arg("-i").arg("tests/fixtures/ramp.ape").arg(&input));
    let mut flac = fs::read(&input).unwrap();
    // Zero denotes an unknown sample count in the 36-bit STREAMINFO field.
    flac[21] &= 0xf0;
    flac[22..26].fill(0);
    fs::write(&input,&flac).unwrap();
    let pcm = media::probe(&input).unwrap();
    assert_eq!(pcm.total_frames, None);
    let output = |name, cut| [track(d.path().join(format!("{name}-first.wav")),0,1),
                              track(d.path().join(format!("{name}-last.wav")),cut,2)];
    let exact = output("exact",16631);
    let err = split::run(&input,&pcm,&exact,"wav",&[],None).unwrap_err();
    assert!(err.is::<split::EmptyFinalSegment>(), "{err:#}");
    assert_eq!(decoded(&exact[0].path,"s16le"),ramp());
    assert!(!exact[1].path.exists());
    let beyond = output("beyond",16632);
    let err = split::run(&input,&pcm,&beyond,"wav",&[],None).unwrap_err();
    assert!(!err.is::<split::EmptyFinalSegment>());
    assert!(!beyond[0].path.exists());
    // A complete earlier segment does not make an interrupted decode recoverable.
    flac.truncate(flac.len()-11);
    fs::write(&input,&flac).unwrap();
    let damaged = output("damaged",588);
    let err = split::run(&input,&pcm,&damaged,"wav",&[],None).unwrap_err();
    assert!(!err.is::<split::EmptyFinalSegment>());
    assert!(!damaged[1].path.exists());
}

#[test]
fn encoder_tuning_cannot_create_other_files_change_boundaries_or_override_the_codec() {
    let d = tempdir().unwrap();
    let path = Path::new("tests/fixtures/ramp.ape");
    let pcm = media::probe(path).unwrap();
    let side = d.path().join("side.flac");
    let t = [track(d.path().join("protected.flac"), 0, 1)];
    for key in ["-i", "-passlogfile", "-filter_complex_script", "-map", "-af", "-f", "-ar", "-c:a", "-sample_fmt", "-frames:a", "-t", "-/metadata"] {
        assert!(split::run(path, &pcm, &t, "flac", &[key.into(), side.to_str().unwrap().into()], None).is_err());
        assert!(!t[0].path.exists());
        assert!(!side.exists());
    }
    fs::write(&t[0].path, b"mine").unwrap();
    assert!(split::run(path, &pcm, &t, "flac", &[], None).is_err());
    assert_eq!(fs::read(&t[0].path).unwrap(), b"mine");
}

#[test]
fn legacy_wave_split_preserves_both_halves_of_a_straddled_multichannel_frame() {
    let d = tempdir().unwrap();
    let raw = d.path().join("multichannel.pcm");
    let bytes = ramp().into_iter().take(49000).collect::<Vec<_>>();
    fs::write(&raw, &bytes).unwrap();
    let input = d.path().join("5ch.wav");
    checked(tool("ffmpeg").args(["-f", "s16le", "-ar", "44100", "-ac", "5", "-i"]).arg(&raw).arg(&input));
    let pcm = media::probe(&input).unwrap();
    let mut tracks = [track(d.path().join("01.wav"), 0, 1), track(d.path().join("02.wav"), 235, 2)];
    tracks[1].start_byte = 2352;
    split::run(&input, &pcm, &tracks, "wav", &[], None).unwrap();
    for (out, expected) in tracks.iter().zip([&bytes[..2352], &bytes[2352..]]) {
        let file = fs::read(&out.path).unwrap();
        let data = file.windows(4).position(|s| s == b"data").unwrap();
        let size = u32::from_le_bytes(file[data+4..data+8].try_into().unwrap()) as usize;
        assert_eq!(size, expected.len());
        assert_eq!(&file[data+8..data+8+size], expected);
    }
    for t in &mut tracks { t.path.set_extension("flac"); }
    split::run(&input, &pcm, &tracks, "flac", &[], None).unwrap();
    // Original FLAC encoder drops only the incomplete *suffix of each slice*;
    // aligning the second start to a frame instead corrupts all its channels.
    assert_eq!(decoded(&tracks[0].path, "s16le"), &bytes[..2350]);
    assert_eq!(decoded(&tracks[1].path, "s16le"), &bytes[2352..49000-8]);
}

#[test]
fn extensible_wave_with_24_valid_bits_in_32_bit_words_is_not_adobe_float_pcm() {
    let d = tempdir().unwrap();
    let input = d.path().join("pcm.wav");
    let samples: Vec<u8> = (0i32..4096).flat_map(|i| ((i.wrapping_mul(17011)-8000)<<8).to_le_bytes()).collect();
    let mut wave = Vec::new();
    wave.extend(b"RIFF");
    wave.extend((60 + samples.len() as u32).to_le_bytes());
    wave.extend(b"WAVEfmt ");
    wave.extend(40u32.to_le_bytes());
    wave.extend(0xfffeu16.to_le_bytes());
    wave.extend(2u16.to_le_bytes());
    wave.extend(96000u32.to_le_bytes());
    wave.extend((96000u32*8).to_le_bytes());
    for n in [8u16,32,22,24] { wave.extend(n.to_le_bytes()); }
    wave.extend(3u32.to_le_bytes());
    wave.extend([1,0,0,0,0,0,16,0,128,0,0,170,0,56,155,113]);
    wave.extend(b"data");
    wave.extend((samples.len() as u32).to_le_bytes());
    wave.extend(&samples);
    fs::write(&input, wave).unwrap();
    let pcm = media::probe(&input).unwrap();
    assert_eq!((pcm.bits,pcm.frame_bytes,pcm.total_frames), (24,8,Some(2048)));
    for format in ["wav","flac"] {
        let out = [track(d.path().join(format!("24in32.{format}")), 0, 1)];
        split::run(&input,&pcm,&out,format,&[],None).unwrap();
        assert_eq!(media::probe(&out[0].path).unwrap().bits, 24);
        let mut cmd = tool("ffmpeg");
        if format == "wav" { cmd.args(["-c:a","pcm_s32le"]); }
        assert_eq!(checked(cmd.arg("-i").arg(&out[0].path).args(["-f","s32le","pipe:1"])), samples);
    }
}

#[test]
fn rf64_float_samples_remain_float_in_wave_and_never_silently_become_integer_flac() {
    let d = tempdir().unwrap();
    let samples: Vec<u8> = (0..4001).flat_map(|i| ((i as f32 - 1700.5) / 900.0).to_le_bytes()).collect();
    let raw = d.path().join("float.pcm");
    fs::write(&raw, &samples).unwrap();
    let input = d.path().join("wide.wav");
    checked(tool("ffmpeg").args(["-f","f32le","-ar","48000","-ac","1","-i"]).arg(&raw)
        .args(["-c:a","pcm_f32le","-rf64","always"]).arg(&input));
    let pcm = media::probe(&input).unwrap();
    assert_eq!((pcm.bits, pcm.channels, pcm.total_frames), (32,1,Some(4001)));
    let out = [track(d.path().join("float.wav"),0,1)];
    split::run(&input,&pcm,&out,"wav",&[],None).unwrap();
    assert_eq!(decoded(&out[0].path, "f32le"), samples);
    let out = [track(d.path().join("float.flac"),0,1)];
    assert!(split::run(&input,&pcm,&out,"flac",&[],None).is_err());
    assert!(!out[0].path.exists());
}
