use split2flac_rs::{cue, layout, source};
use std::fs;

// Literal outcomes from shntool 3.0.10 / cuetools 1.4.0 in the pinned image.
fn sheet(tracks: &[&str]) -> String {
    let mut text = String::from("FILE \"image.wav\" WAVE\n");
    for (i, lines) in tracks.iter().enumerate() {
        text.push_str(&format!(" TRACK {:02} AUDIO\n TITLE \"T{}\"\n{lines}\n", i + 1, i + 1));
    }
    text
}
fn cuts(tracks: &[&str], width: u32, len: Option<u64>) -> anyhow::Result<Vec<(usize, u64, u64)>> {
    Ok(layout::plan(&cue::parse(&sheet(tracks))?, width, len)?.iter()
        .map(|s| (s.number, s.start, s.start_byte)).collect())
}

#[test]
fn gaps_and_later_indexes_do_not_move_split_points() {
    let tracks = ["INDEX 00 00:00:00\n INDEX 01 00:00:05\n POSTGAP 00:00:03",
        "PREGAP 00:00:04\n INDEX 00 00:00:70\n INDEX 01 00:01:01\n INDEX 02 00:01:05"];
    assert_eq!(cuts(&tracks, 4, Some(88217)).unwrap(), [(0,0,0),(1,2940,11760),(2,44688,178752)]);
}

#[test]
fn split_offset_is_in_cd_bytes_even_for_non_cd_pcm() {
    let tracks = ["INDEX 01 00:00:00", "INDEX 01 00:01:01"];
    for (width, sample) in [(4,44688),(6,29792),(2,89376)] {
        assert_eq!(cuts(&tracks, width, None).unwrap()[1], (2,sample,178752));
    }
    // Original WAVs carry partial sample frames; do not round or reject this cut.
    assert_eq!(cuts(&[tracks[0], "INDEX 01 00:00:01"], 10, None).unwrap()[1], (2,235,2352));
}

#[test]
fn eof_drops_only_the_empty_file_but_overrun_rejects_the_entire_split() {
    let tracks = ["INDEX 01 00:00:00", "INDEX 01 00:01:01"];
    assert_eq!(cuts(&tracks,4,Some(44688)).unwrap(), [(1,0,0)]);
    assert!(cuts(&tracks,4,Some(44687)).is_err());
    assert_eq!(cuts(&tracks,4,Some(44689)).unwrap().len(), 2);
}

#[test]
fn single_track_does_not_use_cue_indexes() {
    for line in ["", "INDEX 01 00:00:05", "INDEX 01 01:00:99"] {
        assert_eq!(cuts(&[line],4,Some(88217)).unwrap(), [(1,0,0)]);
    }
}

#[test]
fn cueprint_counts_tracks_but_shnsplit_uses_literal_index_lines() {
    let zero = "INDEX 01 00:00:00";
    let one = "INDEX 01 00:01:00";
    assert_eq!(cuts(&["", one],4,None).unwrap(), [(1,0,0),(2,44100,176400)]);
    assert_eq!(cuts(&[zero,"",one],4,None).unwrap().len(), 2);
    assert!(cuts(&[zero,""],4,None).is_err());
    assert!(cuts(&[zero,one,one],4,None).is_err());
    assert!(cuts(&[zero,one,"INDEX 01 00:00:74"],4,None).is_err());
    assert!(cuts(&[zero,"INDEX 01 00:00:75"],4,None).is_err());
    assert!(cuts(&[zero,"INDEX 01 00:00:10\n INDEX 01 00:00:11"],4,None).is_err());
    assert_eq!(cuts(&["INDEX 01 00:00:00\n INDEX 01 00:00:30",one],4,None).unwrap(),
               [(0,0,0),(1,17640,70560),(2,44100,176400)]);
    let text = sheet(&[zero,one]).replace("02 AUDIO", "02 MODE1/2352");
    assert_eq!(layout::plan(&cue::parse(&text).unwrap(),4,None).unwrap().len(),2);
    let second_file = |idx| format!("FILE \"a\" WAVE\n TRACK 01 AUDIO\n {zero}\n FILE \"b\" WAVE\n TRACK 02 AUDIO\n {idx}\n");
    assert!(cue::parse(&second_file(zero)).is_err());
    assert!(cue::parse(&second_file(one)).is_ok());
}

