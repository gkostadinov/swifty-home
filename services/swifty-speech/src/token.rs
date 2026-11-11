//! The bearer token: generation, storage and comparison.
//!
//! The token value never leaves this module except through `Token::matches`.
//! `Token` has no `Debug` or `Display`, and every message names the file only.

use std::fs::File;
use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};

use subtle::ConstantTimeEq;

/// 256 bits, hex encoded.
const TOKEN_BYTES: usize = 32;
pub const MIN_TOKEN_CHARS: usize = TOKEN_BYTES * 2;
const MAX_TOKEN_CHARS: usize = 4096;

pub struct Token(Box<[u8]>);

impl Token {
    /// Constant-time comparison. A length difference is visible, but the
    /// length of the token is not a secret.
    pub fn matches(&self, presented: &[u8]) -> bool {
        self.0.as_ref().ct_eq(presented).into()
    }
}

/// Who may read a token file created by this program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readers {
    AdministratorsAndSystem,
    CurrentUserAdministratorsAndSystem,
}

/// Extract the credentials from an `Authorization` header value.
pub fn bearer_credentials(header: &[u8]) -> Option<&[u8]> {
    let (scheme, rest) = header.split_at_checked(6)?;
    if !scheme.eq_ignore_ascii_case(b"Bearer") || rest.first() != Some(&b' ') {
        return None;
    }
    Some(rest.trim_ascii())
}

/// The token file in use, its path, and its value. `custom_readers` says who may
/// have access to a file given with `--token-file`; the default location is
/// always restricted to Administrators and SYSTEM.
pub fn resolve(custom_path: Option<&Path>, custom_readers: Readers) -> Result<(PathBuf, Token), String> {
    match custom_path {
        Some(path) => {
            let token = load_or_create(path, custom_readers)?;
            Ok((path.to_path_buf(), token))
        }
        None => default_location(),
    }
}

/// `%ProgramData%\SwiftyHome\speech`, the directory for the token and service log.
#[cfg(windows)]
pub fn data_directory() -> PathBuf {
    program_data().join("SwiftyHome").join("speech")
}

/// `%ProgramData%\Glimdock\speech`, where the service kept them under its old
/// name. `install` moves them from there.
#[cfg(windows)]
pub fn old_data_directory() -> PathBuf {
    program_data().join("Glimdock").join("speech")
}

#[cfg(windows)]
fn program_data() -> PathBuf {
    PathBuf::from(std::env::var_os("ProgramData").unwrap_or_else(|| r"C:\ProgramData".into()))
}

#[cfg(windows)]
fn default_location() -> Result<(PathBuf, Token), String> {
    use crate::win_acl;

    let directory = data_directory();
    let path = directory.join(crate::install::TOKEN_FILE);
    if !win_acl::is_elevated() {
        return Err(format!(
            "the default token file {} is restricted to Administrators and SYSTEM. \
             Run this from an administrator prompt, or pass --token-file PATH to use a file of your own.",
            path.display()
        ));
    }
    win_acl::ensure_private_directory(&directory)?;
    let token = load_or_create(&path, Readers::AdministratorsAndSystem)?;
    Ok((path, token))
}

#[cfg(not(windows))]
fn default_location() -> Result<(PathBuf, Token), String> {
    Err("this platform has no default token location; pass --token-file PATH".to_string())
}

/// Read the token file, generating it first if it does not exist.
pub fn load_or_create(path: &Path, readers: Readers) -> Result<Token, String> {
    if let Some(contents) = read_if_present(path, readers)? {
        return parse(&contents, path);
    }

    let mut random = [0_u8; TOKEN_BYTES];
    getrandom::fill(&mut random).map_err(|_| "the operating system did not provide random bytes".to_string())?;
    let hex = encode_hex(&random);

    match create_new(path, hex.as_bytes(), readers) {
        Ok(()) => parse(hex.as_bytes(), path),
        // Lost a race with another process: its file is the one to use.
        Err(error) if error.kind() == ErrorKind::AlreadyExists => match read_if_present(path, readers)? {
            Some(contents) => parse(&contents, path),
            None => Err(format!("token file {} disappeared while it was being read", path.display())),
        },
        Err(error) => Err(format!("cannot create token file {}: {error}", path.display())),
    }
}

fn read_if_present(path: &Path, readers: Readers) -> Result<Option<Vec<u8>>, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) if error.kind() == ErrorKind::PermissionDenied => {
            return Err(format!(
                "cannot read token file {}: access denied. Run from an administrator prompt, \
                 or pass --token-file PATH to use a file of your own.",
                path.display()
            ));
        }
        Err(error) => return Err(format!("cannot read token file {}: {error}", path.display())),
    };
    // On the handle being read, so the permissions checked are this file's.
    verify_readers(&file, path, readers)?;

    let mut contents = Vec::new();
    file.take(MAX_TOKEN_CHARS as u64 + 64)
        .read_to_end(&mut contents)
        .map_err(|error| format!("cannot read token file {}: {error}", path.display()))?;
    Ok(Some(contents))
}

