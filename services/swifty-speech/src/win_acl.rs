//! Windows permissions for the token file and the service's data directory.

use std::ffi::{c_void, OsStr};
use std::fs::File;
use std::io::{self, ErrorKind, Write};
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_SUCCESS, GENERIC_ALL, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW,
    GetSecurityInfo, SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    CheckTokenMembership, CreateWellKnownSid, GetAce, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
    GetTokenInformation, IsWellKnownSid, TokenUser, WinBuiltinAdministratorsSid, WinLocalSystemSid, ACCESS_ALLOWED_ACE,
    ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, INHERIT_ONLY_ACE, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateDirectoryW, CreateFileW, CREATE_NEW, DELETE, FILE_APPEND_DATA, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DELETE_CHILD, FILE_WRITE_DATA, WRITE_DAC, WRITE_OWNER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::token::Readers;

/// Owner Administrators; full control for SYSTEM and Administrators; nothing inherited.
const ADMIN_FILE_SDDL: &str = "O:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)";
/// The same for a directory, and inherited by everything created inside it.
const ADMIN_DIRECTORY_SDDL: &str = "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;
const MAX_SID_BYTES: usize = 68;
const TRUSTED_INSTALLER_SID: &str = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";

/// Rights that let a principal replace a file or directory or take it over.
const TAKEOVER_RIGHTS: u32 = DELETE | WRITE_DAC | WRITE_OWNER | GENERIC_WRITE | GENERIC_ALL;
/// Writing, on a file. Adding or removing entries, on a directory.
const CHANGE_RIGHTS: u32 = FILE_WRITE_DATA | FILE_APPEND_DATA | FILE_DELETE_CHILD;
/// What matters on a directory further up the path. Adding new entries does no
/// harm there, and ordinary users may do that at the root of a drive.
const ANCESTOR_RIGHTS: u32 = TAKEOVER_RIGHTS | FILE_DELETE_CHILD;
/// Every entry counts, whatever it grants.
const ANY_RIGHTS: u32 = u32::MAX;

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(Some(0)).collect()
}

/// Whether this process has an elevated administrator token. True for an
/// administrator prompt and for LocalSystem; false for an ordinary user and
/// for an administrator who has not elevated.
pub fn is_elevated() -> bool {
    let mut sid = [0_u8; MAX_SID_BYTES];
    let mut size = MAX_SID_BYTES as u32;
    let mut member = 0;
    // SAFETY: `sid` is writable for `size` bytes, and `member` outlives the call.
    unsafe {
        CreateWellKnownSid(WinBuiltinAdministratorsSid, null_mut(), sid.as_mut_ptr().cast(), &mut size) != 0
            && CheckTokenMembership(null_mut(), sid.as_mut_ptr().cast(), &mut member) != 0
            && member != 0
    }
}

struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    fn from_sddl(sddl: &str) -> io::Result<Self> {
        let text = wide(OsStr::new(sddl));
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: `text` is NUL terminated. On success the descriptor is owned
        // by this value and released in `drop`.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        };
        if converted == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: the pointer came from the conversion function, which
        // documents LocalFree as its release.
        unsafe { LocalFree(self.0) };
    }
}

fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = null_mut();
    // SAFETY: `token` is written on success and closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }

    // u64 storage gives the alignment TOKEN_USER needs.
    let mut buffer = [0_u64; 64];
    let mut needed = 0;
    // SAFETY: `buffer` is writable for the length passed.
    let queried = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast::<c_void>(),
            size_of::<[u64; 64]>() as u32,
            &mut needed,
        )
    };
    let result = if queried == 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: on success the buffer starts with a valid TOKEN_USER.
        let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        sid_to_string(sid)
    };
    // SAFETY: `token` is an open handle owned here.
    unsafe { CloseHandle(token) };
    result
}

