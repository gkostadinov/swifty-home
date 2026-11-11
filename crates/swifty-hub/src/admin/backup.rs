//! `swifty backup`: everything a hub knows, copied while it runs.
//!
//! Each backup is a directory of its own under `--out`, named for when it was taken
//! (`swifty-home-20261110T043012Z`), mode 0700, holding:
//!
//! - `home.db`: the database, by SQLite's online backup, so it is consistent even while
//!   the hub writes. It is a plain single file (no write-ahead log beside it).
//! - `config/`: `swifty.toml` and the key files it names (audit key, speech token,
//!   assistant key), and any further Proxmox host's files in `config/hosts/`.
//! - `secrets.tar`: the secret store as a tar archive, mode 0600. Unpack it into the
//!   secrets directory to restore it: `tar -xf secrets.tar -C /var/lib/swifty-home/secrets`.
//!
//! Every file is 0600. The directory is written under a temporary name and renamed
//! when it is complete, so a backup that stopped half way never looks like one.
//! `--keep-days N` then removes the backups in `--out` older than N days, by the time
//! in their names. Nothing else there is ever touched.
//!
//! A backup holds every credential of every integration.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::admin::files;
use crate::audit;
use crate::config::Config;
use crate::inventory;

/// The start of every backup directory's name.
pub const PREFIX: &str = "swifty-home-";
/// The database's name in a backup.
pub const DATABASE: &str = "home.db";
/// The secret store's archive in a backup.
pub const SECRETS_ARCHIVE: &str = "secrets.tar";
/// The configuration and its key files in a backup.
pub const CONFIG_DIR: &str = "config";
const DAY_MS: i64 = 86_400_000;

/// Why a backup failed. Says which file, never what is in it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct BackupError(pub String);

fn io_error(what: &str, path: &Path) -> impl FnOnce(io::Error) -> BackupError {
    let text = format!("cannot {what} {}", path.display());
    move |error| BackupError(format!("{text}: {error}"))
}

/// What to back up, and where.
pub struct BackupOptions<'a> {
    pub config: &'a Config,
    /// The `swifty.toml` the configuration was read from.
    pub config_path: &'a Path,
    pub out: &'a Path,
    pub keep_days: Option<u32>,
    pub now_ms: i64,
}

/// What a backup wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupReport {
    pub dir: PathBuf,
    /// Every file in it, relative to it, with its size.
    pub files: Vec<(String, u64)>,
    /// The audit checkpoint of the copied database, if it has rows.
    pub checkpoint: Option<String>,
    /// How many secret slots the archive holds.
    pub secrets: usize,
    /// Older backups that `--keep-days` removed.
    pub removed: Vec<PathBuf>,
}

/// The days since 1970-01-01 as a civil date (proleptic Gregorian).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The days since 1970-01-01 of a civil date.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let month = i64::from(month);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `20261110T043012Z` for a time in milliseconds since the epoch.
pub fn stamp(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let of_day = secs.rem_euclid(86_400);
    format!("{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z", of_day / 3600, of_day % 3600 / 60, of_day % 60)
}

