use std::{collections::HashSet, ffi::OsStr, fs, io::{self, Write}, path::{Component, Path, PathBuf}};
use anyhow::{Context, Result, anyhow, bail, ensure};
use glob::{MatchOptions, Pattern};
use tempfile::{Builder, NamedTempFile, TempDir, TempPath};
use crate::{cli::Options, cue::{self, Sheet}, gain, layout, media, naming, source, split::{self, Output}};

fn ext_is(path: &Path, ext: &str) -> bool {
    path.extension().and_then(OsStr::to_str).is_some_and(|v| v.eq_ignore_ascii_case(ext))
}

fn inputs(root: &Path, failures: &mut Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    // Snapshot before creating any output, even when the output lives inside root.
    let mut pending = vec![root.to_owned()];
    let mut files = Vec::new();
    while let Some(path) = pending.pop() {
        let entries = match fs::read_dir(&path) { Ok(v) => v, Err(e) => { eprintln!("{path:?}: {e}"); failures.push(path); continue; } };
        for entry in entries {
            let entry = match entry { Ok(v)=>v, Err(e)=>{eprintln!("{path:?}: {e}");failures.push(path.clone());continue;} };
            let kind = match entry.file_type() { Ok(v)=>v, Err(e)=>{eprintln!("{:?}: {e}", entry.path());failures.push(entry.path());continue;} };
            let path = entry.path();
            if kind.is_dir() { pending.push(path); }
            else if kind.is_file() && ["ape", "flac", "tta", "wv", "wav"].iter().any(|e| ext_is(&path,e)) { files.push(path); }
        }
    }
    files.sort_unstable();
    Ok(files)
}

pub fn process(opt: &Options, collection: bool) -> Result<()> {
    if !collection { return one(opt, &opt.input, false); }
    let mut inaccessible = Vec::new();
    let paths = inputs(&opt.input,&mut inaccessible)?;
    let mut failures = Vec::new();
    for path in &paths {
        if let Err(e) = one(opt, path, true) {
            eprintln!("{}: {e:#}", path.display()); failures.push(path);
        }
    }
    failures.extend(inaccessible.iter());
    failures.sort_unstable();
    failures.dedup();
    let report = Path::new("split_failed.txt");
    if !failures.is_empty() {
        let mut out = NamedTempFile::new_in(".")?;
        for p in &failures { writeln!(out, "{}", p.display())?; }
        out.persist(report)?;
        bail!("{} album(s) failed; see {}", failures.len(), report.display());
    }
    if report.is_file() { fs::remove_file(report)?; }
    Ok(())
}

fn source_dir(input: &Path) -> &Path { input.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")) }

fn find_cover(dir: &Path) -> Result<Option<PathBuf>> {
    fn scan(dir: &Path, subdirs: Option<&mut Vec<PathBuf>>) -> Result<Option<PathBuf>> {
        let mut folders = subdirs;
        let mut covers = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?; let ty = entry.file_type()?; let path = entry.path();
            if ty.is_dir() { if let Some(ds) = folders.as_deref_mut() { ds.push(path); } }
            else if ty.is_file() && ext_is(&path,"jpg") {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if name.contains("cover") { covers.push((0,path)); }
                else if name.contains("folder") { covers.push((1,path)); }
            }
        }
        Ok(covers.into_iter().min().map(|(_,p)| p))
    }
    let mut dirs = vec![];
    if let Some(p) = scan(dir,Some(&mut dirs))? { return Ok(Some(p)); }
    dirs.sort_unstable();
    for d in dirs { if let Some(p) = scan(&d,None)? { return Ok(Some(p)); } }
    Ok(None)
}

fn tags(sheet: &Sheet, number: usize, format: &str) -> Vec<(String,String)> {
    if number == 0 || number > sheet.tracks.len() || format == "wav" { return Vec::new(); }
    let t = &sheet.tracks[number-1];
    let artist = if t.performer.is_empty() { &sheet.artist } else { &t.performer };
    let mut v = vec![("ARTIST".into(),artist.clone()),("ALBUM".into(),sheet.album.clone()),("TITLE".into(),t.title.clone())];
    let n = sheet.tracks.len();
    if matches!(format,"m4a"|"mp3") { v.push(("track".into(),format!("{number}/{n}"))); }
    else { v.push(("track".into(),number.to_string())); v.push(("TRACKTOTAL".into(),n.to_string())); }
    if !sheet.year.is_empty() { v.push(("DATE".into(),sheet.year.clone())); }
    if !sheet.genre.is_empty() { v.push(("GENRE".into(),sheet.genre.clone())); }
    v
}

