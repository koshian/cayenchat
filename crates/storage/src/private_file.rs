//! Files only the user can read, replaced whole or not at all.

use std::{
    fs,
    io::{self, Write},
    path::Path,
};

/// Writes `bytes` to `path`: a fresh user-only (`0600`) file next to it is
/// written and flushed to disk, then renamed over `path`. A crash or power
/// loss leaves either the old file or the new one, never half of one, and
/// the contents are never in a file with broader permissions. Missing
/// directories are created user-only (`0700`).
pub(crate) fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the path has no directory"))?;
    create_dir(parent)?;
    let temporary = path.with_extension(format!("tmp{}", std::process::id()));
    let _ = fs::remove_file(&temporary);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        // `rename` replaces an existing file on Windows too.
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Creates missing directories as user-only (`0700`) on Unix. Existing
/// directories keep their permissions.
pub(crate) fn create_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_whole_file_and_leaves_nothing_else() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested").join("file.json");
        write(&path, b"a much longer first version").unwrap();
        write(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["file.json"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(path.parent().unwrap()), 0o700);
        }
    }
}
