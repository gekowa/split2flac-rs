use std::{collections::BTreeMap, fs, path::{Path, PathBuf}, process::Command};

use split2flac::{gain, tools::binary};
use tempfile::tempdir;

// Decoded audio, not a file checksum: changing a comment necessarily changes the container.
fn decoded(path: &Path) -> Vec<u8> {
    let out = Command::new(binary("ffmpeg")).args(["-v", "error", "-i"]).arg(path)
        .args(["-map", "0:a:0", "-f", "f32le", "-"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

fn track(dir: &Path, format: &str, amp: i16) -> PathBuf {
    let path = dir.join(format!("音轨 {amp}.{format}"));
    if format != "flac" {
        // Frozen inputs, not recordings: the PCM formula below was encoded by FFmpeg
        // 9.0.2 (default AAC and libmp3lame) or oggenc 1.4.0 (default quality).
        // New encoder versions may produce different loudness; the oracle must
        // always analyze the same packets as this test.
        let name = if amp == 400 { "q" } else { "l" };
        fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/gain/{name}.{format}")), &path).unwrap();
        return path;
    }
    let raw = dir.join("tone.s16");
    let pcm = (0..44100).flat_map(|i| {
        let s = (f64::from(amp) * (std::f64::consts::TAU * f64::from(i % 100) / 100.).sin()).round() as i16;
        let [lo, hi] = s.to_le_bytes();
        [lo, hi, lo, hi]
    }).collect::<Vec<_>>();
    fs::write(&raw, pcm).unwrap();
    let mut cmd = Command::new(binary("ffmpeg"));
    cmd.args(["-v", "error", "-f", "s16le", "-ar", "44100", "-ac", "2", "-i"]).arg(&raw)
        .args(["-metadata", "title=preserve me"]);
    let out = cmd.arg(&path).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    path
}

// ffprobe does not expose APE tags on MP3 with a leading ID3v2 tag; read the footer
// independently (mp3rgain is the writer). For the other containers use ffprobe.
fn tags(path: &Path, format: &str) -> BTreeMap<String, String> {
    if format == "mp3" {
        let file = fs::read(path).unwrap();
        let footer = &file[file.len()-32..];
        assert_eq!(&footer[..8], b"APETAGEX");
        let le = |s: &[u8]| u32::from_le_bytes(s.try_into().unwrap()) as usize;
        let mut data = &file[file.len()-le(&footer[12..16])..];
        let mut tags = BTreeMap::new();
        for _ in 0..le(&footer[16..20]) {
            let n = le(&data[..4]);
            let end = 8 + data[8..].iter().position(|b| *b == 0).unwrap();
            tags.insert(String::from_utf8(data[8..end].into()).unwrap(),
                        String::from_utf8(data[end+1..end+1+n].into()).unwrap());
            data = &data[end+1+n..];
        }
        return tags;
    }
    let out = Command::new(binary("ffprobe")).args(["-v", "error", "-show_entries", "stream_tags:format_tags", "-of", "json"]).arg(path).output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let t = if format == "ogg" { &v["streams"][0]["tags"] } else { &v["format"]["tags"] };
    assert_eq!(t["title"], "preserve me");
    t.as_object().unwrap().iter().map(|(k,v)| (k.clone(), v.as_str().unwrap().to_owned())).collect()
}

#[test]
fn two_track_album_and_independent_singleton() {
    let tmp = tempdir().unwrap();
    for f in ["flac", "ogg", "mp3", "m4a"] {
        let q = track(tmp.path(), f, 400);
        let l = track(tmp.path(), f, 4000);
        let solo = tmp.path().join(format!("solo.{f}"));
        fs::copy(&q, &solo).unwrap();
        let before = [decoded(&q), decoded(&l)];
        gain::apply(f, &[q.clone(), l.clone()]).unwrap();
        gain::apply(f, std::slice::from_ref(&solo)).unwrap();
        assert!(decoded(&q) == before[0] && decoded(&solo) == before[0] && decoded(&l) == before[1], "audio changed: {f}");
        let [quiet, loud, single] = [&q, &l, &solo].map(|p| tags(p, f));
        let key = |k: &str| if f == "m4a" { k.to_ascii_lowercase() } else { k.to_owned() };
        let tg = key("REPLAYGAIN_TRACK_GAIN"); let ag = key("REPLAYGAIN_ALBUM_GAIN");
        let tp = key("REPLAYGAIN_TRACK_PEAK"); let ap = key("REPLAYGAIN_ALBUM_PEAK");
        assert_eq!(quiet[&ag], loud[&ag]); assert_eq!(quiet[&ap], loud[&tp]);
        assert_eq!(quiet[&tp], single[&tp]); assert_eq!(quiet[&tg], single[&tg]);
        // Native reference programs from mikenye/split2flac@sha256:47e6cb8d835a05f08efae20f7bf196552754df09e9dc957cc473b870f4dbc8ba.
        // These are independently measured on these exact compressed audio packets.
        let (q_ref, l_ref, a_ref) = match f {
            "flac" => ("+23.45 dB", "+3.45 dB", "+3.45 dB"),
            "ogg" => ("+23.32 dB", "+3.37 dB", "+3.39 dB"),
            "mp3" => ("+23.89000 dB", "+3.890000 dB", "+3.890000 dB"),
            "m4a" => ("23.46", "3.42", "3.43"),
            _ => unreachable!(),
        };
        assert_eq!((&*quiet[&tg], &*loud[&tg], &*quiet[&ag]), (q_ref,l_ref,a_ref), "{f}");
        let peaks = (&*quiet[&tp], &*loud[&tp]);
        match f {
            "flac" => assert_eq!(peaks, ("0.01220703", "0.12207031")),
            "mp3" => assert_eq!(peaks, ("0.011614", "0.116037")),
            "m4a" => assert_eq!(peaks, ("0.01", "0.12")),
            "ogg" => { // Platform-specific Vorbis SIMD rounding in the final float.
                for (value, expected) in [(peaks.0,0.01275655),(peaks.1,0.12736325)] {
                    assert!((value.parse::<f64>().unwrap() - expected).abs() <= 0.00000005);
                }
            }
            _ => unreachable!(),
        }
        if f == "flac" {
            assert_eq!(quiet["REPLAYGAIN_REFERENCE_LOUDNESS"], "89.0 dB");
        }
        if f == "flac" || f == "ogg" {
            assert_eq!(single[&ag], single[&tg]); assert_eq!(single[&ap], single[&tp]);
            assert_eq!(quiet[&tp].split_once('.').unwrap().1.len(), 8);
            assert!(quiet[&tg].starts_with('+') && quiet[&tg].ends_with(" dB"));
        } else {
            assert!(!single.contains_key(&ag) && !single.contains_key(&ap));
            let (mm, am) = if f == "mp3" { ("MP3GAIN_MINMAX", "MP3GAIN_ALBUM_MINMAX") } else { ("replaygain_track_minmax", "replaygain_album_minmax") };
            assert!(!single.contains_key(am));
            let want = if f == "mp3" { ("113,210", "126,210", "113,210") } else { ("103,147", "104,153", "103,153") };
            assert_eq!((&*quiet[mm], &*loud[mm], &*quiet[am]), want); assert_eq!(quiet[am], loud[am]);
            assert_eq!(quiet[&tp].split_once('.').unwrap().1.len(), if f == "mp3" { 6 } else { 2 });
            if f == "mp3" { assert_eq!(quiet[&tg].len(), 12); assert!(quiet[&tg].ends_with(" dB")); }
            else { assert!(!quiet[&tg].starts_with('+')); assert_eq!(quiet[&tg].split_once('.').unwrap().1.len(), 2); }
        }
    }
}

#[test]
fn wav_and_opus_are_not_opened_for_gain() {
    for format in ["wav", "opus"] { gain::apply(format, &[PathBuf::from("nonexistent")]).unwrap(); }
}
