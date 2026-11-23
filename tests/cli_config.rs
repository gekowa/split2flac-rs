use std::{fs, path::Path, process::{Command, Output}};

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_split2flac-rs"))
        .current_dir(home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .args(args)
        .output().unwrap()
}

#[test]
fn existing_xdg_configuration_wins_and_save_never_modifies_the_home_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let fallback = home.join(".split2flac");
    fs::write(&fallback, "DIR='fallback library'\n").unwrap();
    let xdg = home.join("xdg/split2flac/split2flac.conf");
    fs::create_dir_all(xdg.parent().unwrap()).unwrap();
    fs::write(&xdg, "DIR='old XDG library'\n").unwrap();
    let help = run(home, &["-h"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("old XDG library"));

    // As in v122, saving is performed even without an image, but a missing
    // image does not count as a successful split.
    let save = run(home, &["-o", "新 library", "-s"]);
    assert_eq!(save.status.code(), Some(1));
    assert_eq!(fs::read_to_string(&fallback).unwrap(), "DIR='fallback library'\n");
    assert!(String::from_utf8_lossy(&run(home, &["--help"]).stdout).contains("新 library"));
    fs::remove_file(xdg).unwrap();
    assert!(String::from_utf8_lossy(&run(home, &["-help"]).stdout).contains("fallback library"));
}

#[test]
fn short_and_long_version_report_the_selected_format_without_an_image() {
    let temp = tempfile::tempdir().unwrap();
    for flag in ["-v", "--version"] {
        let version = run(temp.path(), &["-f", "wav", flag]);
        assert!(version.status.success());
        assert!(String::from_utf8_lossy(&version.stdout).contains("split2wav version: 122"));
    }
}
