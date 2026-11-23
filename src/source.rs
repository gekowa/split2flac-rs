use std::{fs::File, io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write}, path::{Path, PathBuf}};
use anyhow::{Context, Result, bail, ensure};
use chardetng::EncodingDetector;
use encoding_rs::{Encoding, UTF_8};

// Covers the largest possible FLAC metadata block, without trusting a 32-bit
// APE length or accidentally loading a whole album passed as -cue.
const MAX_CUE: u64 = 16 * 1024 * 1024;

pub fn load(input: &Path, explicit: Option<&Path>, charset: Option<&str>, nask: bool, enca: &str) -> Result<String> {
    let hint = detector_hint(enca)?;
    let bytes = if let Some(path) = explicit {
        external(File::open(path).with_context(|| format!("opening CUE {}", path.display()))?)?
    } else {
        let mut suffixed = input.as_os_str().to_owned();
        suffixed.push(".cue");
        let mut chosen = None;
        for path in [PathBuf::from(suffixed), input.with_extension("cue")] {
            if let Ok(f) = File::open(&path) {
                chosen = Some(external(f).with_context(|| format!("reading {}", path.display()))?);
                break;
            }
        }
        match chosen {
            Some(bytes) => bytes,
            None => embedded(input)?.with_context(|| format!("no CUE sheet for {}", input.display()))?,
        }
    };
    decode(bytes, charset, hint, nask)
}

fn external(f: File) -> Result<Vec<u8>> {
    let size = f.metadata()?.len();
    ensure!(size <= MAX_CUE, "CUE sheet exceeds 16 MiB");
    let mut bytes = Vec::with_capacity(size as usize);
    f.take(MAX_CUE + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_CUE, "CUE sheet exceeds 16 MiB");
    Ok(bytes)
}

fn fixed<const N: usize>(f: &mut impl Read) -> io::Result<[u8; N]> {
    let mut b = [0; N]; f.read_exact(&mut b)?; Ok(b)
}

fn le(f: &mut impl Read) -> io::Result<u32> { Ok(u32::from_le_bytes(fixed(f)?)) }

type Reader = BufReader<File>;

fn checked_end(f: &mut Reader, n: u64, limit: u64) -> Result<u64> {
    let pos = f.stream_position()?;
    ensure!(pos <= limit && n <= limit-pos, "metadata field exceeds container bounds");
    Ok(pos + n)
}

