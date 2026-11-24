use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const FORMATS: [&str; 6] = ["flac", "m4a", "mp3", "ogg", "opus", "wav"];

// Only the starred preferences in the original CLI survive -s. In
// particular, permission to use cover art is persistent; a cover path isn't.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Options {
    #[serde(skip)]
    pub input: PathBuf,
    pub out: PathBuf,
    pub pattern: String,
    #[serde(skip)]
    pub format: String,
    #[serde(skip)]
    pub encargs: Vec<String>,
    #[serde(skip)]
    pub cue: Option<PathBuf>,
    #[serde(skip)]
    pub charset: Option<String>,
    #[serde(skip)]
    pub nask: bool,
    pub enca: String,
    #[serde(skip)]
    pub cover: Option<PathBuf>,
    pub no_cover: bool,
    pub cover_size: String,
    pub masks: String,
    pub copy: bool,
    pub subdirs: bool,
    #[serde(skip)]
    pub dry: bool,
    pub remove: bool,
    #[serde(skip)]
    pub force: bool,
    pub colors: bool,
    pub gain: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            input: PathBuf::new(),
            out: ".".into(),
            pattern: "@artist/{@year - }@album/@track - @title.@ext".into(),
            format: "flac".into(),
            encargs: Vec::new(),
            cue: None,
            charset: None,
            nask: false,
            enca: String::new(),
            cover: None,
            no_cover: false,
            cover_size: "192x192".into(),
            masks: "[Cc]overs *.log *.txt *.jpg *.cbr".into(),
            copy: true,
            subdirs: true,
            dry: false,
            remove: false,
            force: false,
            colors: true,
            gain: false,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    settings: Options,
    encoder_args: BTreeMap<String, Vec<String>>,
}

fn config_path() -> Option<PathBuf> {
    let xdg = env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty())
        .map(|dir| PathBuf::from(dir).join("split2flac").join("split2flac.conf"));
    if let Some(path) = xdg.filter(|p| p.is_file()) {
        return Some(path);
    }
    env::var_os("HOME").filter(|v| !v.is_empty())
        .or_else(|| env::var_os("USERPROFILE").filter(|v| !v.is_empty()))
        .map(|dir| PathBuf::from(dir).join(".split2flac"))
}

fn default_args(format: &str) -> Vec<String> {
    let words: &[&str] = match format {
        "flac" => &["-compression_level", "8"],
        "m4a" | "mp3" => &["-q:a", "2"],
        "ogg" => &["-q:a", "5"],
        _ => &[],
    };
    words.iter().map(|s| (*s).to_owned()).collect()
}

fn legacy_encoder(format: &str, raw: &str) -> Result<Vec<String>> {
    ensure!(FORMATS.contains(&format), "unknown encoder {format}");
    let args = shell_words::split(raw).context("invalid quotes in legacy encoder arguments")?;
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let (key, val) = match (format, words.as_slice()) {
        (_, []) => return Ok(Vec::new()),
        ("flac", ["--best"]) => ("-compression_level", "8"),
        ("flac", ["--fast"]) => ("-compression_level", "0"),
        ("flac", [flag]) if matches!(*flag, "-0"|"-1"|"-2"|"-3"|"-4"|"-5"|"-6"|"-7"|"-8") =>
            ("-compression_level", &flag[1..]),
        ("m4a", ["-q", "500"]) => ("-q:a", "2"),
        ("mp3", ["--preset", "standard"]) => ("-q:a", "2"),
        ("mp3", ["--preset", "extreme"]) => ("-q:a", "0"),
        ("mp3", ["--preset", "insane"]) => ("-b:a", "320k"),
        ("ogg", ["-q", value]) if value.parse::<f32>().is_ok() => ("-q:a", *value),
        _ => bail!("cannot migrate ENCARGS_{format}={raw:?} to FFmpeg; replace the legacy setting with an empty string, then use -e with FFmpeg audio encoder options and -s"),
    };
    Ok(vec![key.into(), val.into()])
}

fn legacy_bool(value: &str) -> Result<bool> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => bail!("expected 0 or 1, got {value:?}"),
    }
}