fn sid_to_string(sid: PSID) -> io::Result<String> {
    let mut text: *mut u16 = null_mut();
    // SAFETY: `sid` is a valid SID. The returned string is released below.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the string is NUL terminated and stays valid until LocalFree.
    let converted = unsafe {
        let length = (0..).take_while(|&i| *text.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
    };
    // SAFETY: `text` was allocated by ConvertSidToStringSidW.
    unsafe { LocalFree(text.cast()) };
    Ok(converted)
}

/// Create a new file whose permissions are in place from the moment it exists.
/// Fails with `AlreadyExists` rather than touching an existing file.
pub fn create_new_private_file(path: &Path, contents: &[u8], readers: Readers) -> io::Result<()> {
    let sddl = match readers {
        Readers::AdministratorsAndSystem => ADMIN_FILE_SDDL.to_string(),
        Readers::CurrentUserAdministratorsAndSystem => {
            format!("D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;{})", current_user_sid()?)
        }
    };
    write_new_file(path, contents, &sddl)
}

fn write_new_file(path: &Path, contents: &[u8], sddl: &str) -> io::Result<()> {
    let descriptor = SecurityDescriptor::from_sddl(sddl)?;
    let attributes = descriptor.attributes();
    let name = wide(path.as_os_str());
    // SAFETY: `name` is NUL terminated and `attributes` lives across the call.
    let handle = unsafe {
        CreateFileW(name.as_ptr(), GENERIC_WRITE, 0, &attributes, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, null_mut())
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handle` is a valid file handle whose only owner is this `File`.
    let mut file = unsafe { File::from_raw_handle(handle) };
    file.write_all(contents)?;
    file.sync_all()
}

/// Make `directory` private to Administrators and SYSTEM. A missing parent is
/// created the same way. An existing parent may hold other Swifty Home components
/// and is left as it is, but it must not be under anyone else's control, because
/// everything inside it would then be under their control too.
pub fn ensure_private_directory(directory: &Path) -> Result<(), String> {
    let fail = |error: io::Error| format!("cannot prepare {}: {error}", directory.display());

    if let Some(parent) = directory.parent() {
        ensure_protected_parent(parent)?;
    }

    match create_directory(directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            require_plain_directory(directory)?;
            restrict_existing_directory(directory).map_err(fail)
        }
        Err(error) => Err(fail(error)),
    }
}

fn ensure_protected_parent(parent: &Path) -> Result<(), String> {
    match create_directory(parent) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("cannot prepare {}: {error}", parent.display())),
    }
    require_plain_directory(parent)?;
    protection_problem(parent, TAKEOVER_RIGHTS | CHANGE_RIGHTS).map_err(|why| {
        format!(
            "refusing to use {}: {why}. Restrict it to Administrators and SYSTEM, or delete it if nothing needs it.",
            parent.display()
        )
    })
}

/// Refuse a directory, or a parent of it, that anyone but Administrators, SYSTEM
/// or TrustedInstaller can change or add files to, or that is a link. Used before
/// files are taken from the folder the service had under its old name.
pub fn check_trusted_directory(directory: &Path) -> Result<(), String> {
    for path in std::iter::once(directory).chain(directory.parent()) {
        require_plain_directory(path)?;
        protection_problem(path, TAKEOVER_RIGHTS | CHANGE_RIGHTS).map_err(|why| {
            format!(
                "refusing to take files from {}: {why}. Look at what is in it, then restrict it to                  Administrators and SYSTEM, or delete it.",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn require_plain_directory(path: &Path) -> Result<(), String> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|error| format!("cannot prepare {}: {error}", path.display()))?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !metadata.is_dir() {
        return Err(format!("{} exists but is not a plain directory; refusing to use it", path.display()));
    }
    Ok(())
}

fn create_directory(path: &Path) -> io::Result<()> {
    let descriptor = SecurityDescriptor::from_sddl(ADMIN_DIRECTORY_SDDL)?;
    let attributes = descriptor.attributes();
    let name = wide(path.as_os_str());
    // SAFETY: `name` is NUL terminated and `attributes` lives across the call.
    if unsafe { CreateDirectoryW(name.as_ptr(), &attributes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn restrict_existing_directory(directory: &Path) -> io::Result<()> {
    let descriptor = SecurityDescriptor::from_sddl(ADMIN_DIRECTORY_SDDL)?;
    let mut owner: PSID = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let (mut defaulted, mut present) = (0, 0);
    // SAFETY: the descriptor is valid and the outputs outlive the calls. The
    // returned pointers point into the descriptor, which outlives their use.
    unsafe {
        if GetSecurityDescriptorOwner(descriptor.0, &mut owner, &mut defaulted) == 0
            || GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }

    let name = wide(directory.as_os_str());
    // SAFETY: `name` is NUL terminated; `owner` and `dacl` are valid as above.
    let status = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            owner,
            null_mut(),
            dacl,
            null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

/// The owner and access list of one file or directory.
struct ObjectSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
    owner: PSID,
    dacl: *mut ACL,
}

impl ObjectSecurity {
    fn of_path(path: &Path) -> io::Result<Self> {
        let name = wide(path.as_os_str());
        let mut security = Self { descriptor: null_mut(), owner: null_mut(), dacl: null_mut() };
        // SAFETY: `name` is NUL terminated and the outputs outlive the call.
        let status = unsafe {
            GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut security.owner,
                null_mut(),
                &mut security.dacl,
                null_mut(),
                &mut security.descriptor,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(security)
    }

    fn of_file(file: &File) -> io::Result<Self> {
        let mut security = Self { descriptor: null_mut(), owner: null_mut(), dacl: null_mut() };
        // SAFETY: the handle is open for the whole call and the outputs outlive it.
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut security.owner,
                null_mut(),
                &mut security.dacl,
                null_mut(),
                &mut security.descriptor,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(security)
    }

    /// The owner, and everyone granted any of `rights`, must be `permitted`.
    fn check(&self, rights: u32, permitted: &dyn Fn(PSID) -> bool) -> Result<(), String> {
        if !permitted(self.owner) {
            return Err("its owner is someone else".to_string());
        }
        // SAFETY: `dacl` is null or points into the descriptor this value holds.
        unsafe { check_dacl(self.dacl, rights, permitted) }
    }
}

impl Drop for ObjectSecurity {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            // SAFETY: the descriptor came from a call documenting LocalFree as
            // its release, and `owner` and `dacl` are not used after this.
            unsafe { LocalFree(self.descriptor) };
        }
    }
}

/// Refuse a token file that anyone else can get at, such as one an ordinary user
/// planted before the service was installed. Takes the open file, so what is
/// checked is the file that gets read.
pub fn verify_token_file(file: &File, path: &Path, readers: Readers) -> Result<(), String> {
    let (user, allowed) = match readers {
        Readers::AdministratorsAndSystem => (None, "Administrators and SYSTEM"),
        Readers::CurrentUserAdministratorsAndSystem => (
            Some(current_user_sid().map_err(|error| format!("cannot identify the current user: {error}"))?),
            "you, Administrators and SYSTEM",
        ),
    };
    let permitted = |sid: PSID| {
        // SAFETY: `check` passes only SIDs read from a valid descriptor.
        let administrative = unsafe { is_administrators_or_system(sid) };
        administrative || user.as_ref().is_some_and(|user| sid_to_string(sid).is_ok_and(|text| text == *user))
    };

    ObjectSecurity::of_file(file)
        .map_err(|error| format!("its permissions cannot be read: {error}"))
        .and_then(|security| security.check(ANY_RIGHTS, &permitted))
        .map_err(|why| {
            format!(
                "token file {} is not private ({why}). Restrict it to {allowed}, or delete it so that a new one is generated.",
                path.display()
            )
        })
}

/// `path` with every link and junction resolved, in the ordinary drive-letter
/// form rather than the `\\?\` form the system returns.
pub fn real_path(path: &Path) -> Result<PathBuf, String> {
    let resolved =
        std::fs::canonicalize(path).map_err(|error| format!("cannot resolve {}: {error}", path.display()))?;
    Ok(match resolved.to_str().and_then(|text| text.strip_prefix(r"\\?\")) {
        Some(plain) if !plain.starts_with(r"UNC\") => PathBuf::from(plain),
        _ => resolved,
    })
}

/// Refuse to register a service over files that anyone but Administrators, SYSTEM
/// or TrustedInstaller can change, since SYSTEM will run whatever is placed there.
pub fn verify_install_location(executable: &Path, model_files: &[&Path]) -> Result<(), String> {
    let check = |path: &Path, rights: u32| {
        protection_problem(path, rights).map_err(|why| {
            format!(
                "refusing to install: {} can be changed by non-administrators ({why}). The service runs as \
                 SYSTEM, so that would hand SYSTEM to them. Copy the whole folder to a protected location, \
                 such as C:\\Program Files\\Swifty Speech, from an administrator prompt, and run install from there.",
                path.display()
            )
        })
    };

    for file in std::iter::once(executable).chain(model_files.iter().copied()) {
        check(file, TAKEOVER_RIGHTS | CHANGE_RIGHTS)?;
        let mut parents = file.ancestors().skip(1);
        if let Some(folder) = parents.next() {
            check(folder, TAKEOVER_RIGHTS | CHANGE_RIGHTS)?;
        }
        for ancestor in parents {
            check(ancestor, ANCESTOR_RIGHTS)?;
        }
    }
    Ok(())
}

/// Delete whatever is at `path` unless it is a plain file that only trusted
/// accounts can change, so that something planted there earlier cannot redirect
/// or expose what SYSTEM writes. A link is removed, not followed.
pub fn remove_unless_protected(path: &Path) -> Result<(), String> {
    let fail = |error: io::Error| format!("cannot prepare {}: {error}", path.display());
    let attributes = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata.file_attributes(),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(fail(error)),
    };

    let plain_file = attributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY) == 0;
    if plain_file && protection_problem(path, TAKEOVER_RIGHTS | CHANGE_RIGHTS).is_ok() {
        return Ok(());
    }
    let removed = if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    removed.map_err(fail)
}

/// Why `path` is not protected from everyone but the trusted system accounts.
fn protection_problem(path: &Path, rights: u32) -> Result<(), String> {
    ObjectSecurity::of_path(path)
        .map_err(|error| format!("its permissions cannot be read: {error}"))?
        .check(rights, &is_trusted_system_account)
}

fn is_trusted_system_account(sid: PSID) -> bool {
    // SAFETY: `check` passes only SIDs read from a valid descriptor.
    let administrative = unsafe { is_administrators_or_system(sid) };
    administrative || sid_to_string(sid).is_ok_and(|text| text == TRUSTED_INSTALLER_SID)
}

/// # Safety
/// `sid` must point to a valid SID.
unsafe fn is_administrators_or_system(sid: PSID) -> bool {
    IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0 || IsWellKnownSid(sid, WinLocalSystemSid) != 0
}

/// Every entry that grants any of `rights` must be for a `permitted` principal.
/// Entries that only apply to future children do not count.
///
/// # Safety
/// `dacl` must be null or point to a valid ACL.
unsafe fn check_dacl(dacl: *const ACL, rights: u32, permitted: &dyn Fn(PSID) -> bool) -> Result<(), String> {
    if dacl.is_null() {
        return Err("it has no access list".to_string());
    }
    for index in 0..u32::from((*dacl).AceCount) {
        let mut entry: *mut c_void = null_mut();
        if GetAce(dacl, index, &mut entry) == 0 {
            return Err("its access list cannot be read".to_string());
        }
        let header = *entry.cast::<ACE_HEADER>();
        match header.AceType {
            ACCESS_ALLOWED_ACE_TYPE => {
                if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0 {
                    continue;
                }
                let granted = (*entry.cast::<ACCESS_ALLOWED_ACE>()).Mask;
                let sid = std::ptr::addr_of_mut!((*entry.cast::<ACCESS_ALLOWED_ACE>()).SidStart);
                if granted & rights != 0 && !permitted(sid.cast()) {
                    return Err("it grants access to someone else".to_string());
                }
            }
            ACCESS_DENIED_ACE_TYPE => {}
            _ => return Err("it has an access entry of an unexpected type".to_string()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("gs-acl-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// (Protected, [string SIDs]) of the allow entries on `path`.
    fn read_acl(path: &Path) -> (bool, Vec<String>) {
        let name = wide(path.as_os_str());
        let mut dacl: *mut ACL = null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        unsafe {
            let status = GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut descriptor,
            );
            assert_eq!(status, ERROR_SUCCESS);
            let mut sids = Vec::new();
            for index in 0..u32::from((*dacl).AceCount) {
                let mut entry: *mut c_void = null_mut();
                assert_ne!(GetAce(dacl, index, &mut entry), 0);
                assert_eq!((*entry.cast::<ACE_HEADER>()).AceType, ACCESS_ALLOWED_ACE_TYPE);
                let sid = std::ptr::addr_of_mut!((*entry.cast::<ACCESS_ALLOWED_ACE>()).SidStart);
                sids.push(sid_to_string(sid.cast()).unwrap());
            }
            let mut control = 0_u16;
            let mut revision = 0_u32;
            assert_ne!(
                windows_sys::Win32::Security::GetSecurityDescriptorControl(descriptor, &mut control, &mut revision),
                0
            );
            LocalFree(descriptor);
            sids.sort();
            (control & 0x1000 != 0, sids) // SE_DACL_PROTECTED
        }
    }

    /// Give the current user control again, so the test can delete the file.
    fn reopen_for_cleanup(path: &Path) {
        let sddl = format!("D:P(A;;FA;;;{})", current_user_sid().unwrap());
        let descriptor = SecurityDescriptor::from_sddl(&sddl).unwrap();
        let mut dacl: *mut ACL = null_mut();
        let (mut defaulted, mut present) = (0, 0);
        let name = wide(path.as_os_str());
        unsafe {
            assert_ne!(GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted), 0);
            let status = SetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                dacl,
                null(),
            );
            assert_eq!(status, ERROR_SUCCESS);
        }
    }

    #[test]
    fn user_token_file_is_protected_and_lists_exactly_three_principals() {
        let dir = scratch("user");
        let path = dir.join("token");
        create_new_private_file(&path, b"contents", Readers::CurrentUserAdministratorsAndSystem).unwrap();

        let (protected, sids) = read_acl(&path);
        assert!(protected, "inherited entries must be blocked");
        let mut expected = vec!["S-1-5-18".to_string(), "S-1-5-32-544".to_string(), current_user_sid().unwrap()];
        expected.sort();
        assert_eq!(sids, expected);
        assert_eq!(fs::read(&path).unwrap(), b"contents");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn admin_only_file_blocks_an_ordinary_user() {
        let dir = scratch("admin");
        let path = dir.join("token");
        // ADMIN_FILE_SDDL also sets the owner, which needs elevation: this test
        // has to work without it, so it applies the same access list only.
        let sddl = ADMIN_FILE_SDDL.strip_prefix("O:BA").unwrap();
        write_new_file(&path, b"contents", sddl).unwrap();

        let (protected, sids) = read_acl(&path);
        assert!(protected);
        assert_eq!(sids, vec!["S-1-5-18".to_string(), "S-1-5-32-544".to_string()]);
        if !is_elevated() {
            assert_eq!(fs::read(&path).unwrap_err().kind(), ErrorKind::PermissionDenied);
        }

        reopen_for_cleanup(&path);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_to_replace_an_existing_file() {
        let dir = scratch("exists");
        let path = dir.join("token");
        fs::write(&path, b"original").unwrap();
        let error = create_new_private_file(&path, b"new", Readers::CurrentUserAdministratorsAndSystem).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"original");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dacl_check_tells_admin_only_files_from_others() {
        let dir = scratch("verify");
        let admin = dir.join("admin");
        write_new_file(&admin, b"x", ADMIN_FILE_SDDL.strip_prefix("O:BA").unwrap()).unwrap();
        let user = dir.join("user");
        create_new_private_file(&user, b"x", Readers::CurrentUserAdministratorsAndSystem).unwrap();

        let check = |path: &Path| {
            let security = ObjectSecurity::of_path(path).unwrap();
            unsafe { check_dacl(security.dacl, ANY_RIGHTS, &|sid| is_administrators_or_system(sid)) }
        };
        assert!(check(&admin).is_ok());
        assert_eq!(check(&user).unwrap_err(), "it grants access to someone else");

        reopen_for_cleanup(&admin);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn verification_refuses_a_file_owned_by_an_ordinary_user() {
        if is_elevated() {
            return;
        }
        let dir = scratch("owner");
        let path = dir.join("token");
        fs::write(&path, b"planted").unwrap();
        let file = File::open(&path).unwrap();
        let message = verify_token_file(&file, &path, Readers::AdministratorsAndSystem).unwrap_err();
        assert!(message.contains("owner"), "{message}");
        assert!(!message.contains("planted"));
        drop(file);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_custom_token_file_may_be_read_by_its_user_and_nobody_else() {
        let dir = scratch("custom");
        let user = current_user_sid().unwrap();
        let readers = Readers::CurrentUserAdministratorsAndSystem;

        let private = dir.join("private");
        write_new_file(&private, b"x", &format!("D:P(A;;FA;;;{user})")).unwrap();
        assert!(verify_token_file(&File::open(&private).unwrap(), &private, readers).is_ok());
        let strict = Readers::AdministratorsAndSystem;
        let message = verify_token_file(&File::open(&private).unwrap(), &private, strict).unwrap_err();
        assert!(message.contains("not private"), "{message}");

        let shared = dir.join("shared");
        write_new_file(&shared, b"SECRETVALUE", &format!("D:P(A;;FA;;;{user})(A;;FR;;;WD)")).unwrap();
        let message = verify_token_file(&File::open(&shared).unwrap(), &shared, readers).unwrap_err();
        assert!(message.contains("someone else") && message.contains("shared"), "{message}");
        assert!(!message.contains("SECRETVALUE"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn system_files_count_as_a_protected_install_location() {
        let system = Path::new(r"C:\Windows\System32");
        let (executable, library) = (system.join("cmd.exe"), system.join("kernel32.dll"));
        verify_install_location(&executable, &[library.as_path()]).unwrap();
    }

    #[test]
    fn a_folder_ordinary_users_can_change_is_not_an_install_location() {
        let dir = scratch("install");
        let executable = dir.join("swifty-speech.exe");
        fs::write(&executable, b"x").unwrap();
        let system_file = Path::new(r"C:\Windows\System32\kernel32.dll");
        let message = verify_install_location(&executable, &[system_file]).unwrap_err();
        assert!(message.contains("refusing to install") && message.contains("swifty-speech.exe"), "{message}");

        let protected = Path::new(r"C:\Windows\System32\cmd.exe");
        let message = verify_install_location(protected, &[executable.as_path()]).unwrap_err();
        assert!(message.contains("swifty-speech.exe"), "{message}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn entries_for_future_children_only_do_not_count() {
        let dir = scratch("inherit");
        let folder = dir.join("folder");
        let descriptor = SecurityDescriptor::from_sddl("D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;OICIIO;FA;;;AU)").unwrap();
        let attributes = descriptor.attributes();
        let name = wide(folder.as_os_str());
        assert_ne!(unsafe { CreateDirectoryW(name.as_ptr(), &attributes) }, 0);

        let security = ObjectSecurity::of_path(&folder).unwrap();
        let outcome = unsafe {
            check_dacl(security.dacl, TAKEOVER_RIGHTS | CHANGE_RIGHTS, &|sid| is_trusted_system_account(sid))
        };
        assert_eq!(outcome, Ok(()));
        drop(security);

        reopen_for_cleanup(&folder);
        fs::remove_dir_all(&dir).unwrap();
    }

    fn make_junction(link: &Path, target: &Path) {
        let status = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn a_parent_directory_under_someone_elses_control_is_refused() {
        if is_elevated() {
            return;
        }
        let dir = scratch("parent");
        let parent = dir.join("SwiftyHome");
        fs::create_dir(&parent).unwrap();
        let child = parent.join("speech");
        let message = ensure_private_directory(&child).unwrap_err();
        assert!(message.contains("refusing to use") && message.contains("SwiftyHome"), "{message}");
        assert!(!child.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_junction_as_the_parent_directory_is_refused() {
        let dir = scratch("junction");
        let target = dir.join("elsewhere");
        fs::create_dir(&target).unwrap();
        let parent = dir.join("SwiftyHome");
        make_junction(&parent, &target);

        let message = ensure_private_directory(&parent.join("speech")).unwrap_err();
        assert!(message.contains("not a plain directory"), "{message}");
        assert!(!target.join("speech").exists());
        fs::remove_dir(&parent).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_old_folder_ordinary_users_can_change_is_not_taken_from() {
        if is_elevated() {
            return;
        }
        let dir = scratch("old-open");
        let old = dir.join("Glimdock").join("speech");
        fs::create_dir_all(&old).unwrap();
        let message = check_trusted_directory(&old).unwrap_err();
        assert!(message.contains("refusing to take files from"), "{message}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_old_folder_that_is_a_junction_is_not_taken_from() {
        let dir = scratch("old-junction");
        let target = dir.join("elsewhere");
        fs::create_dir(&target).unwrap();
        let parent = dir.join("Glimdock");
        fs::create_dir(&parent).unwrap();
        let old = parent.join("speech");
        make_junction(&old, &target);
        let message = check_trusted_directory(&old).unwrap_err();
        assert!(message.contains("not a plain directory"), "{message}");
        fs::remove_dir(&old).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_protected_old_folder_is_taken_from() {
        // A folder owned by Administrators can only be made from an elevated prompt.
        if !is_elevated() {
            return;
        }
        let dir = scratch("old-protected");
        let mut folders = Vec::new();
        for name in ["Glimdock", r"Glimdock\speech"] {
            let folder = dir.join(name);
            let descriptor = SecurityDescriptor::from_sddl("D:P(A;;FA;;;SY)(A;;FA;;;BA)").unwrap();
            let attributes = descriptor.attributes();
            let wide_name = wide(folder.as_os_str());
            assert_ne!(unsafe { CreateDirectoryW(wide_name.as_ptr(), &attributes) }, 0);
            folders.push(folder);
        }
        check_trusted_directory(&folders[1]).unwrap();
        for folder in folders.iter().rev() {
            reopen_for_cleanup(folder);
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_planted_log_is_removed_and_a_link_is_not_followed() {
        let dir = scratch("log");
        let planted = dir.join("speech.log");
        fs::write(&planted, b"planted").unwrap();
        remove_unless_protected(&planted).unwrap();
        assert!(!planted.exists());

        let target = dir.join("elsewhere");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep"), b"keep").unwrap();
        make_junction(&planted, &target);
        remove_unless_protected(&planted).unwrap();
        assert!(fs::symlink_metadata(&planted).is_err());
        assert_eq!(fs::read(target.join("keep")).unwrap(), b"keep");

        remove_unless_protected(&planted).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }
}
