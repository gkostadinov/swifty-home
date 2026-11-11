//! Who the command line works as.
//!
//! Run as root on Unix, it switches to the user and group that own the data
//! directory before it touches the database or writes a file, so it never leaves
//! a file there that the service cannot read or replace. Elsewhere, and for
//! anyone but root, nothing changes.

use std::path::Path;

use crate::cli::CliError;

/// The name of the user running this, for the audit log. `local` where there is
/// no such name.
pub fn user_name() -> String {
    #[cfg(unix)]
    {
        unix::user_name(unix::effective_uid()).unwrap_or_else(|| "local".to_string())
    }
    #[cfg(not(unix))]
    {
        "local".to_string()
    }
}

/// When running as root, becomes the owner of `data_dir`. Returns the name of
/// the user it became, when it changed anything.
pub fn become_owner_of(data_dir: &Path) -> Result<Option<String>, CliError> {
    #[cfg(unix)]
    {
        unix::become_owner_of(data_dir)
    }
    #[cfg(not(unix))]
    {
        let _ = data_dir;
        Ok(None)
    }
}

#[cfg(unix)]
mod unix {
    use std::ffi::CStr;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    // The C name of a user database entry, under a plainer one.
    use libc::passwd as UserEntry;

    use crate::cli::CliError;

    pub fn effective_uid() -> u32 {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    pub fn user_name(uid: u32) -> Option<String> {
        let mut buffer = vec![0 as libc::c_char; 4096];
        // SAFETY: an all-zero entry is a valid value for getpwuid_r to fill in.
        let mut entry: UserEntry = unsafe { std::mem::zeroed() };
        let mut found: *mut UserEntry = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call and the buffer's length is passed with it.
        let status = unsafe { libc::getpwuid_r(uid, &mut entry, buffer.as_mut_ptr(), buffer.len(), &mut found) };
        if status != 0 || found.is_null() || entry.pw_name.is_null() {
            return None;
        }
        // SAFETY: getpwuid_r succeeded, so pw_name points to a NUL-terminated string inside `buffer`.
        let name = unsafe { CStr::from_ptr(entry.pw_name) };
        Some(name.to_string_lossy().into_owned())
    }

    pub fn become_owner_of(data_dir: &Path) -> Result<Option<String>, CliError> {
        if effective_uid() != 0 {
            return Ok(None);
        }
        let metadata = std::fs::metadata(data_dir).map_err(|_| {
            CliError::new(format!("{} does not exist yet.", data_dir.display())).hint(format!(
                "Run as root, swifty works as the owner of the data directory. Create it first:\n  \
                 sudo install -d -m 0700 -o swifty-home -g swifty-home {}\nThe install script does this for you.",
                data_dir.display()
            ))
        })?;
        let (uid, gid) = (metadata.uid(), metadata.gid());
        if uid == 0 {
            return Ok(None);
        }
        let failed = |what: &str| {
            CliError::new(format!("cannot switch to the owner of {} ({what}).", data_dir.display()))
                .hint("Run it as that user instead, for example: sudo -u swifty-home swifty ...")
        };
        // Groups first: once the user id has changed, they can no longer be changed.
        // SAFETY: setgroups reads one gid from a valid pointer; setgid and setuid take plain ids.
        unsafe {
            if libc::setgroups(1, &gid) != 0 {
                return Err(failed("groups"));
            }
            if libc::setgid(gid) != 0 {
                return Err(failed("group"));
            }
            if libc::setuid(uid) != 0 {
                return Err(failed("user"));
            }
            // Root must be gone for good.
            if libc::setuid(0) == 0 || libc::geteuid() != uid || libc::getegid() != gid {
                return Err(failed("check"));
            }
        }
        Ok(Some(user_name(uid).unwrap_or_else(|| uid.to_string())))
    }
}