/// The time a backup's name says it was taken, in ms. `None` for any other name.
pub fn taken_at(name: &str) -> Option<i64> {
    let rest = name.strip_prefix(PREFIX)?;
    let (text, _again) = match rest.split_once('-') {
        Some((text, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => (text, Some(n)),
        Some(_) => return None,
        None => (rest, None),
    };
    let bytes = text.as_bytes();
    if bytes.len() != 16 || bytes[8] != b'T' || bytes[15] != b'Z' {
        return None;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = text.get(range)?;
        part.bytes().all(|b| b.is_ascii_digit()).then(|| part.parse().ok())?
    };
    let (year, month, day) = (digits(0..4)?, digits(4..6)?, digits(6..8)?);
    let (hour, minute, second) = (digits(9..11)?, digits(11..13)?, digits(13..15)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month as u32, day as u32);
    Some((days * 86_400 + hour * 3600 + minute * 60 + second) * 1000)
}

/// Copies a file into the backup, 0600.
fn copy_into(from: &Path, to: &Path) -> Result<(), BackupError> {
    let bytes = std::fs::read(from).map_err(io_error("read", from))?;
    files::write_atomic(to, &bytes, 0o600, false).map_err(io_error("write", to))
}

/// The database, by online backup, as one file with no write-ahead log.
fn copy_database(from: &Path, to: &Path) -> Result<Option<String>, BackupError> {
    if !from.is_file() {
        return Err(BackupError(format!("there is no database at {}", from.display())));
    }
    let failed = |error: rusqlite::Error| BackupError(format!("cannot copy the database {}: {error}", from.display()));
    let source = Connection::open_with_flags(from, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(failed)?;
    source.busy_timeout(Duration::from_secs(5)).map_err(failed)?;
    {
        let mut target = Connection::open(to).map_err(failed)?;
        files::set_mode(to, 0o600).map_err(io_error("protect", to))?;
        {
            let backup = rusqlite::backup::Backup::new(&source, &mut target).map_err(failed)?;
            backup.run_to_completion(512, Duration::from_millis(5), None).map_err(failed)?;
        }
        target.query_row("PRAGMA journal_mode = DELETE", [], |row| row.get::<_, String>(0)).map_err(failed)?;
        let check: String = target.query_row("PRAGMA quick_check", [], |row| row.get(0)).map_err(failed)?;
        if check != "ok" {
            return Err(BackupError(format!("the copy of the database does not check out: {check}")));
        }
        audit::checkpoint(&target).map_err(failed)
    }
}

// ----- the secret store as a tar archive (POSIX ustar)

const BLOCK: usize = 512;

fn octal(field: &mut [u8], value: u64) {
    let width = field.len() - 1;
    let text = format!("{value:0width$o}");
    let text = &text.as_bytes()[text.len().saturating_sub(width)..];
    field[..width].copy_from_slice(text);
    field[width] = 0;
}

/// A ustar header for one entry. A name longer than 100 bytes is split at a `/`
/// into the prefix field.
fn header(path: &str, is_dir: bool, size: u64, mode: u32, owner: (u32, u32), mtime: u64) -> io::Result<[u8; BLOCK]> {
    let mut block = [0u8; BLOCK];
    let (prefix, name) = if path.len() <= 100 {
        ("", path)
    } else {
        let split = path
            .char_indices()
            .filter(|(_, c)| *c == '/')
            .map(|(i, _)| i)
            .find(|&i| i <= 155 && path.len() - i - 1 <= 100)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, format!("{path} is too long for the archive"))
            })?;
        (&path[..split], &path[split + 1..])
    };
    block[..name.len()].copy_from_slice(name.as_bytes());
    octal(&mut block[100..108], u64::from(mode));
    octal(&mut block[108..116], u64::from(owner.0));
    octal(&mut block[116..124], u64::from(owner.1));
    octal(&mut block[124..136], size);
    octal(&mut block[136..148], mtime);
    block[156] = if is_dir { b'5' } else { b'0' };
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    block[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    block[148..156].copy_from_slice(b"        ");
    let sum: u64 = block.iter().map(|&b| u64::from(b)).sum();
    let text = format!("{sum:06o}\0 ");
    block[148..156].copy_from_slice(text.as_bytes());
    Ok(block)
}

/// Every directory and slot file under the secret store's root, by path relative to
/// it, in order. Temporary files of a write in progress and anything that is not a
/// plain file or directory are left out.
fn secret_entries(root: &Path, at: &Path, out: &mut Vec<(String, PathBuf, bool)>) -> io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(at)?.collect::<io::Result<_>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let kind = std::fs::symlink_metadata(&path)?.file_type();
        let relative: Vec<String> = path
            .strip_prefix(root)
            .map_err(io::Error::other)?
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect();
        let relative = relative.join("/");
        if kind.is_dir() {
            out.push((format!("{relative}/"), path.clone(), true));
            secret_entries(root, &path, out)?;
        } else if kind.is_file() {
            out.push((relative, path, false));
        }
    }
    Ok(())
}