fn value(f: &mut Reader, n: u64) -> Result<Vec<u8>> {
    ensure!(n <= MAX_CUE, "embedded CUE exceeds 16 MiB");
    let mut bytes = vec![0; n as usize];
    f.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn embedded(path: &Path) -> Result<Option<Vec<u8>>> {
    let mut f = BufReader::new(File::open(path).with_context(|| format!("opening {}", path.display()))?);
    let size = f.get_ref().metadata()?.len();
    if size < 4 { return Ok(None); }
    let mut magic = fixed::<4>(&mut f)?;
    if &magic[..3] == b"ID3" && size >= 14 {
        let rest = fixed::<6>(&mut f)?;
        if rest[2..].iter().all(|v| v & 0x80 == 0) {
            let tag_size = rest[2..].iter().fold(0u64, |v,b| (v<<7) + u64::from(*b));
            let footer = if magic[3] == 4 && rest[1] & 0x10 != 0 { 10 } else { 0 };
            if tag_size + 14 + footer <= size {
                f.seek(SeekFrom::Start(tag_size + 10 + footer))?;
                magic = fixed::<4>(&mut f)?;
            }
        }
    }
    if &magic == b"fLaC" {
        loop {
            let block = fixed::<4>(&mut f).context("truncated FLAC metadata")?;
            let length = u32::from_be_bytes(block) & 0x00ff_ffff;
            let end = checked_end(&mut f, length.into(), size)?;
            // Type 5 is a binary FLAC seek/CUESHEET block, NOT v122's textual tag.
            if block[0] & 0x7f == 4 {
                if let Some(cue) = flac_comments(&mut f, end)? { return Ok(Some(cue)); }
            }
            if block[0] & 0x80 != 0 { break; }
            f.seek(SeekFrom::Start(end))?;
        }
    }
    // Both WavPack's textual embedded sheet and Monkey's Audio use APEv2 items.
    // Skip unrelated items (notably binary covers) without reading their payloads.
    if size >= 32 {
        if let Some(cue) = ape_items(&mut f, size)? { return Ok(Some(cue)); }
        if size >= 160 {
            f.seek(SeekFrom::Start(size - 128))?;
            if &fixed::<3>(&mut f)? == b"TAG" { return ape_items(&mut f, size - 128); }
        }
    }
    Ok(None)
}

fn flac_comments(f: &mut Reader, limit: u64) -> Result<Option<Vec<u8>>> {
    checked_end(f, 4, limit)?;
    let vendor = le(f)?;
    let end = checked_end(f, vendor.into(), limit)?;
    f.seek(SeekFrom::Start(end))?;
    checked_end(f, 4, limit)?;
    let count = le(f)?;
    for _ in 0..count {
        checked_end(f, 4, limit)?;
        let n = le(f)?;
        let end = checked_end(f,n.into(),limit)?;
        if n > 9 && fixed::<9>(f)?.eq_ignore_ascii_case(b"CUESHEET=") {
            return Ok(Some(value(f, u64::from(n-9))?));
        }
        f.seek(SeekFrom::Start(end))?;
    }
    Ok(None)
}

fn ape_items(f: &mut Reader, end: u64) -> Result<Option<Vec<u8>>> {
    let limit = end - 32;
    f.seek(SeekFrom::Start(limit))?;
    if &fixed::<8>(f)? != b"APETAGEX" { return Ok(None); }
    let version = le(f)?;
    ensure!(matches!(version, 1000 | 2000), "unsupported APE tag version {version}");
    let size = u64::from(le(f)?);
    let count = le(f)?;
    ensure!((32..=end).contains(&size), "invalid APE tag length");
    f.seek(SeekFrom::Start(end-size))?;
    for _ in 0..count {
        checked_end(f,8,limit)?;
        let n = u64::from(le(f)?);
        let flags = le(f)?;
        let mut key = [0; 255];
        let mut len = 0;
        loop {
            let [b] = fixed::<1>(f)?;
            if b == 0 { break; }
            ensure!(len < key.len(), "invalid APE item key");
            key[len] = b; len += 1;
        }
        let next = checked_end(f,n,limit)?;
        if flags & 6 == 0 && key[..len].eq_ignore_ascii_case(b"CUESHEET") && n > 0 {
            return Ok(Some(value(f,n)?));
        }
        f.seek(SeekFrom::Start(next))?;
    }
    Ok(None)
}

#[derive(Clone, Copy, Default)]
struct Hint { region: Option<&'static [u8]>, multibyte_only: bool }

fn language(locale: &str) -> Result<Hint> {
    let lower = locale.to_ascii_lowercase();
    let tag = lower.split(['.', '@']).next().unwrap_or("");
    let mut parts = tag.split(['-', '_']);
    let lang = parts.next().unwrap_or("");
    let region = match lang {
        "none" => return Ok(Hint { region: None, multibyte_only: true }),
        "zh" if parts.any(|s| matches!(s,"tw"|"hk"|"mo"|"hant")) => "tw",
        "zh" => "cn", "ja" => "jp", "ko" => "kr", "uk" => "ua", "be" => "by",
        "cs" => "cz", "sl" => "si", "sv" => "se", "et" => "ee", "el" => "gr",
        "bg" => "bg", "hr" => "hr", "hu" => "hu", "lt" => "lt", "lv" => "lv",
        "pl" => "pl", "ru" => "ru", "sk" => "sk", "ro" => "ro", "fi" => "fi",
        "de" => "de", "fr" => "fr", "en" => "us", "it" => "it", "es" => "es",
        _ => bail!("unsupported -enca language '{locale}'; use a language code such as zh or ru, or -cuecharset"),
    };
    Ok(Hint { region: Some(region.as_bytes()), multibyte_only: false })
}

fn detector_hint(raw: &str) -> Result<Hint> {
    let mut hint = Hint::default();
    let mut words = shell_words::split(raw).context("invalid quoting in -enca")?.into_iter();
    while let Some(word) = words.next() {
        hint = if word == "-L" || word == "--language" {
            language(&words.next().context("missing -enca language")?)?
        } else if let Some(lang) = word.strip_prefix("--language=").or_else(|| word.strip_prefix("-L")) {
            language(lang)?
        } else { bail!("unsupported -enca argument '{word}'; use -L LANGUAGE or -cuecharset CHARSET"); };
    }
    Ok(hint)
}

pub fn validate_enca(raw: &str) -> Result<()> { detector_hint(raw).map(|_| ()) }

fn utf32_bom(bytes: &[u8]) -> Option<bool> {
    if bytes.starts_with(b"\xff\xfe\0\0") { Some(true) }
    else if bytes.starts_with(b"\0\0\xfe\xff") { Some(false) }
    else { None }
}

fn utf32(bytes: &[u8], little: bool) -> Result<String> {
    let skip = if utf32_bom(bytes) == Some(little) { 4 } else { 0 };
    let words = bytes[skip..].chunks_exact(4);
    ensure!(words.remainder().is_empty(), "truncated UTF-32 CUE");
    let mut text = String::with_capacity(words.len());
    for word in words {
        let array = word.try_into()?;
        let scalar = if little { u32::from_le_bytes(array) } else { u32::from_be_bytes(array) };
        text.push(char::from_u32(scalar).context("invalid UTF-32 code point")?);
    }
    Ok(text)
}

fn convert(bytes: &[u8], label: &str) -> Result<String> {
    if label.eq_ignore_ascii_case("UTF-32") { return utf32(bytes, utf32_bom(bytes).unwrap_or(true)); }
    if label.eq_ignore_ascii_case("UTF-32LE") { return utf32(bytes, true); }
    if label.eq_ignore_ascii_case("UTF-32BE") { return utf32(bytes, false); }
    let enc = Encoding::for_label_no_replacement(label.as_bytes()).with_context(|| format!("unknown CUE charset '{label}'"))?;
    let skip = Encoding::for_bom(bytes).filter(|(e,_)| *e == enc).map_or(0,|(_,n)|n);
    enc.decode_without_bom_handling_and_without_replacement(&bytes[skip..])
        .map(|text| text.into_owned()).with_context(|| format!("invalid {} CUE text", enc.name()))
}

fn decode(mut bytes: Vec<u8>, charset: Option<&str>, hint: Hint, nask: bool) -> Result<String> {
    if let Some(label) = charset {
        if let Ok(text) = convert(&bytes,label) { return Ok(text); }
    }
    if let Some(little) = utf32_bom(&bytes) { return utf32(&bytes, little); }
    if let Some((enc,n)) = Encoding::for_bom(&bytes) {
        if enc != UTF_8 {
            return enc.decode_without_bom_handling_and_without_replacement(&bytes[n..])
                .map(|s| s.into_owned()).context("invalid Unicode CUE sheet");
        }
        bytes.drain(..n);
    }
    let bytes = match String::from_utf8(bytes) { Ok(s) => return Ok(s), Err(e) => e.into_bytes() };
    let mut detector = EncodingDetector::new();
    detector.feed(&bytes, true);
    let (enc, confident) = detector.guess_assess(hint.region, false);
    if confident && !(hint.multibyte_only && enc.is_single_byte()) {
        if let Some(text) = enc.decode_without_bom_handling_and_without_replacement(&bytes) { return Ok(text.into_owned()); }
    }
    ensure!(!nask, "could not determine CUE charset; specify -cuecharset CHARSET");
    let mut input = io::stdin().lock();
    let mut answer = String::new();
    loop {
        eprint!("Please enter the CUE charset (or press ENTER to cancel) > "); io::stderr().flush()?;
        answer.clear(); input.read_line(&mut answer)?;
        ensure!(!answer.trim().is_empty(), "CUE charset not specified");
        match convert(&bytes,answer.trim()) {
            Err(e) => eprintln!("{e}"),
            Ok(s) => {
                eprintln!("{s}"); eprint!("Is this right? [Y/n] > "); io::stderr().flush()?;
                answer.clear(); input.read_line(&mut answer)?;
                if answer.trim().is_empty() || answer.trim().eq_ignore_ascii_case("y") { return Ok(s); }
            }
        }
    }
}