// Follow only existing directories underneath the selected root. Check the
// parent before creating its child, not after writing through a symlink.
fn ensure_subdir(root: &Path, path: &Path) -> Result<()> {
    let relative = path.strip_prefix(root).context("output is outside selected directory")?;
    let boundary = root.canonicalize()?;
    let mut dir = root.to_path_buf();
    for part in relative.components() {
        let Component::Normal(name) = part else { ensure!(part == Component::CurDir, "invalid output path"); continue };
        dir.push(name);
        match fs::create_dir(&dir) {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
        ensure!(!fs::symlink_metadata(&dir)?.file_type().is_symlink() && dir.is_dir() && dir.canonicalize()?.starts_with(&boundary), "invalid output directory: {}",dir.display());
    }
    Ok(())
}

// Mirroring names into a reservation directory on the actual volume lets that
// filesystem decide case/Unicode equivalence. Never open or truncate user files.
fn reserve(directory: &Path, destinations: &[PathBuf], root: &Path, collection: bool) -> Result<TempDir> {
    fs::create_dir_all(root)?;
    ensure_subdir(root, directory)?;
    let work = Builder::new().prefix(".split2flac-").tempdir_in(directory)?;
    let names = work.path().join("names"); fs::create_dir(&names)?;
    let cwd = std::env::current_dir()?.canonicalize()?;
    for target in destinations {
        let parent = target.parent().context("missing output directory")?;
        ensure_subdir(directory, parent)?;
        if collection && parent.canonicalize()? == cwd && target.file_name().is_some_and(|s| s.eq_ignore_ascii_case("split_failed.txt")) {
            bail!("output conflicts with the collection failure report");
        }
        match fs::symlink_metadata(target) {
            Ok(_) => bail!("output exists: {}",target.display()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
        let relative = target.strip_prefix(directory)?;
        let shadow = names.join(relative);
        fs::create_dir_all(shadow.parent().context("invalid reservation path")?)?;
        fs::File::create_new(shadow).with_context(|| format!("conflicting output: {}",target.display()))?;
    }
    Ok(work)
}

fn publish(staged: &Path, target: &Path) -> Result<()> {
    TempPath::try_from_path(staged)?.persist_noclobber(target).with_context(|| format!("publish {}",target.display()))?;
    Ok(())
}

fn one(opt: &Options, input: &Path, collection: bool) -> Result<()> {
    let identity = if opt.remove && !opt.dry {Some(same_file::Handle::from_path(input)?)} else {None};
    let explicit = (!collection).then_some(opt.cue.as_deref()).flatten();
    let text = source::load(input,explicit,opt.charset.as_deref(),opt.nask,&opt.enca)?;
    let sheet = cue::parse(&text)?;
    let directory = naming::album_dir(opt,&sheet)?;
    println!("{}{} - {}{}",if opt.colors {"\x1b[32m"} else {""},sheet.artist.escape_debug(),sheet.album.escape_debug(),if opt.colors {"\x1b[0m"} else {""});
    if opt.dry {
        for number in 1..=sheet.tracks.len() { println!("   -> {}",naming::destination(opt,&sheet,number)?.display()); }
        return Ok(());
    }
    let pcm = media::probe(input)?;
    let segments = layout::plan_pcm(&sheet,&pcm)?;
    let mut destinations = Vec::with_capacity(segments.len());
    for seg in &segments {
        destinations.push(if seg.number > sheet.tracks.len() { directory.join(format!("{:02}.{}",seg.number,opt.format)) } else { naming::destination(opt,&sheet,seg.number)? });
    }
    let extras = if opt.copy { companions(input,&directory,&opt.masks)? } else { Vec::new() };
    destinations.extend(extras.iter().map(|(_,dest)|dest.clone()));
    let work = reserve(&directory,&destinations,&opt.out,collection)?;
    let cover_path = work.path().join("cover.jpg");
    let cover = if opt.no_cover || !matches!(opt.format.as_str(),"flac"|"m4a") { None } else {
        let external = if !collection && opt.cover.is_some() { opt.cover.clone() } else { find_cover(source_dir(input))? };
        if split::cover(input,external.as_deref(),&opt.cover_size,&cover_path)? { Some(cover_path.as_path()) } else { None }
    };
    let outputs: Vec<_> = segments.iter().map(|s| Output {number:s.number, start:s.start, start_byte:s.start_byte, path:work.path().join(format!("{:02}.{}",s.number,opt.format)),tags:tags(&sheet,s.number,&opt.format)}).collect();
    let outputs = match split::run(input,&pcm,&outputs,&opt.format,&opt.encargs,cover) {
        Ok(()) => &outputs[..],
        Err(e) if e.is::<split::EmptyFinalSegment>() => &outputs[..outputs.len()-1],
        Err(e) => return Err(e),
    };
    // shnsplit discards a boundary exactly at EOF; the original wrapper then
    // renames/tags preceding tracks before discovering that the last is absent.
    let last = outputs.last().map_or(0, |o| o.number);
    for n in last+1..=sheet.tracks.len() {
        if opt.format != "wav" || naming::destination(opt,&sheet,n)? != directory.join(format!("{n:02}.wav")) {
            for (o,d) in outputs.iter().zip(&destinations) { publish(&o.path,d)?; }
            bail!("no samples for track {n} at end of image");
        }
    }
    if opt.gain { let paths=outputs.iter().map(|o|o.path.clone()).collect::<Vec<_>>(); gain::apply(&opt.format,&paths)?; }
    for (o,d) in outputs.iter().zip(&destinations) { publish(&o.path,d)?; println!("   -> {}",d.display()); }
    for (src,dest) in extras {copy_item(&src,&dest)?;}
    if last >= sheet.tracks.len() && let Some(original) = identity && (opt.force || confirm(input)?) {
        ensure!(original == same_file::Handle::from_path(input)?, "source changed during processing; not deleting {input:?}");
        drop(original);
        fs::remove_file(input).with_context(||format!("remove {input:?}"))?;
    }
    Ok(())
}

fn confirm(input: &Path) -> Result<bool> {
    eprint!("Delete original {}? [y/N] ",input.display()); io::stderr().flush()?;
    let mut line=String::new(); io::stdin().read_line(&mut line)?; Ok(line.trim().eq_ignore_ascii_case("y"))
}

fn companions(input: &Path, output: &Path, masks: &str) -> Result<Vec<(PathBuf,PathBuf)>> {
    let src=source_dir(input).canonicalize()?;
    let out=std::path::absolute(output)?;
    let out=out.canonicalize().unwrap_or(out);
    if src==out { return Ok(Vec::new()); }
    let input=input.canonicalize()?;
    let masks = if cfg!(windows) {masks.replace('\\',"/")} else {masks.to_owned()};
    let patterns=shell_words::split(&masks)?;
    let mut files = Vec::new();
    let mut selected=HashSet::new();
    let opts=MatchOptions { case_sensitive: !cfg!(windows), require_literal_separator:true, require_literal_leading_dot:true };
    for mask in patterns {
        let mut matches=vec![src.clone()];
        for component in Path::new(&mask).components() {
            if component==Component::CurDir {continue;}
            let Component::Normal(name)=component else {bail!("invalid copy pattern: {mask}");};
            let pattern=Pattern::new(name.to_str().context("invalid copy pattern")?)?;
            let mut children=Vec::new();
            for dir in matches {
                if !fs::symlink_metadata(&dir)?.is_dir() {continue;}
                for entry in fs::read_dir(dir)? {
                    let entry=entry?;
                    if pattern.matches_with(&entry.file_name().to_string_lossy(),opts) {children.push(entry.path());}
                }
            }
            matches=children;
        }
        for entry in matches {
            if !selected.insert(entry.clone()) {continue;}
            let name=entry.file_name().ok_or_else(||anyhow!("empty copy source"))?;
            let real=entry.canonicalize()?;
            if !real.starts_with(&src) || real==input || (out.starts_with(&src) && real.starts_with(&out)) || out.starts_with(&real) { continue; }
            let dest=output.join(name);
            files.push((entry,dest));
        }
    }
    Ok(files)
}

fn copy_item(source:&Path,dest:&Path)->Result<()> {
    let ty=fs::symlink_metadata(source)?.file_type();
    if ty.is_dir() {
        fs::create_dir(dest).with_context(||format!("copy directory {}", dest.display()))?;
        for entry in fs::read_dir(source)? { let entry=entry?; copy_item(&entry.path(),&dest.join(entry.file_name()))?; }
    } else if ty.is_file() {
        let mut reader=fs::File::open(source)?; let mut temp=NamedTempFile::new_in(dest.parent().context("missing copy target directory")?)?;
        io::copy(&mut reader,&mut temp)?; temp.persist_noclobber(dest).with_context(||format!("copy to {}",dest.display()))?;
    }
    Ok(())
}
