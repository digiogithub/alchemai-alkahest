//! Files only their owner may read: the API key store, key usage counters, the audit log and
//! the TLS private key (ALK-US-0084).
//!
//! Unix: created with mode 0600. Windows: created with no sharing (nobody else can open the
//! file while it is being set up) and given a protected DACL with one entry, full control
//! for the account the server runs as, before any byte is written. Inheritance is cut, so a
//! data directory under `C:\ProgramData` (the service's), whose children normally inherit
//! read access for every local user, does not leak these files.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Creates `path`, which must not exist, with owner-only access, open for writing.
pub(crate) fn create_new(path: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_WRITE, WRITE_DAC};
        opts.share_mode(0)
            .access_mode(FILE_GENERIC_WRITE | WRITE_DAC);
    }
    let file = opts.open(path)?;
    #[cfg(windows)]
    if let Err(e) = windows::restrict_to_owner(&file) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(e);
    }
    Ok(file)
}

#[cfg(windows)]
mod windows {
    use std::fs::File;
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW,
        SetSecurityInfo, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GetTokenInformation, NO_INHERITANCE,
        PROTECTED_DACL_SECURITY_INFORMATION, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// Replaces the DACL of `file` with "full control for the current user", protected from
    /// inheritance.
    #[allow(unsafe_code)]
    pub(super) fn restrict_to_owner(file: &File) -> io::Result<()> {
        // SAFETY: every pointer passed below is either null where the API allows it or points
        // to a live, properly sized local buffer: the token handle is owned by `OwnedHandle`
        // (closed once), the TOKEN_USER buffer is 8-byte aligned and as long as the size the
        // first GetTokenInformation call reported, the SID inside it outlives SetEntriesInAclW
        // (which copies it into the new ACL), and the ACL allocated by SetEntriesInAclW is
        // released with LocalFree exactly once. The file handle belongs to `file`.
        unsafe {
            let mut raw_token = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut raw_token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let token = OwnedHandle::from_raw_handle(raw_token);
            let mut len = 0u32;
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                std::ptr::null_mut(),
                0,
                &raw mut len,
            );
            if len == 0 {
                return Err(io::Error::last_os_error());
            }
            let words = usize::try_from(len).map_err(io::Error::other)?.div_ceil(8);
            let mut buf = vec![0u64; words];
            if GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buf.as_mut_ptr().cast(),
                len,
                &raw mut len,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let user = &*buf.as_ptr().cast::<TOKEN_USER>();
            let access = EXPLICIT_ACCESS_W {
                grfAccessPermissions: FILE_ALL_ACCESS,
                grfAccessMode: SET_ACCESS,
                grfInheritance: NO_INHERITANCE,
                Trustee: TRUSTEE_W {
                    pMultipleTrustee: std::ptr::null_mut(),
                    MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                    TrusteeForm: TRUSTEE_IS_SID,
                    TrusteeType: TRUSTEE_IS_USER,
                    ptstrName: user.User.Sid.cast(),
                },
            };
            let mut acl: *mut ACL = std::ptr::null_mut();
            let rc = SetEntriesInAclW(1, &raw const access, std::ptr::null(), &raw mut acl);
            if rc != 0 {
                return Err(io::Error::from_raw_os_error(rc.cast_signed()));
            }
            let rc = SetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl,
                std::ptr::null(),
            );
            LocalFree(acl.cast());
            if rc != 0 {
                return Err(io::Error::from_raw_os_error(rc.cast_signed()));
            }
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn created_files_are_owner_only_and_never_reused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        let mut f = create_new(&path).unwrap();
        f.write_all(b"x").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(
            create_new(&path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
    }
}
