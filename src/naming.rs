use crate::{cli::Options, cue::Sheet};
use anyhow::{ensure, Result};
use std::borrow::Cow;
use std::path::PathBuf;

// This is lexical containment. The caller also checks the actual filesystem
// (including symlinks, volume name equivalence and existing outputs).
fn component(name: &str, windows: bool) -> Result<Cow<'_, str>> {
    ensure!(!matches!(name, "" | "." | ".."), "unsafe template path component {name:?}");
    ensure!(!name.contains(['/', '\0']), "invalid character in output filename {name:?}");
    if !windows {
        return Ok(Cow::Borrowed(name));
    }
    let illegal = |c: char| c <= '\u{1f}' || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*');
    let end = name.trim_end_matches(['.', ' ']).len();
    let stem = name.split('.').next().unwrap_or("").trim_end_matches(' ');
    let device = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
        .iter().any(|s| stem.eq_ignore_ascii_case(s))
        || stem.get(..3).is_some_and(|s| s.eq_ignore_ascii_case("COM") || s.eq_ignore_ascii_case("LPT"))
            && matches!(stem.get(3..), Some("1"|"2"|"3"|"4"|"5"|"6"|"7"|"8"|"9"|"¹"|"²"|"³"));
    if !device && end == name.len() && !name.chars().any(illegal) {
        return Ok(Cow::Borrowed(name));
    }
    let mut out = String::with_capacity(name.len() + usize::from(device));
    if device {
        out.push('_');
    }
    for (i, c) in name.char_indices() {
        out.push(if i >= end || illegal(c) { '_' } else { c });
    }
    Ok(Cow::Owned(out))
}

struct Rendered {
    text: String,
    // Byte offset of the first track-dependent substitution. Everything up
    // through the separator before it is stable across the entire album.
    first_track: Option<usize>,
}

impl Rendered {
    fn literal(&mut self, text: &str) {
        for ch in text.chars() {
            self.text.push(match ch { '\\' => '/', '\0' => '_', _ => ch });
        }
    }

    fn fragment(&mut self, mut text: &str, fields: &[(&str, &str, bool)]) -> (bool, bool) {
        let (mut found, mut complete) = (false, true);
        while let Some((before, rest)) = text.split_once('@') {
            self.literal(before);
            let token = rest.strip_prefix('_').unwrap_or(rest);
            if let Some(&(key, value, track)) = fields.iter().find(|(key, _, _)| token.starts_with(*key)) {
                found = true;
                complete &= !value.is_empty();
                if track {
                    self.first_track.get_or_insert(self.text.len());
                }
                let underscored = token.len() != rest.len();
                for ch in value.chars() {
                    // A substituted value is data, never a template token or
                    // a path separator. Preserve other legal Unicode verbatim.
                    let replace = matches!(ch, '/' | '\0') || cfg!(windows) && ch == '\\' || underscored && ch == ' ';
                    self.text.push(if replace { '_' } else { ch });
                }
                text = &token[key.len()..];
            } else {
                self.text.push('@');
                text = rest;
            }
        }
        self.literal(text);
        (found, complete)
    }
}