impl Config {
    fn from_legacy(text: &str) -> Result<Self> {
        let mut c = Self::default();
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, rhs) = line.split_once('=')
                .with_context(|| format!("line {}: expected KEY=value; shell commands are not supported", i + 1))?;
            let mut values = shell_words::split(rhs)
                .with_context(|| format!("line {}: invalid quotes for {key}", i + 1))?;
            ensure!(values.len() <= 1, "line {}: quote spaces in {key} (shell commands are not supported)", i + 1);
            let value = values.pop().unwrap_or_default();
            let o = &mut c.settings;
            match key {
                "DIR" => o.out = value.into(),
                "OUTPATTERN" => o.pattern = value,
                "COPYMASKS" => o.masks = value,
                "COPYFILES" => o.copy = legacy_bool(&value)?,
                "NOSUBDIRS" => o.subdirs = !legacy_bool(&value)?,
                "NOPIC" => o.no_cover = legacy_bool(&value)?,
                "REMOVE" => o.remove = legacy_bool(&value)?,
                "PIC_SIZE" => o.cover_size = value,
                "NOCOLORS" => o.colors = !legacy_bool(&value)?,
                "REPLAY_GAIN" => o.gain = legacy_bool(&value)?,
                "ENCA_ARGS" => o.enca = value,
                key if key.starts_with("ENCARGS_") => {
                    let format = &key["ENCARGS_".len()..];
                    c.encoder_args.insert(format.into(), legacy_encoder(format, &value)?);
                }
                _ => bail!("line {}: unsupported legacy setting {key:?}", i + 1),
            }
        }
        Ok(c)
    }

    fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let text = text.trim_start_matches('\u{feff}').trim_start();
        if text.starts_with('{') {
            serde_json::from_str(text).context("invalid split2flac JSON configuration")
        } else {
            Self::from_legacy(text)
        }
        .with_context(|| format!("loading {}", path.display()))
    }

    fn save(&self, path: &Path) -> Result<()> {
        let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let mut f = tempfile::NamedTempFile::new_in(dir)?;
        serde_json::to_writer_pretty(&mut f, self)?;
        writeln!(f)?;
        f.as_file().sync_all()?;
        f.persist(path)?;
        Ok(())
    }
}

const HELP: &str = "\
Usage: split2FORMAT-rs [OPTIONS] FILE|DIR [OPTIONS]
Split APE, FLAC, TTA, WavPack or WAV using an external or embedded CUE sheet.

  -p                    Dry run: show the plan without writing audio or deleting files
  -o DIRECTORY          Output directory (default: current directory)
  -of PATTERN           Output naming template (see below)
  -cue FILE             Use this CUE for a single input image
  -cuecharset CHARSET   Decode CUE text using this charset
  -nask                 Do not prompt for an encoding
  -f FORMAT             Select flac, m4a, mp3, ogg, opus or wav
  -e 'OPTIONS'          FFmpeg audio encoder options, e.g. '-b:a 192k'
  -eh                   Show help for the selected encoder and exit
  -enca 'OPTIONS'       Charset detector language hint, e.g. '-L zh'
  -c FILE / -nc         Select a single-image cover / disable covers
  -cs WxH               Maximum cover size (default: 192x192)
  -C 'MASKS' / -nC      Copy matching companion files / disable copying
  -d / -nd              Use template directories / output filenames only
  -D / -nD              Delete source after album succeeds / keep it (default)
  -F                    Do not prompt for deletion; never overwrite output
  -colors / -nocolors   Enable / disable colored output
  -g / -ng              Calculate ReplayGain tags / skip gain analysis (default)
  -s                    Save preferences (not input, cue, cover path or force flag)
  -h, -help, --help     Show this help
  -v, --version         Show version information

Template fields: @artist, @album, @year, @genre, @track, @title, @performer, @ext.
Use @_artist etc. to change spaces to underscores; {@year - } is optional.
Default: @artist/{@year - }@album/@track - @title.@ext
Track performer falls back to album artist. Use / for template directories.";

pub fn parse() -> Result<Option<Options>> {
    parse_from(env::args_os(), config_path().as_deref())
}

