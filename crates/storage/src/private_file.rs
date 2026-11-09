// SPDX-License-Identifier: MPL-2.0
//! Files only the user can read, replaced whole or not at all.

use std::{
    fs,
    io::{self, Write},
    path::Path,
};

/// Whether [`write`] waits for the data to reach the disk before it renames.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flush {
    /// Flush the file first, so that a power loss leaves the old or the new
    /// contents. This waits for the disk: do not use it on the UI thread.
    Disk,
    /// Rename without waiting. A crash of the process still leaves the old
    /// or the new file whole, but after a power loss the file system decides
    /// whether the new file has its contents.
    Skip,
}

/// Writes `bytes` to `path`: a fresh user-only (`0600`) file next to it is
/// written (and flushed to disk with [`Flush::Disk`]), then renamed over
/// `path`. A crash during the write leaves the old file, and the rename
/// replaces it whole, so no reader sees half of a file. The directory is not
/// flushed with [`Flush::Skip`], so after a power loss the rename itself may
/// not have happened yet; [`Flush::Disk`] makes the rename durable too (the
/// directory on Unix, write-through on Windows) and fails if it cannot. The
/// contents are never in a file with broader permissions. Missing
/// directories are created user-only (`0700`).
pub(crate) fn write(path: &Path, bytes: &[u8], flush: Flush) -> io::Result<()> {
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
        if flush == Flush::Disk {
            file.sync_all()?;
        }
        if flush == Flush::Disk {
            replace_durably(&temporary, path, parent)
        } else {
            // `rename` replaces an existing file on Windows too.
            fs::rename(&temporary, path)
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Renames `from` over `to` and returns only once the rename itself has
/// reached the disk: on Unix by flushing the directory, on Windows by
/// `MoveFileExW` with `MOVEFILE_WRITE_THROUGH`. An error means the rename is
/// not known to be durable, so the caller must keep what it would delete.
fn replace_durably(from: &Path, to: &Path, parent: &Path) -> io::Result<()> {
    #[cfg(test)]
    tests::DURABLE_REPLACES.with(|count| count.set(count.get() + 1));
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        let wide =
            |path: &Path| -> Vec<u16> { path.as_os_str().encode_wide().chain(Some(0)).collect() };
        let _ = parent;
        let (from, to) = (wide(from), wide(to));
        // SAFETY: both buffers are NUL-terminated and outlive the call.
        let moved = unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::rename(from, to)?;
        fs::File::open(parent)?.sync_all()
    }
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
    use std::cell::Cell;

    thread_local! {
        pub(super) static DURABLE_REPLACES: Cell<u32> = const { Cell::new(0) };
    }

    #[test]
    fn only_a_disk_write_makes_the_replacement_durable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.json");
        let count = || DURABLE_REPLACES.with(Cell::get);
        write(&path, b"a", Flush::Skip).unwrap();
        assert_eq!(count(), 0);
        write(&path, b"b", Flush::Disk).unwrap();
        assert_eq!(count(), 1);
        assert_eq!(fs::read(&path).unwrap(), b"b");
    }

    #[test]
    fn replaces_the_whole_file_and_leaves_nothing_else() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested").join("file.json");
        write(&path, b"a much longer first version", Flush::Disk).unwrap();
        write(&path, b"second", Flush::Skip).unwrap();
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
