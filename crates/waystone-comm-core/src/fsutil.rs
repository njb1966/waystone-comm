use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

/// Atomically replace `path` with `contents`.
///
/// The temporary file is created in the target directory so the final rename is
/// on the same filesystem.
pub(crate) fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    atomic_write_with_mode(path, contents, None)
}

#[cfg(unix)]
pub(crate) fn atomic_write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    atomic_write_with_mode(path, contents, Some(0o600))
}

#[cfg(not(unix))]
pub(crate) fn atomic_write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    atomic_write_with_mode(path, contents, None)
}

fn atomic_write_with_mode(path: &Path, contents: &[u8], mode: Option<u32>) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let temp_path = temp_path_for(path);
    let write_result = write_temp_file(&temp_path, contents, mode).and_then(|()| {
        fs::rename(&temp_path, path)?;
        let _ = sync_parent(path);
        Ok(())
    });

    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }

    write_result
}

fn write_temp_file(path: &Path, contents: &[u8], mode: Option<u32>) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if let Some(mode) = mode {
        options.mode(mode);
    }
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

fn sync_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn temp_path_for(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("waystone-comm");
    let suffix = format!(".tmp-{}-{}", std::process::id(), unique_suffix());
    path.with_file_name(format!("{file_name}{suffix}"))
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}
