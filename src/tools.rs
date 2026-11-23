use std::{env, ffi::OsString, path::PathBuf};

/// A packaged installation is self-contained: a missing helper is an error,
/// not an invitation to run a different version from the current directory.
pub fn binary(name: &str) -> PathBuf {
    let mut file = OsString::from(name);
    file.push(env::consts::EXE_SUFFIX);
    if let Some(dir) = env::var_os("SPLIT2FLAC_TOOLS") {
        return PathBuf::from(dir).join(file);
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            let tools = dir.join("tools");
            if tools.is_dir() { return tools.join(file); }
        }
    }
    PathBuf::from(file)
}
