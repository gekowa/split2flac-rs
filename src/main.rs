use std::process::ExitCode;
use split2flac_rs::{album, cli};

fn main() -> ExitCode {
    let options = match cli::parse() {
        Ok(Some(o)) => o,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => { eprintln!("{e:#}"); return ExitCode::from(1); }
    };
    let metadata = match options.input.metadata() {
        Ok(meta) if meta.is_dir() || meta.is_file() => meta,
        Ok(_) => { eprintln!("Not a regular file or directory: {}", options.input.display()); return ExitCode::from(2); }
        Err(e) => { eprintln!("{}: {e}", options.input.display()); return ExitCode::from(2); }
    };
    if metadata.is_dir() && std::fs::read_dir(&options.input).is_err()
        || metadata.is_file() && std::fs::File::open(&options.input).is_err() {
        eprintln!("Cannot read {}", options.input.display()); return ExitCode::from(2);
    }
    match album::process(&options, metadata.is_dir()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("{e:#}"); ExitCode::from(3) }
    }
}
