use anyhow::{Result, ensure};

#[derive(Debug, Default)]
pub struct Sheet {
    pub artist: String,
    pub album: String,
    pub year: String,
    pub genre: String,
    pub tracks: Vec<Track>,
    pub(crate) points: Vec<Point>,
    pub(crate) shn_tracks: usize,
    pub(crate) raw: bool,
}

#[derive(Debug, Default)]
pub struct Track {
    pub title: String,
    pub performer: String,
    /// INDEX number and position in CD frames, in source order (including duplicates).
    pub indexes: Vec<(u32, u64)>,
    pub pregap: u64,
    pub postgap: u64,
    // cuetools fills skipped index slots and records generated pregap at INDEX time.
    breakpoint: Option<i128>,
}

#[derive(Debug)]
pub(crate) struct Point {
    pub track: usize,
    pub offset: Offset,
    pub line: usize,
}

#[derive(Debug)]
pub(crate) enum Offset {
    Cd(u64), Bytes(u64), Seconds(u64), Millis(u64, u64), Invalid,
}

fn word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    s.find(char::is_whitespace).map_or((s, ""), |i| (&s[..i], s[i..].trim_start()))
}

fn name(s: &str) -> &str {
    if let Some(quoted) = s.strip_prefix('"') {
        if let Some(end) = quoted.rfind('"') { return &quoted[..end]; }
    }
    word(s).0
}

fn number(s: &str) -> Option<u64> {
    if !s.bytes().all(|b| b.is_ascii_digit()) { return None; }
    s.parse().ok()
}

fn cd_time(s: &str) -> Option<u64> {
    if !s.contains(':') { return number(s); }
    let (m, tail) = s.split_once(':')?;
    let (s, f) = tail.split_once(':')?;
    number(m)?.checked_mul(60)?.checked_add(number(s)?)?.checked_mul(75)?.checked_add(number(f)?)
}

// shntool's extract() scans *backward* for the last numeric field. Only this
// view enforces the two-digit CD frame field or three-digit milliseconds field.
fn shn_offset(line: &str) -> Option<Offset> {
    if line.contains("INDEX") && !line.contains(':') { return None; }
    let numeric = |c: char| c.is_ascii_digit() || matches!(c, ':' | '.');
    let tail = line.trim_end_matches(|c| !numeric(c));
    let token = tail.rsplit(|c| !numeric(c)).next()?;
    let Some((minutes, tail)) = token.split_once(':') else { return Some(Offset::Bytes(number(token)?)); };
    let (sec, frac) = match tail.split_once([':', '.']) { Some((s, f)) => (s, Some(f)), None => (tail, None) };
    if sec.len() != 2 || number(sec)? >= 60 { return None; }
    let seconds = number(minutes)?.checked_mul(60)?.checked_add(number(sec)?)?;
    match frac {
        None => Some(Offset::Seconds(seconds)),
        Some(f) if f.len() == 2 && number(f)? < 75 => Some(Offset::Cd(seconds.checked_mul(75)?.checked_add(number(f)?)?)),
        Some(f) if f.len() == 3 => Some(Offset::Millis(seconds, number(f)?)),
        _ => None,
    }
}

fn global_keyword(keyword: &str) -> bool {
    matches!(keyword, "FILE" | "CATALOG" | "CDTEXTFILE" | "REM" | "TITLE" | "PERFORMER" | "SONGWRITER" | "COMPOSER" | "ARRANGER" | "MESSAGE" | "DISC_ID" | "GENRE" | "TOC_INFO" | "TOC_INFO2" | "UPC_EAN" | "ISRC" | "SIZE_INFO")
}

fn valid_mode(mode: &str) -> bool {
    matches!(mode, "AUDIO" | "MODE1/2048" | "MODE1/2352" | "MODE2/2336" | "MODE2/2048" | "MODE2/2342" | "MODE2/2332" | "MODE2/2352")
}

pub fn parse(text: &str) -> Result<Sheet> {
    let mut sheet = Sheet::default();
    let mut has_statement = false;
    let mut saw_date = false;
    let mut saw_genre = false;
    for (i, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let (key, value) = word(line);
        if i == 0 { sheet.raw = !global_keyword(key); }
        if key == "TRACK" { sheet.shn_tracks += 1; }
        if sheet.raw || (key == "INDEX" && line.contains("INDEX 01")) {
            sheet.points.push(Point { track: sheet.shn_tracks, line: i + 1,
                offset: shn_offset(line).unwrap_or(Offset::Invalid) });
        }
        if key == "REM" {
            let (field, tail) = word(value);
            match field {
                "GENRE" if !saw_genre && !tail.is_empty() => {
                    saw_genre = true;
                    let genre = tail.trim_end();
                    sheet.genre = genre.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(genre).into();
                }
                "DATE" if !saw_date => {
                    saw_date = true;
                    // The shell obtains the integer prefix with awk, then removes its sign.
                    let digits = tail.trim_start_matches(['+', '-']);
                    let end = digits.bytes().take_while(u8::is_ascii_digit).count();
                    let year = digits[..end].trim_start_matches('0');
                    sheet.year = year.into();
                }
                _ => {}
            }
            continue;
        }
        if key.is_empty() { continue; }
        if key == "TRACK" {
            // In cuetools a track needs at least one statement. A second TRACK
            // where a statement was required is consumed by line-level recovery.
            if !sheet.tracks.is_empty() && !has_statement { has_statement = true; continue; }
            let (n, mode) = word(value);
            ensure!(number(n).is_some() && valid_mode(mode.trim_end()), "invalid TRACK definition on line {}", i + 1);
            ensure!(sheet.tracks.len() < 99, "more than 99 CUE tracks");
            sheet.tracks.push(Track::default());
            has_statement = false;
            continue;
        }
        let track = sheet.tracks.last_mut();
        if track.is_some() { has_statement = true; }
        match (key, track) {
            ("TITLE", None) => sheet.album = name(value).into(),
            ("PERFORMER", None) => sheet.artist = name(value).into(),
            ("TITLE", Some(t)) => t.title = name(value).into(),
            ("PERFORMER", Some(t)) => t.performer = name(value).into(),
            ("PREGAP", Some(t)) => { if let Some(p) = cd_time(value.trim_end()) { t.pregap = p; } }
            ("POSTGAP", Some(t)) => { if let Some(p) = cd_time(value.trim_end()) { t.postgap = p; } }
            ("INDEX", Some(t)) => {
                let (n, when) = word(value);
                if let (Ok(n), Some(pos)) = (n.parse::<u32>(), cd_time(when.trim_end())) {
                    if n >= 1 && t.breakpoint.is_none() {
                        t.breakpoint = Some(i128::from(pos) + i128::from(t.pregap));
                    }
                    t.indexes.push((n, pos));
                }
            }
            _ => {}
        }
    }
    ensure!(!sheet.tracks.is_empty() && has_statement, "failed to get number of tracks from CUE sheet");
    // cuebreakpoints --append-gaps skips track 1 and suppresses only positions
    // equal to zero. A missing index returns -1, which *does* print a line.
    for (i, t) in sheet.tracks.iter().enumerate().skip(1) {
        let pos = t.breakpoint.unwrap_or_else(|| i128::from(t.indexes.first().map_or(0, |&(_, p)| p)) - 1);
        ensure!(pos != i128::from(t.pregap), "CUE breakpoint count disagrees with track count (track {})", i + 1);
    }
    Ok(sheet)
}
