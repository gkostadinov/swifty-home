//! The secret store: where plugin credentials are kept.
//!
//! One value per slot, at `<secrets_dir>/<instance>/<slot>`: a plain file of mode
//! 0600 in directories of mode 0700. Plain files, because encrypting them with a
//! key that sits beside the data would add nothing. Values are written by setup
//! flows and `swifty plugin secret set`, and never go into the database, the
//! TOML, the environment, a log, an API response or the site file.
//!
//! A slot name is `[a-z0-9_]{1,32}`, with one optional `/<row id>` part for a
//! credential per row (`local_key/<socket id>`). An instance id is
//! `[a-z][a-z0-9]{0,15}`. Anything else is refused before a path is built, so no
//! name can reach outside the store.
//!
//! Which slots hold a value can be listed without reading any value, so a dry-run
//! hub can still say what an instance needs. Reading a value takes a
//! [`LiveGrant`], which a dry-run process does not have:
//!
//! ```compile_fail
//! use swifty_hub::secrets::{MemorySecretStore, SecretStore};
//!
//! let store = MemorySecretStore::default();
//! // No grant, so this does not build.
//! let value = store.read("hue", "app_key");
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use swifty_net::LiveGrant;
use swifty_plugin_api::Secret;

/// The largest value a slot holds. A CA chain fits many times over.
pub const MAX_VALUE_BYTES: usize = 256 * 1024;
const MAX_SLOT: usize = 32;
const MAX_ROW_ID: usize = 64;
const MAX_INSTANCE: usize = 16;

/// Where plugin credentials are kept.
pub trait SecretStore: Send + Sync {
    /// Which slots hold a value. Reads no value.
    fn slots(&self, instance: &str) -> io::Result<BTreeSet<String>>;
    /// Reading a value takes a grant, so a dry-run hub cannot.
    fn read(&self, grant: LiveGrant, instance: &str, slot: &str) -> io::Result<Option<Secret>>;
    /// Atomic: a temporary file, mode 0600, then a rename.
    fn write(&self, instance: &str, slot: &str, value: &Secret) -> io::Result<()>;
    /// Empties a slot. A slot that holds nothing is not an error.
    fn remove(&self, instance: &str, slot: &str) -> io::Result<()>;
    /// Empties every slot of an instance.
    fn remove_instance(&self, instance: &str) -> io::Result<()>;
}

/// `[a-z][a-z0-9]{0,15}`: what an instance id may be.
pub fn is_instance_id(text: &str) -> bool {
    text.len() <= MAX_INSTANCE
        && text.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// `[a-z0-9_]{1,32}`, with one optional `/<row id>` part, where a row id is
/// `[a-z0-9][a-z0-9_-]{0,63}`.
pub fn is_slot_name(text: &str) -> bool {
    match text.split_once('/') {
        Some((base, row)) => is_slot_base(base) && is_row_id(row),
        None => is_slot_base(text),
    }
}

fn is_slot_base(text: &str) -> bool {
    (1..=MAX_SLOT).contains(&text.len())
        && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn is_row_id(text: &str) -> bool {
    text.len() <= MAX_ROW_ID
        && text.as_bytes().first().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && text.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn refused(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}

fn check_instance(instance: &str) -> io::Result<()> {
    if is_instance_id(instance) {
        Ok(())
    } else {
        Err(refused("not an instance id"))
    }
}

fn check_names(instance: &str, slot: &str) -> io::Result<()> {
    check_instance(instance)?;
    if is_slot_name(slot) {
        Ok(())
    } else {
        Err(refused("not a secret slot name"))
    }
}

fn check_value(value: &Secret) -> io::Result<()> {
    let length = value.expose().len();
    if length == 0 {
        return Err(refused("an empty value is not stored; remove the slot instead"));
    }
    if length > MAX_VALUE_BYTES {
        return Err(refused("the value is too long for a secret slot"));
    }
    Ok(())
}

/// The store on disk.
pub struct FileSecretStore {
    root: PathBuf,
}

impl fmt::Debug for FileSecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileSecretStore").field("root", &self.root).finish()
    }
}

impl FileSecretStore {
    /// A store under `root`, which is created (mode 0700) on the first write.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn instance_dir(&self, instance: &str) -> io::Result<PathBuf> {
        check_instance(instance)?;
        Ok(self.root.join(instance))
    }

    /// The file of a slot. The names are checked first, so each part is one plain
    /// path component.
    fn slot_path(&self, instance: &str, slot: &str) -> io::Result<PathBuf> {
        check_names(instance, slot)?;
        let mut path = self.root.join(instance);
        path.extend(slot.split('/'));
        Ok(path)
    }
}