fn parse_from(mut args: impl Iterator<Item = OsString>, config: Option<&Path>) -> Result<Option<Options>> {
    let exe = args.next().unwrap_or_default();
    let alias = Path::new(&exe).file_stem().and_then(|s| s.to_str())
        .and_then(|s| s.get(..6).filter(|p| p.eq_ignore_ascii_case("split2")).and_then(|_| s.get(6..)))
        .and_then(|suffix| FORMATS.into_iter().find(|f| f.eq_ignore_ascii_case(suffix)))
        .unwrap_or("flac");
    let mut c = config.map(Config::load).transpose()?.unwrap_or_default();
    let o = &mut c.settings;
    o.format = alias.into();
    let (mut save, mut enc_help, mut positional) = (false, false, false);
    let mut encoder = None;
    while let Some(arg) = args.next() {
        let flag = arg.to_str().unwrap_or("");
        let mut value = || args.next().with_context(|| format!("{flag} requires a value"));
        let mut text = || value()?.into_string().map_err(|_| anyhow!("{flag} requires a Unicode value"));
        match if positional { "" } else { flag } {
            "--" => positional = true,
            "-o" => o.out = value()?.into(),
            "-of" => o.pattern = text()?,
            "-cue" => o.cue = Some(value()?.into()),
            "-cuecharset" => o.charset = Some(text()?),
            "-nask" => o.nask = true,
            "-f" => o.format = text()?,
            "-e" => encoder = Some(shell_words::split(&text()?).context("invalid quoting in -e")?),
            "-eh" => enc_help = true,
            "-enca" => o.enca = text()?,
            "-c" => { o.cover = Some(value()?.into()); o.no_cover = false; }
            "-nc" => o.no_cover = true,
            "-C" => { o.masks = text()?; o.copy = true; }
            "-nC" => o.copy = false,
            "-cs" => o.cover_size = text()?,
            "-d" => o.subdirs = true,
            "-nd" => o.subdirs = false,
            "-p" => o.dry = true,
            "-D" => o.remove = true,
            "-nD" => o.remove = false,
            "-F" => o.force = true,
            "-colors" => o.colors = true,
            "-nocolors" => o.colors = false,
            "-g" => o.gain = true,
            "-ng" => o.gain = false,
            "-s" => save = true,
            "-h" | "-help" | "--help" => {
                println!("{HELP}\n\nCurrent: format={}, output={}, pattern={}", o.format, o.out.display(), o.pattern);
                return Ok(None);
            }
            "-v" | "--version" => {
                println!("split2{} version: 122 (Rust port {})", o.format, env!("CARGO_PKG_VERSION"));
                for name in ["ffmpeg", "ffprobe"] {
                    if let Ok(output) = Command::new(crate::tools::binary(name)).arg("-version").output() {
                        if output.status.success() {
                            if let Some(line) = String::from_utf8_lossy(&output.stdout).lines().next() {
                                println!("{line}");
                            }
                        }
                    }
                }
                return Ok(None);
            }
            flag if flag.starts_with('-') => bail!("unknown option {flag}; use -h for help"),
            _ => {
                ensure!(o.input.as_os_str().is_empty(), "expected one input image or directory, not {:?}", arg);
                ensure!(!arg.is_empty(), "empty input path");
                o.input = arg.into();
            }
        }
    }
    ensure!(FORMATS.contains(&o.format.as_str()), "unknown output format {:?}", o.format);
    ensure!(!o.out.as_os_str().is_empty(), "output directory must not be empty");
    ensure!(!o.pattern.is_empty(), "output template must not be empty");
    if let Some(args) = encoder {
        c.encoder_args.insert(o.format.clone(), args);
    }
    for (format, args) in &c.encoder_args {
        ensure!(FORMATS.contains(&format.as_str()), "unknown configured output format {format}");
        crate::split::validate_args(format, args)
            .with_context(|| format!("invalid FFmpeg arguments for {format}; see -f {format} -eh"))?;
    }
    crate::source::validate_enca(&o.enca)?;
    if save {
        let path = config.context("cannot save configuration: HOME/USERPROFILE not set")?;
        c.save(path).with_context(|| format!("saving {}", path.display()))?;
        println!("Configuration saved to {}", path.display());
    }
    let mut o = c.settings;
    if enc_help {
        crate::media::encoder_help(&o.format)?;
        return Ok(None);
    }
    ensure!(!o.input.as_os_str().is_empty(), "no input filename given; use -h for help");
    o.encargs = c.encoder_args.remove(&o.format).unwrap_or_else(|| default_args(&o.format));
    Ok(Some(o))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn invoke(path: &Path, args: &[&str]) -> Result<Option<Options>> {
        parse_from(args.iter().map(OsString::from), Some(path))
    }

    #[test]
    fn alias_and_options_on_either_side_of_image() {
        let tmp = tempdir().unwrap();
        let cfg = tmp.path().join("config");
        let o = invoke(&cfg, &["split2mp3.exe", "-cuecharset", "GBK", "音乐.flac", "-c", "art.jpg", "-nc", "-C", "*.log", "-nC", "-D", "-F", "-g", "-nd", "-p"]).unwrap().unwrap();
        assert_eq!(o.format, "mp3");
        assert_eq!(o.encargs, ["-q:a", "2"]);
        assert_eq!(o.input, Path::new("音乐.flac"));
        assert_eq!(o.charset.as_deref(), Some("GBK"));
        assert!(o.no_cover && !o.copy && o.remove && o.force && o.gain && !o.subdirs && o.dry);
        assert_eq!(invoke(&cfg, &["split2flac", "-e", "-b:a '192k'", "song", "-f", "mp3"]).unwrap().unwrap().encargs, ["-b:a", "192k"]);
    }

    #[test]
    fn persisted_preferences_are_per_codec_and_transient_flags_stay_transient() {
        let tmp = tempdir().unwrap();
        let cfg = tmp.path().join(".split2flac");
        invoke(&cfg, &["split2ogg", "-s", "-e", "-q:a 7", "-o", "音乐 library", "-of", "@track.@ext", "-nc", "-c", "not-persisted.jpg", "-cue", "explicit.cue", "-nask", "-p", "-F", "-D", "-nd", "-nC", "-nocolors", "-g", "album"]).unwrap();
        let o = invoke(&cfg, &["split2ogg", "album2"]).unwrap().unwrap();
        assert_eq!(o.encargs, ["-q:a", "7"]);
        assert_eq!(o.out, Path::new("音乐 library"));
        assert_eq!(o.pattern, "@track.@ext");
        assert!(o.remove && o.gain && !o.colors && !o.copy && !o.subdirs && !o.no_cover);
        assert!(o.cover.is_none() && o.cue.is_none() && !o.nask && !o.dry && !o.force);
        let flac = invoke(&cfg, &["split2flac", "album"]).unwrap().unwrap();
        assert_eq!(flac.encargs, ["-compression_level", "8"]);
    }

    #[test]
    fn reads_legacy_assignments_as_data_and_migrates_codec_defaults() {
        let tmp = tempdir().unwrap();
        let cfg = tmp.path().join(".split2flac");
        let marker = tmp.path().join("must-not-exist");
        fs::write(&cfg, format!("DIR=\"$(touch '{}')\"\nCOPYFILES=0\nENCARGS_m4a=\"-q 500\"\nENCARGS_mp3=\"--preset standard\"\n", marker.display())).unwrap();
        let o = invoke(&cfg, &["split2m4a", "album"]).unwrap().unwrap();
        assert!(!marker.exists());
        assert!(o.out.to_string_lossy().starts_with("$(touch"));
        assert!(!o.copy);
        assert_eq!(o.encargs, ["-q:a", "2"]);
        fs::write(&cfg, "ENCARGS_mp3='--private-old-switch'\n").unwrap();
        assert!(format!("{:#}", invoke(&cfg, &["split2mp3", "album"]).unwrap_err()).contains("FFmpeg"));
    }

    #[test]
    fn rejects_extra_images_missing_values_unknown_switches_and_unsafe_encoder_args() {
        let tmp = tempdir().unwrap();
        let cfg = tmp.path().join("config");
        for args in [&["split2wav", "a", "b"][..], &["split2wav", "-o"], &["split2wav", "-x", "a"], &["split2flac", "-p", "a", "-e", "-i other.wav"], &["split2wav", "-f", "bogus", "a"]] {
            assert!(invoke(&cfg, args).is_err(), "{args:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_input_path_is_not_lossy() {
        use std::os::unix::ffi::OsStringExt;
        let tmp = tempdir().unwrap();
        let raw = OsString::from_vec(b"image\xff.flac".to_vec());
        let o = parse_from([OsString::from("split2flac"), raw.clone()].into_iter(), Some(&tmp.path().join("config"))).unwrap().unwrap();
        assert_eq!(o.input.as_os_str(), raw.as_os_str());
    }
}
