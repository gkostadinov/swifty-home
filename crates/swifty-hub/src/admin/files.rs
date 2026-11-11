//! Writing the hub's own files: whole or not at all, with the mode and the owner
//! they need, and undone when a command that wrote several of them fails.
//!
//! Modes and owners are Unix ideas. Elsewhere they are left to the system.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

/// Who the files a command writes belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// Whoever runs the command: nothing is changed.
    Current,
    /// Run as root for a service user: files are given to this user and group.
    /// The configuration directory and `swifty.toml` stay root's, in this group.
    Service { uid: u32, gid: u32 },
}

/// The mode a file has, without the type bits. `None` where there are no modes.
pub fn mode_of(path: &Path) -> io::Result<Option<u32>> {
    let metadata = std::fs::metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(Some(metadata.permissions().mode() & 0o7777))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Ok(None)
    }
}

/// Sets a file's or directory's mode. Does nothing where there are no modes.
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// Gives a file or directory to a user and group. Never follows a symlink.
pub fn set_owner(path: &Path, uid: u32, gid: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::lchown(path, Some(uid), Some(gid))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, uid, gid);
        Ok(())
    }
}

/// The owner and group of a path, where there are such things.
pub fn owner_of(path: &Path) -> io::Result<Option<(u32, u32)>> {
    let metadata = std::fs::symlink_metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(Some((metadata.uid(), metadata.gid())))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Ok(None)
    }
}

/// A name for a file that is being written, beside where it will be.
fn pending_name(path: &Path) -> PathBuf {
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let random = hex::encode(crate::crypto::random_bytes::<6>());
    path.with_file_name(format!(".{name}.{random}.tmp"))
}

fn open_new(path: &Path, mode: u32) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    options.open(path)
}

/// Writes a file whole: into a new file beside it with `mode`, synced, then renamed
/// over `path`. With `replace` false an existing file is never replaced. A failure
/// leaves no partial file.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32, replace: bool) -> io::Result<()> {
    if !replace && std::fs::symlink_metadata(path).is_ok() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} already exists", path.display())));
    }
    let pending = pending_name(path);
    let written = (|| {
        let mut file = open_new(&pending, mode)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        // The mode asked for, whatever the umask made of it.
        set_mode(&pending, mode)?;
        if !replace && std::fs::symlink_metadata(path).is_ok() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} already exists", path.display())));
        }
        std::fs::rename(&pending, path)?;
        sync_dir(path.parent());
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&pending);
    }
    written
}

/// Makes a rename durable: the directory that holds it is synced. Unix only.
fn sync_dir(dir: Option<&Path>) {
    #[cfg(unix)]
    if let Some(dir) = dir.filter(|dir| !dir.as_os_str().is_empty()) {
        if let Ok(handle) = std::fs::File::open(dir) {
            let _ = handle.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Everything a command created, so that it can be removed again when the command
/// fails half way. Only what was created is ever removed.
#[derive(Debug, Default)]
pub struct Created {
    paths: Vec<(PathBuf, bool)>,
}

impl Created {
    /// Remembers a file this command made.
    pub fn file(&mut self, path: impl Into<PathBuf>) {
        self.paths.push((path.into(), false));
    }

    /// Remembers a directory this command made. Removing it removes what is in it.
    pub fn dir(&mut self, path: impl Into<PathBuf>) {
        self.paths.push((path.into(), true));
    }

    /// What was created, in order.
    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.paths.iter().map(|(path, _)| path.as_path())
    }

    /// Creates a directory that is not there, with its missing parents, and remembers
    /// each one it made. The last gets `mode`. Says whether it made the last.
    pub fn make_dir(&mut self, path: &Path, mode: u32) -> io::Result<bool> {
        if path.is_dir() {
            return Ok(false);
        }
        let mut missing = Vec::new();
        let mut at = Some(path);
        while let Some(dir) = at.filter(|dir| !dir.as_os_str().is_empty() && !dir.exists()) {
            missing.push(dir.to_path_buf());
            at = dir.parent();
        }
        for dir in missing.iter().rev() {
            std::fs::create_dir(dir)?;
            self.dir(dir.clone());
        }
        set_mode(path, mode)?;
        Ok(true)
    }

    /// Removes what was created, newest first. Errors are ignored: this runs when
    /// something has already gone wrong, and it removes what it can.
    pub fn undo(&mut self) {
        while let Some((path, is_dir)) = self.paths.pop() {
            if is_dir {
                let _ = std::fs::remove_dir_all(&path);
            } else {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    /// Keeps everything: the command succeeded.
    pub fn keep(&mut self) {
        self.paths.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_written_whole_and_replaced_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        write_atomic(&path, b"one\n", 0o600, false).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"one\n");
        let refused = write_atomic(&path, b"two\n", 0o600, false).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path).unwrap(), b"one\n");
        write_atomic(&path, b"two\n", 0o600, true).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two\n");
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["key"], "no file is left behind");
        #[cfg(unix)]
        assert_eq!(mode_of(&path).unwrap(), Some(0o600));
    }

    #[test]
    fn what_was_created_is_removed_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let kept = dir.path().join("kept");
        std::fs::create_dir(&kept).unwrap();
        std::fs::write(kept.join("old"), "old").unwrap();
        let mut created = Created::default();
        assert!(!created.make_dir(&kept, 0o700).unwrap(), "an existing directory is not made");
        let deep = dir.path().join("a").join("b");
        assert!(created.make_dir(&deep, 0o700).unwrap());
        std::fs::write(kept.join("new"), "new").unwrap();
        created.file(kept.join("new"));
        std::fs::write(deep.join("inside"), "x").unwrap();
        created.undo();
        assert!(!dir.path().join("a").exists());
        assert!(!kept.join("new").exists());
        assert_eq!(std::fs::read_to_string(kept.join("old")).unwrap(), "old");
    }
}