/// Writes the secret store as a tar archive, 0600. Returns how many slots it holds.
fn archive_secrets(root: &Path, to: &Path, mtime: u64) -> Result<usize, BackupError> {
    let mut entries = Vec::new();
    if root.is_dir() {
        secret_entries(root, root, &mut entries).map_err(io_error("read", root))?;
    }
    let mut bytes = Vec::new();
    let mut slots = 0;
    for (name, path, is_dir) in &entries {
        let owner = files::owner_of(path).map_err(io_error("read", path))?.unwrap_or((0, 0));
        let content = if *is_dir { Vec::new() } else { std::fs::read(path).map_err(io_error("read", path))? };
        let mode = if *is_dir { 0o700 } else { 0o600 };
        let block =
            header(name, *is_dir, content.len() as u64, mode, owner, mtime).map_err(io_error("archive", path))?;
        bytes.extend_from_slice(&block);
        bytes.extend_from_slice(&content);
        bytes.resize(bytes.len().div_ceil(BLOCK) * BLOCK, 0);
        if !is_dir {
            slots += 1;
        }
    }
    bytes.resize(bytes.len() + 2 * BLOCK, 0);
    files::write_atomic(to, &bytes, 0o600, false).map_err(io_error("write", to))?;
    Ok(slots)
}

/// The further Proxmox hosts' files directory, if an instance names one.
fn host_files_dirs(database: &Path) -> Vec<PathBuf> {
    let Ok(conn) = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return Vec::new();
    };
    let specs = inventory::instance_specs(&conn).unwrap_or_default();
    let dirs: BTreeSet<PathBuf> = specs
        .iter()
        .filter_map(|spec| spec.config.get("host_files_dir").and_then(|dir| dir.as_str()).map(PathBuf::from))
        .collect();
    dirs.into_iter().collect()
}

/// Writes the parts of a backup into `dir`.
fn fill(options: &BackupOptions<'_>, dir: &Path) -> Result<(Option<String>, usize), BackupError> {
    let config = options.config;
    let checkpoint = copy_database(&config.database_path(), &dir.join(DATABASE))?;
    let config_dir = dir.join(CONFIG_DIR);
    std::fs::create_dir(&config_dir).map_err(io_error("create", &config_dir))?;
    files::set_mode(&config_dir, 0o700).map_err(io_error("protect", &config_dir))?;
    copy_into(options.config_path, &config_dir.join("swifty.toml"))?;
    let keys = [
        config.audit.key_file.as_deref(),
        config.speech.as_ref().map(|speech| speech.token_file.as_path()),
        config.assistant.as_ref().map(|assistant| assistant.key_file.as_path()),
    ];
    let mut taken = BTreeSet::from(["swifty.toml".to_string(), "hosts".to_string()]);
    for key in keys.into_iter().flatten() {
        if !key.is_file() {
            continue;
        }
        let name = key.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        if name.is_empty() || !taken.insert(name.clone()) {
            return Err(BackupError(format!("{} has a file name another file of the backup has", key.display())));
        }
        copy_into(key, &config_dir.join(name))?;
    }
    for hosts in host_files_dirs(&config.database_path()) {
        if !hosts.is_dir() {
            continue;
        }
        let into = config_dir.join("hosts");
        if !into.is_dir() {
            std::fs::create_dir(&into).map_err(io_error("create", &into))?;
            files::set_mode(&into, 0o700).map_err(io_error("protect", &into))?;
        }
        for entry in std::fs::read_dir(&hosts).map_err(io_error("read", &hosts))? {
            let entry = entry.map_err(io_error("read", &hosts))?;
            let path = entry.path();
            if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
                copy_into(&path, &into.join(entry.file_name()))?;
            }
        }
    }
    let mtime = u64::try_from(options.now_ms / 1000).unwrap_or(0);
    let slots = archive_secrets(&config.secrets_path(), &dir.join(SECRETS_ARCHIVE), mtime)?;
    Ok((checkpoint, slots))
}

fn listing(dir: &Path, at: &Path, out: &mut Vec<(String, u64)>) -> io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(at)?.collect::<io::Result<_>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            listing(dir, &path, out)?;
        } else {
            let relative: Vec<String> = path
                .strip_prefix(dir)
                .map_err(io::Error::other)?
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect();
            out.push((relative.join("/"), metadata.len()));
        }
    }
    Ok(())
}