fn parse(contents: &[u8], path: &Path) -> Result<Token, String> {
    let text = contents.trim_ascii();
    if text.is_empty() {
        return Err(format!("token file {} is empty", path.display()));
    }
    if text.len() < MIN_TOKEN_CHARS {
        return Err(format!(
            "token file {} is too short: a token needs at least {MIN_TOKEN_CHARS} characters",
            path.display()
        ));
    }
    if text.len() > MAX_TOKEN_CHARS {
        return Err(format!("token file {} is too long", path.display()));
    }
    if !text.iter().all(u8::is_ascii_graphic) {
        return Err(format!("token file {} contains characters that cannot appear in a header", path.display()));
    }
    Ok(Token(text.into()))
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    hex
}

#[cfg(windows)]
fn verify_readers(file: &File, path: &Path, readers: Readers) -> Result<(), String> {
    crate::win_acl::verify_token_file(file, path, readers)
}

#[cfg(not(windows))]
fn verify_readers(_file: &File, _path: &Path, _readers: Readers) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
pub(crate) fn create_new(path: &Path, contents: &[u8], readers: Readers) -> io::Result<()> {
    crate::win_acl::create_new_private_file(path, contents, readers)
}

#[cfg(unix)]
pub(crate) fn create_new(path: &Path, contents: &[u8], _readers: Readers) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gs-token-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn valid() -> String {
        "a1".repeat(32)
    }

    #[test]
    fn hex_encoding() {
        assert_eq!(encode_hex(&[0x00, 0x0F, 0xA5, 0xFF]), "000fa5ff");
    }

    #[test]
    fn generates_a_file_with_256_bits_of_hex_and_reuses_it() {
        let dir = scratch("generate");
        let path = dir.join("token");
        let readers = Readers::CurrentUserAdministratorsAndSystem;

        let token = load_or_create(&path, readers).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.len(), 64);
        assert!(text.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert!(token.matches(text.as_bytes()));

        let again = load_or_create(&path, readers).unwrap();
        assert!(again.matches(text.as_bytes()));
        assert_eq!(fs::read_to_string(&path).unwrap(), text);

        let other = dir.join("other");
        load_or_create(&other, readers).unwrap();
        assert_ne!(fs::read_to_string(&other).unwrap(), text);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_empty_or_short_or_binary_files() {
        let path = Path::new("token");
        assert!(parse(b"", path).err().unwrap().contains("empty"));
        assert!(parse(b" \r\n\t", path).err().unwrap().contains("empty"));
        assert!(parse(b"short", path).err().unwrap().contains("too short"));
        assert!(parse("a".repeat(63).as_bytes(), path).err().unwrap().contains("too short"));
        assert!(parse(format!("{} x", valid()).as_bytes(), path).err().unwrap().contains("characters"));
        assert!(parse("a".repeat(MAX_TOKEN_CHARS + 1).as_bytes(), path).err().unwrap().contains("too long"));
    }

    #[test]
    fn error_messages_do_not_contain_file_contents() {
        let short = "SECRETVALUE";
        let message = parse(short.as_bytes(), Path::new("token")).err().unwrap();
        assert!(!message.contains(short));
        let bad = format!("{} SECRETVALUE", valid());
        let message = parse(bad.as_bytes(), Path::new("token")).err().unwrap();
        assert!(!message.contains("SECRETVALUE"));
    }

    #[test]
    fn accepts_a_trailing_newline() {
        let token = parse(format!("{}\r\n", valid()).as_bytes(), Path::new("token")).unwrap();
        assert!(token.matches(valid().as_bytes()));
    }

    #[test]
    fn matching_requires_the_exact_bytes() {
        let token = parse(valid().as_bytes(), Path::new("token")).unwrap();
        let mut wrong_last = valid().into_bytes();
        *wrong_last.last_mut().unwrap() = b'2';
        assert!(token.matches(valid().as_bytes()));
        assert!(!token.matches(&wrong_last));
        assert!(!token.matches(b""));
        assert!(!token.matches(&valid().as_bytes()[..63]));
        assert!(!token.matches(format!("{}0", valid()).as_bytes()));
    }

    #[test]
    fn parses_bearer_headers() {
        assert_eq!(bearer_credentials(b"Bearer abc"), Some(&b"abc"[..]));
        assert_eq!(bearer_credentials(b"bearer abc"), Some(&b"abc"[..]));
        assert_eq!(bearer_credentials(b"BEARER   abc  "), Some(&b"abc"[..]));
        assert_eq!(bearer_credentials(b"Bearer"), None);
        assert_eq!(bearer_credentials(b"Bearerabc"), None);
        assert_eq!(bearer_credentials(b"Basic abc"), None);
        assert_eq!(bearer_credentials(b""), None);
    }
}
