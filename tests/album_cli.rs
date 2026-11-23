use std::{fs, path::Path, process::Command};
use tempfile::tempdir;

fn fixture(root: &Path, titles: &[&str]) {
    let mut wav = Vec::from(&b"RIFF"[..]);
    let data_len = 44100u32 * 4;
    wav.extend((data_len + 36).to_le_bytes()); wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes()); wav.extend(1u16.to_le_bytes()); wav.extend(2u16.to_le_bytes());
    wav.extend(44100u32.to_le_bytes()); wav.extend((44100u32 * 4).to_le_bytes());
    wav.extend(4u16.to_le_bytes()); wav.extend(16u16.to_le_bytes()); wav.extend(b"data");
    wav.extend(data_len.to_le_bytes()); wav.resize(wav.len() + data_len as usize, 0);
    fs::write(root.join("盘.wav"), wav).unwrap();
    let mut cue = String::from("PERFORMER \"A\"\nTITLE \"B\"\nFILE \"盘.wav\" WAVE\n");
    for (i, title) in titles.iter().enumerate() {
        cue.push_str(&format!(" TRACK {:02} AUDIO\n TITLE \"{title}\"\n INDEX 01 00:00:{:02}\n", i+1, i*10));
    }
    fs::write(root.join("盘.cue"), cue).unwrap();
}
fn run(root:&Path, args:&[&str])->std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_split2flac")).current_dir(root).env("HOME",root).env("USERPROFILE",root).env("XDG_CONFIG_HOME",root).env("APPDATA",root).args(args).output().unwrap()
}
#[test]
fn duplicate_track_names_fail_before_first_output_or_deletion() {
    let tmp=tempdir().unwrap(); let dir=tmp.path(); fixture(dir,&["same","same"]);
    let result=run(dir,&["-o","out","-f","wav","-of","@title.@ext","-D","-F","盘.wav"]);
    assert_eq!(result.status.code(),Some(3),"{:?}",result);
    assert!(dir.join("盘.wav").exists()); assert!(!dir.join("out/same.wav").exists());
}
#[test]
fn dry_run_does_not_write_or_remove_music() {
    let tmp=tempdir().unwrap(); let dir=tmp.path(); fixture(dir,&["One","Two"]);
    let result=run(dir,&["-p","-o","out","-f","wav","-D","-F","盘.wav"]);
    assert!(result.status.success(),"{:?}",result);
    assert!(String::from_utf8_lossy(&result.stdout).contains("Two")); assert!(dir.join("盘.wav").exists()); assert!(!dir.join("out").exists());
}
#[test]
fn batch_continues_after_bad_album_and_preserves_successful_output() {
    let tmp=tempdir().unwrap(); let dir=tmp.path(); let albums=dir.join("music"); fs::create_dir(&albums).unwrap(); fixture(&albums,&["One","Two"]); fs::write(albums.join("bad.wav"),b"not audio").unwrap();
    let result=run(dir,&["-nask","-nC","-nc","-o","out","-f","wav","music"]);
    assert_eq!(result.status.code(),Some(3),"{:?}",result); assert!(dir.join("out/A/B/02 - Two.wav").is_file());
    let failed=fs::read_to_string(dir.join("split_failed.txt")).unwrap(); assert!(failed.contains("bad.wav")); assert!(!failed.contains("盘.wav"));
}

#[test]
fn force_deletes_only_after_extras_are_copied() {
    let tmp=tempdir().unwrap(); let dir=tmp.path(); fixture(dir,&["One","Two"]);
    fs::write(dir.join("rip.log"), b"verified tracks\n").unwrap();
    let result=run(dir,&["-D","-F","-f","wav","-C","*.log","-o","out","盘.wav"]);
    assert!(result.status.success(),"{result:?}");
    assert_eq!(fs::read(dir.join("out/A/B/rip.log")).unwrap(), b"verified tracks\n");
    assert!(!dir.join("盘.wav").exists());
}

#[cfg(unix)]
#[test]
fn a_symlinked_album_directory_cannot_escape_the_output_root() {
    let tmp=tempdir().unwrap(); let dir=tmp.path(); fixture(dir,&["One","Two"]);
    let outside=dir.join("outside");
    fs::create_dir(&outside).unwrap(); fs::create_dir(dir.join("out")).unwrap();
    std::os::unix::fs::symlink(&outside,dir.join("out/A")).unwrap();
    let result=run(dir,&["-f","wav","-D","-F","-o","out","盘.wav"]);
    assert_eq!(result.status.code(),Some(3),"{result:?}");
    assert!(dir.join("盘.wav").exists());
    assert_eq!(outside.read_dir().unwrap().count(),0);
}