/// Takes a backup into a new directory under `out`, then removes backups there
/// older than `keep_days`.
pub fn run(options: &BackupOptions<'_>) -> Result<BackupReport, BackupError> {
    let out = options.out;
    if !out.is_dir() {
        std::fs::create_dir_all(out).map_err(io_error("create", out))?;
        files::set_mode(out, 0o700).map_err(io_error("protect", out))?;
    }
    let base = format!("{PREFIX}{}", stamp(options.now_ms));
    let name = (1..100)
        .map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") })
        .find(|name| std::fs::symlink_metadata(out.join(name)).is_err())
        .ok_or_else(|| BackupError(format!("too many backups in {} for this second", out.display())))?;
    let partial = out.join(format!(".{name}.partial"));
    std::fs::create_dir(&partial).map_err(io_error("create", &partial))?;
    let filled =
        files::set_mode(&partial, 0o700).map_err(io_error("protect", &partial)).and_then(|()| fill(options, &partial));
    let (checkpoint, secrets) = match filled {
        Ok(filled) => filled,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&partial);
            return Err(error);
        }
    };
    let dir = out.join(&name);
    if let Err(error) = std::fs::rename(&partial, &dir) {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(io_error("write", &dir)(error));
    }
    if let Ok(handle) = std::fs::File::open(out) {
        let _ = handle.sync_all();
    }
    let mut written = Vec::new();
    listing(&dir, &dir, &mut written).map_err(io_error("read", &dir))?;

    let mut removed = Vec::new();
    if let Some(days) = options.keep_days {
        let oldest = options.now_ms - i64::from(days) * DAY_MS;
        let mut old: Vec<PathBuf> = Vec::new();
        for entry in std::fs::read_dir(out).map_err(io_error("read", out))? {
            let entry = entry.map_err(io_error("read", out))?;
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = std::fs::symlink_metadata(entry.path()).is_ok_and(|metadata| metadata.is_dir());
            if is_dir && file_name != name && taken_at(&file_name).is_some_and(|at| at < oldest) {
                old.push(entry.path());
            }
        }
        old.sort();
        for path in old {
            std::fs::remove_dir_all(&path).map_err(io_error("remove", &path))?;
            removed.push(path);
        }
    }
    Ok(BackupReport { dir, files: written, checkpoint, secrets, removed })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_backup_is_named_for_when_it_was_taken_and_read_back_from_its_name() {
        // 2026-11-10 04:30:12 UTC.
        let ms = 1_794_285_012_000;
        assert_eq!(stamp(ms), "20261110T043012Z");
        assert_eq!(taken_at("swifty-home-20261110T043012Z"), Some(ms));
        assert_eq!(taken_at("swifty-home-20261110T043012Z-2"), Some(ms));
        assert_eq!(stamp(0), "19700101T000000Z");
        assert_eq!(taken_at("swifty-home-19700101T000000Z"), Some(0));
        assert_eq!(stamp(951_782_400_000), "20000229T000000Z", "a leap day");
        for other in [
            "swifty-home-20261110T043012",
            "swifty-home-2026111OT043012Z",
            "swifty-home-20261310T043012Z",
            "swifty-home-20261110T043012Z-x",
            "glimdock-home-20261110T043012Z",
            ".swifty-home-20261110T043012Z.partial",
            "swifty-home-",
        ] {
            assert_eq!(taken_at(other), None, "{other}");
        }
    }

    #[test]
    fn the_archive_is_plain_ustar() {
        let block = header("hue/app_key", false, 5, 0o600, (1000, 1000), 1_700_000_000).unwrap();
        assert_eq!(&block[..11], b"hue/app_key");
        assert_eq!(&block[257..263], b"ustar\0");
        assert_eq!(&block[124..135], b"00000000005");
        let stored = std::str::from_utf8(&block[148..154]).unwrap();
        let mut copy = block;
        copy[148..156].copy_from_slice(b"        ");
        let sum: u64 = copy.iter().map(|&b| u64::from(b)).sum();
        assert_eq!(u64::from_str_radix(stored, 8).unwrap(), sum);

        let long = format!("tuya/local_key/{}", "a".repeat(90));
        let block = header(&long, false, 1, 0o600, (0, 0), 0).unwrap();
        let name = std::str::from_utf8(&block[..100]).unwrap().trim_end_matches('\0');
        let prefix = std::str::from_utf8(&block[345..500]).unwrap().trim_end_matches('\0');
        assert_eq!(format!("{prefix}/{name}"), long);
    }
}