fn render(opt: &Options, sheet: &Sheet, number: Option<usize>) -> Result<Rendered> {
    let pattern = &opt.pattern;
    let bytes = pattern.as_bytes();
    ensure!(!pattern.starts_with(['/', '\\'])
        && !(bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'),
        "output template must be relative to the output directory: {pattern:?}");
    let track = number.and_then(|n| sheet.tracks.get(n - 1));
    let serial = number.map_or_else(|| "_".into(), |n| format!("{n:02}"));
    let fields = [
        ("artist", sheet.artist.as_str(), false),
        ("album", sheet.album.as_str(), false),
        ("year", sheet.year.as_str(), false),
        ("genre", sheet.genre.as_str(), false),
        ("ext", opt.format.as_str(), false),
        ("track", serial.as_str(), true),
        ("performer", track.map_or("_", |t| if t.performer.is_empty() { &sheet.artist } else { &t.performer }), true),
        ("title", track.map_or("_", |t| t.title.as_str()), true),
    ];
    let mut out = Rendered { text: String::with_capacity(pattern.len()), first_track: None };
    let mut tail = pattern.as_str();
    while let Some((before, open)) = tail.split_once('{') {
        let Some((group, after)) = open.split_once('}') else { break };
        ensure!(!group.contains('{'), "nested optional template groups are not supported");
        out.fragment(before, &fields);
        let start = out.text.len();
        let first_track = out.first_track;
        let (found, complete) = out.fragment(group, &fields);
        if !found || !complete {
            out.text.truncate(start);
            out.first_track = first_track;
            if !found {
                out.text.push('{');
                out.literal(group);
                out.text.push('}');
            }
        }
        tail = after;
    }
    out.fragment(tail, &fields);
    Ok(out)
}

fn push_dirs(out: &mut PathBuf, dirs: &str) -> Result<()> {
    for dir in dirs.split('/').filter(|s| !s.is_empty() && *s != ".") {
        out.push(component(dir, cfg!(windows))?.as_ref());
    }
    Ok(())
}

pub fn album_dir(opt: &Options, sheet: &Sheet) -> Result<PathBuf> {
    let mut out = opt.out.clone();
    if opt.subdirs {
        let rendered = render(opt, sheet, None)?;
        let prefix = &rendered.text[..rendered.first_track.unwrap_or(rendered.text.len())];
        if let Some((dirs, _)) = prefix.rsplit_once('/') {
            push_dirs(&mut out, dirs)?;
        }
    }
    Ok(out)
}

pub fn destination(opt: &Options, sheet: &Sheet, number: usize) -> Result<PathBuf> {
    if number == 0 || number > sheet.tracks.len() {
        let name = format!("{number:02}.{}", opt.format);
        return Ok(album_dir(opt, sheet)?.join(component(&name, cfg!(windows))?.as_ref()));
    }
    let rendered = render(opt, sheet, Some(number))?;
    let (dirs, file) = rendered.text.rsplit_once('/').unwrap_or(("", &rendered.text));
    let mut out = opt.out.clone();
    if opt.subdirs {
        push_dirs(&mut out, dirs)?;
    }
    out.push(component(file, cfg!(windows))?.as_ref());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use super::*;

    fn album() -> Sheet {
        crate::cue::parse("REM DATE 1998\nPERFORMER \"Various Artists\"\nTITLE \"合集\"\nFILE \"image.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"A & B\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    PERFORMER \"Other Singer\"\n    TITLE \"C/D\"\n    INDEX 01 00:01:00\n").unwrap()
    }

    #[test]
    fn templates_preserve_unicode_fallback_performer_and_substitute_only_data() {
        let o = Options { out: "out".into(), pattern: "@_artist/{@year - }@album/{@genre - }@track - @_performer - @title.@ext".into(), ..Options::default() };
        let mut sheet = album();
        assert_eq!(destination(&o, &sheet, 1).unwrap(), Path::new("out/Various_Artists/1998 - 合集/01 - Various_Artists - A & B.flac"));
        assert_eq!(destination(&o, &sheet, 2).unwrap(), Path::new("out/Various_Artists/1998 - 合集/02 - Other_Singer - C_D.flac"));
        assert_eq!(destination(&o, &sheet, 0).unwrap(), Path::new("out/Various_Artists/1998 - 合集/00.flac"));
        sheet.tracks[0].title = "@performer".into();
        sheet.year.clear();
        assert_eq!(destination(&o, &sheet, 1).unwrap(), Path::new("out/Various_Artists/合集/01 - Various_Artists - @performer.flac"));
    }

    #[test]
    fn performer_subdirectories_keep_album_assets_at_common_prefix_and_nd_flattens() {
        let mut o = Options { out: "out".into(), pattern: "@album/@performer/@track.@ext".into(), ..Options::default() };
        let sheet = album();
        assert_eq!(album_dir(&o, &sheet).unwrap(), Path::new("out/合集"));
        assert_eq!(destination(&o, &sheet, 2).unwrap(), Path::new("out/合集/Other Singer/02.flac"));
        o.subdirs = false;
        assert_eq!(destination(&o, &sheet, 2).unwrap(), Path::new("out/02.flac"));
        assert_eq!(destination(&o, &sheet, 0).unwrap(), Path::new("out/00.flac"));
    }

    #[test]
    fn templates_cannot_escape_output_root() {
        let sheet = album();
        for pattern in ["../bad", "/absolute", r"\\server\music", "C:/music", "@album/../x", "@album/"] {
            let o = Options { pattern: pattern.into(), ..Options::default() };
            assert!(destination(&o, &sheet, 1).is_err(), "{pattern}");
        }
    }

    #[test]
    fn windows_device_names_and_forbidden_characters_are_escaped_only_on_windows() {
        assert_eq!(component("con.TXT", true).unwrap(), "_con.TXT");
        assert_eq!(component("LPT².wav", true).unwrap(), "_LPT².wav");
        assert_eq!(component("A:|?*.", true).unwrap(), "A_____");
        assert_eq!(component("音:乐?", false).unwrap(), "音:乐?");
    }
}