#[test]
fn external_cue_priority_charset_and_compilation_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("image.flac");
    let suffix = temp.path().join("image.flac.cue");
    let stem = temp.path().join("image.cue");
    let explicit = temp.path().join("chosen.cue");
    fs::write(&suffix,b"suffix").unwrap();
    fs::write(&stem,b"stem").unwrap();
    let text = "REM GENRE Classical\r\nREM DATE 1996-01-01\r\nPERFORMER \"群星\"\r\nTITLE \"专辑音乐经典\"\r\nFILE disc WAVE\r\n TRACK 01 AUDIO\r\n TITLE \"第一首作品\"\r\n PERFORMER \"独奏艺术家\"\r\n INDEX 01 00:00:00\r\n";
    let (bytes, _, errors) = encoding_rs::GBK.encode(text);
    assert!(!errors);
    fs::write(&explicit,bytes.as_ref()).unwrap();
    let load = |p, charset, enca| source::load(&input, p, charset, true, enca);
    for (charset, enca) in [(Some("GBK"),""),(None,"-L zh_CN")] {
        let decoded = load(Some(&explicit),charset,enca).unwrap();
        let s = cue::parse(&decoded).unwrap();
        assert_eq!((&*s.artist,&*s.album,&*s.year,&*s.genre), ("群星","专辑音乐经典","1996","Classical"));
        assert_eq!((&*s.tracks[0].title,&*s.tracks[0].performer), ("第一首作品","独奏艺术家"));
    }
    assert!(load(None,None,"--unsupported").is_err());
    assert_eq!(load(None,None,"").unwrap(),"suffix");
    fs::remove_file(suffix).unwrap();
    assert_eq!(load(None,None,"").unwrap(),"stem");
    fs::write(&stem, [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat()).unwrap();
    assert_eq!(load(None,None,"").unwrap(),text);
    assert!(load(Some(&temp.path().join("missing.cue")),None,"").is_err());
}

#[test]
fn flac_text_comment_and_apev2_text_values_preserve_embedded_line_breaks() {
    let temp = tempfile::tempdir().unwrap();
    let cue = sheet(&["INDEX 01 00:00:00", "INDEX 01 00:01:01"]);
    let mut comments = vec![];
    comments.extend_from_slice(&0u32.to_le_bytes()); // vendor
    comments.extend_from_slice(&1u32.to_le_bytes());
    comments.extend_from_slice(&((cue.len() + 9) as u32).to_le_bytes());
    comments.extend_from_slice(b"CueSheet="); comments.extend_from_slice(cue.as_bytes());
    let mut flac = b"fLaC\x00\x00\x00\x22".to_vec(); flac.extend_from_slice(&[0;34]);
    flac.push(0x84); flac.extend_from_slice(&(comments.len() as u32).to_be_bytes()[1..]);
    flac.extend_from_slice(&comments);
    let path = temp.path().join("music.flac"); fs::write(&path,flac).unwrap();
    assert_eq!(source::load(&path,None,None,true,"").unwrap(),cue);
    for (ext,magic) in [("wv",b"wvpk"),("ape",b"MAC ")] {
        let mut file = magic.to_vec();
        file.extend_from_slice(&(cue.len() as u32).to_le_bytes()); file.extend_from_slice(&[0;4]);
        file.extend_from_slice(b"CUESHEET\0"); file.extend_from_slice(cue.as_bytes());
        let size = (file.len()-4+32) as u32;
        file.extend_from_slice(b"APETAGEX"); file.extend_from_slice(&2000u32.to_le_bytes());
        file.extend_from_slice(&size.to_le_bytes()); file.extend_from_slice(&1u32.to_le_bytes());
        file.extend_from_slice(&[0;12]);
        let path = temp.path().join(format!("music.{ext}")); fs::write(&path,file).unwrap();
        assert_eq!(source::load(&path,None,None,true,"").unwrap(),cue);
    }
}

#[test]
fn utf32_bom_is_not_confused_with_utf16_and_enca_none_rejects_single_byte_text() {
    let root = tempfile::tempdir().unwrap();
    let cue = root.path().join("chosen.cue");
    let input = root.path().join("unused.wav");
    let text = "TITLE \"音楽\"\r\n";
    let mut bytes = vec![0xff, 0xfe, 0, 0];
    bytes.extend(text.chars().flat_map(|c| (c as u32).to_le_bytes()));
    fs::write(&cue, &bytes).unwrap();
    assert_eq!(source::load(&input, Some(&cue), None, true, "").unwrap(), text);
    fs::write(&cue, b"TITLE \"Caf\xe9\"\n").unwrap();
    assert!(source::load(&input, Some(&cue), None, true, "-L none").is_err());
    assert_eq!(source::load(&input, Some(&cue), Some("windows-1252"), true, "-L none").unwrap(), "TITLE \"Café\"\n");
}