/// Makes sure `path` is a directory of mode 0700, creating it (and its missing
/// parents) if needed. A symbolic link or a file in its place is refused.
fn private_dir(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => restrict_dir(path, &metadata),
        Ok(_) => Err(io::Error::new(io::ErrorKind::AlreadyExists, "the secret store's place holds something else")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path)?;
            let metadata = fs::symlink_metadata(path)?;
            restrict_dir(path, &metadata)
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn restrict_dir(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o777 != 0o700 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_dir(_path: &Path, _metadata: &fs::Metadata) -> io::Result<()> {
    Ok(())
}

/// Whether a slot file is one this store wrote: a plain file, readable by its owner only.
#[cfg(unix)]
fn check_private_file(metadata: &fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "a secret slot is readable by group or others"));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_private_file(_metadata: &fs::Metadata) -> io::Result<()> {
    Ok(())
}

/// A plain file with something in it, without opening it.
fn holds_value(entry: &fs::DirEntry) -> io::Result<bool> {
    Ok(entry.file_type()?.is_file() && entry.metadata()?.len() > 0)
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

impl SecretStore for FileSecretStore {
    fn slots(&self, instance: &str) -> io::Result<BTreeSet<String>> {
        let dir = self.instance_dir(instance)?;
        let mut found = BTreeSet::new();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(found),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
            if !is_slot_base(&name) {
                // A temporary file of a write in progress, or something nobody put here.
                continue;
            }
            if entry.file_type()?.is_dir() {
                for row in fs::read_dir(entry.path())? {
                    let row = row?;
                    let Some(id) = row.file_name().to_str().map(str::to_string) else { continue };
                    if is_row_id(&id) && holds_value(&row)? {
                        found.insert(format!("{name}/{id}"));
                    }
                }
            } else if holds_value(&entry)? {
                found.insert(name);
            }
        }
        Ok(found)
    }

    fn read(&self, _grant: LiveGrant, instance: &str, slot: &str) -> io::Result<Option<Secret>> {
        let path = self.slot_path(instance, slot)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => return Err(io::Error::new(io::ErrorKind::InvalidData, "a secret slot is not a plain file")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
        let file = File::open(&path)?;
        let metadata = file.metadata()?;
        check_private_file(&metadata)?;
        if metadata.len() > MAX_VALUE_BYTES as u64 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "a secret slot holds more than a secret"));
        }
        let mut text = String::new();
        file.take(MAX_VALUE_BYTES as u64 + 1).read_to_string(&mut text)?;
        if text.is_empty() {
            return Ok(None);
        }
        Ok(Some(text.into()))
    }

    fn write(&self, instance: &str, slot: &str, value: &Secret) -> io::Result<()> {
        let path = self.slot_path(instance, slot)?;
        check_value(value)?;
        private_dir(&self.root)?;
        let dir = path.parent().expect("a slot path has a parent").to_path_buf();
        private_dir(&self.root.join(instance))?;
        private_dir(&dir)?;
        let name = path.file_name().and_then(|name| name.to_str()).expect("a slot name is plain text");
        let temporary = dir.join(format!(".{name}.{}.tmp", crate::crypto::random_id()));
        let written = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(value.expose().as_bytes())?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, &path)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        written?;
        sync_dir(&dir)
    }

    fn remove(&self, instance: &str, slot: &str) -> io::Result<()> {
        let path = self.slot_path(instance, slot)?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
        if slot.contains('/') {
            // The row's directory goes once its last row has. One still in use stays.
            if let Some(dir) = path.parent() {
                let _ = fs::remove_dir(dir);
            }
        }
        Ok(())
    }

    fn remove_instance(&self, instance: &str) -> io::Result<()> {
        let dir = self.instance_dir(instance)?;
        match fs::remove_dir_all(&dir) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

/// The store in memory, for tests and for a hub that must not touch the disk.
#[derive(Default)]
pub struct MemorySecretStore {
    values: Mutex<BTreeMap<String, BTreeMap<String, Secret>>>,
}

/// Names the slots, never the values.
impl fmt::Debug for MemorySecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let values = self.values.lock().unwrap_or_else(PoisonError::into_inner);
        let slots: BTreeMap<&String, Vec<&String>> =
            values.iter().map(|(instance, slots)| (instance, slots.keys().collect())).collect();
        f.debug_struct("MemorySecretStore").field("slots", &slots).finish()
    }
}

impl SecretStore for MemorySecretStore {
    fn slots(&self, instance: &str) -> io::Result<BTreeSet<String>> {
        check_instance(instance)?;
        let values = self.values.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(values.get(instance).map(|slots| slots.keys().cloned().collect()).unwrap_or_default())
    }

    fn read(&self, _grant: LiveGrant, instance: &str, slot: &str) -> io::Result<Option<Secret>> {
        check_names(instance, slot)?;
        let values = self.values.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(values.get(instance).and_then(|slots| slots.get(slot)).cloned())
    }

    fn write(&self, instance: &str, slot: &str, value: &Secret) -> io::Result<()> {
        check_names(instance, slot)?;
        check_value(value)?;
        let mut values = self.values.lock().unwrap_or_else(PoisonError::into_inner);
        values.entry(instance.to_string()).or_default().insert(slot.to_string(), value.clone());
        Ok(())
    }

    fn remove(&self, instance: &str, slot: &str) -> io::Result<()> {
        check_names(instance, slot)?;
        let mut values = self.values.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(slots) = values.get_mut(instance) {
            slots.remove(slot);
            if slots.is_empty() {
                values.remove(instance);
            }
        }
        Ok(())
    }

    fn remove_instance(&self, instance: &str) -> io::Result<()> {
        check_instance(instance)?;
        self.values.lock().unwrap_or_else(PoisonError::into_inner).remove(instance);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_checked_before_any_path_is_built() {
        for good in ["hue", "hue2", "a", "glimdock", "abcdefghijklmnop"] {
            assert!(is_instance_id(good), "{good}");
        }
        for bad in ["", "Hue", "2hue", "hue-2", "hue_2", "..", "a/b", "abcdefghijklmnopq"] {
            assert!(!is_instance_id(bad), "{bad}");
        }
        for good in ["app_key", "token", "ca", "local_key/bf0123456789abcdef", "login", "k/0", "k/a-b_c"] {
            assert!(is_slot_name(good), "{good}");
        }
        for bad in [
            "",
            "App_key",
            "app-key",
            "../token",
            "local_key/",
            "/token",
            "local_key/../x",
            "local_key/a/b",
            "local_key/-a",
            "local_key/A",
            "a.b",
            &"a".repeat(33),
            &format!("k/{}", "a".repeat(65)),
        ] {
            assert!(!is_slot_name(bad), "{bad}");
        }
    }
}
