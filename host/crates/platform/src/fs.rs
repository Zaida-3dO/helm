//! File-system security primitives, with NTFS equivalents of the POSIX ones:
//!
//! - folders 0700, files 0600 become a protected DACL with one ACE: the current user, full control
//!   (inherited by new children of a folder);
//! - `O_NOFOLLOW` becomes refusing any path component that is a reparse point;
//! - rename then fsync the parent becomes `MoveFileExW` with replace and write-through, retried
//!   while antivirus briefly holds the file.
//!
//! Linux keeps the POSIX behaviour.

use std::fs::{File, Metadata};
use std::io;
use std::path::{Component, Path};
use std::time::Duration;

/// How many times a replace is retried when another process (usually antivirus) holds the file.
pub const REPLACE_ATTEMPTS: u32 = 10;

/// NUL-terminated UTF-16 for Win32 calls.
#[cfg(windows)]
pub fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// True when the entry itself (not its target) is a link: a symlink, junction or any other
/// reparse point.
pub fn is_link_like(meta: &Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// Refuse paths a strict open refuses before touching the disk: relative paths, `.`,
/// `..`, empty components and NUL.
pub fn check_plain_absolute(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(refused("path is not absolute"));
    }
    if path.as_os_str().to_string_lossy().contains('\0') {
        return Err(refused("path contains NUL"));
    }
    for c in path.components() {
        match c {
            Component::CurDir | Component::ParentDir => {
                return Err(refused("path contains . or .."));
            }
            Component::Normal(n) if n.is_empty() => return Err(refused("empty path component")),
            _ => {}
        }
    }
    Ok(())
}

/// Refuse when the path or any of its ancestors is a link or reparse point.
///
/// This is the stand-in for walking with `openat(O_NOFOLLOW)`: Windows has no
/// descriptor-relative open, so each component is checked with a no-follow stat, and the final
/// open (see [`open_regular_no_follow`]) re-checks the handle it got.
pub fn refuse_links_along(path: &Path) -> io::Result<()> {
    check_plain_absolute(path)?;
    let mut prefix = std::path::PathBuf::new();
    for c in path.components() {
        prefix.push(c.as_os_str());
        if matches!(c, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        let meta = std::fs::symlink_metadata(&prefix)?;
        if is_link_like(&meta) {
            return Err(refused("path goes through a link or reparse point"));
        }
    }
    Ok(())
}

/// Open an existing regular file for reading without following links anywhere along its path.
/// The opened handle is re-checked: it must be a regular file and not a reparse point.
pub fn open_regular_no_follow(path: &Path) -> io::Result<File> {
    refuse_links_along(path)?;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        opts.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NOFOLLOW | O_NONBLOCK on Linux.
        opts.custom_flags(0o400000 | 0o4000);
    }
    let file = opts.open(path)?;
    let meta = file.metadata()?;
    if is_link_like(&meta) || !meta.is_file() {
        return Err(refused("not a regular file"));
    }
    Ok(file)
}

fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, format!("refused: {why}"))
}

/// Make a file or folder private to the current user (0700/0600 on Linux; an owner-only,
/// protected DACL on Windows, inherited by new children of a folder).
pub fn restrict_to_owner(path: &Path) -> io::Result<()> {
    imp::restrict_to_owner(path)
}

/// True when only the current user can reach the path (used by tests and startup checks).
pub fn is_owner_only(path: &Path) -> io::Result<bool> {
    imp::is_owner_only(path)
}

/// Atomically replace `to` with `from` (both on the same volume), durably.
///
/// Windows: `MoveFileExW` with replace and write-through, retried with backoff while another
/// process (antivirus, indexer) briefly holds either file. Linux: `rename`, then fsync the parent.
pub fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    replace_file_with(from, to, imp::replace_once)
}

/// [`replace_file`] with the single attempt injected, so the retry policy can be tested.
pub fn replace_file_with(
    from: &Path,
    to: &Path,
    mut attempt: impl FnMut(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let mut delay = Duration::from_millis(5);
    let mut last = None;
    for _ in 0..REPLACE_ATTEMPTS {
        match attempt(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if is_transient(&e) => {
                last = Some(e);
                std::thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_millis(250));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("replace failed")))
}

/// Errors worth retrying: another process holds the file for a moment.
pub fn is_transient(e: &io::Error) -> bool {
    imp::is_transient(e)
}

#[cfg(windows)]
mod imp {
    use super::wide;
    use std::io;
    use std::path::Path;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{
        CloseHandle, LocalFree, ERROR_ACCESS_DENIED, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION,
        ERROR_SUCCESS, HANDLE,
    };
    use windows_sys::Win32::Security::Authorization::{
        GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W,
        NO_MULTIPLE_TRUSTEE, SET_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_USER,
        TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        AclSizeInformation, EqualSid, GetAce, GetAclInformation, GetSecurityDescriptorControl,
        GetTokenInformation, TokenUser, ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION,
        CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, NO_INHERITANCE, OBJECT_INHERIT_ACE,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED, TOKEN_QUERY,
        TOKEN_USER,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, FILE_ALL_ACCESS, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// The current user's SID, copied out of the process token.
    struct UserSid {
        /// u64 storage keeps the TOKEN_USER suitably aligned; the SID it points to lives inside.
        buf: Vec<u64>,
    }

    impl UserSid {
        fn current() -> io::Result<Self> {
            let mut token: HANDLE = null_mut();
            // SAFETY: plain Win32 calls with valid out-pointers; the handle is closed below.
            unsafe {
                if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                    return Err(io::Error::last_os_error());
                }
                let mut len = 0u32;
                GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len);
                let mut buf = vec![0u64; (len as usize).div_ceil(8)];
                let ok =
                    GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
                CloseHandle(token);
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Self { buf })
            }
        }

        fn psid(&self) -> *mut core::ffi::c_void {
            // SAFETY: buf holds a TOKEN_USER written by GetTokenInformation; its Sid points into
            // the same allocation, which lives as long as self.
            unsafe { (*(self.buf.as_ptr() as *const TOKEN_USER)).User.Sid }
        }
    }

    pub fn restrict_to_owner(path: &Path) -> io::Result<()> {
        let sid = UserSid::current()?;
        let is_dir = std::fs::symlink_metadata(path)?.is_dir();
        let inheritance = if is_dir {
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
        } else {
            NO_INHERITANCE
        };
        let access = EXPLICIT_ACCESS_W {
            grfAccessPermissions: FILE_ALL_ACCESS,
            grfAccessMode: SET_ACCESS,
            grfInheritance: inheritance,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_USER,
                ptstrName: sid.psid().cast(),
            },
        };
        let mut acl: *mut ACL = null_mut();
        // SAFETY: one valid EXPLICIT_ACCESS_W; the new ACL is freed with LocalFree.
        unsafe {
            let rc = SetEntriesInAclW(1, &access, null_mut(), &mut acl);
            if rc != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(rc as i32));
            }
            let name = wide(path);
            let rc = SetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                acl,
                null_mut(),
            );
            LocalFree(acl.cast());
            if rc != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(rc as i32));
            }
        }
        Ok(())
    }

    pub fn is_owner_only(path: &Path) -> io::Result<bool> {
        let sid = UserSid::current()?;
        let name = wide(path);
        let mut dacl: *mut ACL = null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: out-pointers are valid; sd is freed with LocalFree; dacl points into sd.
        unsafe {
            let rc = GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut sd,
            );
            if rc != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(rc as i32));
            }
            let result = check_dacl(sd, dacl, sid.psid());
            LocalFree(sd);
            Ok(result)
        }
    }

    /// One allowed ACE, for `sid`, in a protected DACL.
    unsafe fn check_dacl(
        sd: PSECURITY_DESCRIPTOR,
        dacl: *mut ACL,
        sid: *mut core::ffi::c_void,
    ) -> bool {
        if dacl.is_null() {
            return false; // a NULL DACL grants everyone everything
        }
        let mut control = 0u16;
        let mut revision = 0u32;
        if GetSecurityDescriptorControl(sd, &mut control, &mut revision) == 0
            || control & SE_DACL_PROTECTED == 0
        {
            return false;
        }
        let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
        if GetAclInformation(
            dacl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        ) == 0
        {
            return false;
        }
        if info.AceCount != 1 {
            return false;
        }
        let mut ace: *mut core::ffi::c_void = null_mut();
        if GetAce(dacl, 0, &mut ace) == 0 {
            return false;
        }
        let ace = ace as *const ACCESS_ALLOWED_ACE;
        // ACCESS_ALLOWED_ACE_TYPE is 0.
        if (*ace).Header.AceType != 0 {
            return false;
        }
        let ace_sid = std::ptr::addr_of!((*ace).SidStart) as *mut core::ffi::c_void;
        EqualSid(ace_sid, sid) != 0
    }

    pub fn replace_once(from: &Path, to: &Path) -> io::Result<()> {
        let (f, t) = (wide(from), wide(to));
        // SAFETY: both are NUL-terminated wide strings.
        let ok = unsafe {
            MoveFileExW(
                f.as_ptr(),
                t.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn is_transient(e: &io::Error) -> bool {
        matches!(
            e.raw_os_error().map(|c| c as u32),
            Some(ERROR_ACCESS_DENIED) | Some(ERROR_SHARING_VIOLATION) | Some(ERROR_LOCK_VIOLATION)
        )
    }
}

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    pub fn restrict_to_owner(path: &Path) -> io::Result<()> {
        let is_dir = std::fs::symlink_metadata(path)?.is_dir();
        let mode = if is_dir { 0o700 } else { 0o600 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }

    pub fn is_owner_only(path: &Path) -> io::Result<bool> {
        Ok(std::fs::symlink_metadata(path)?.permissions().mode() & 0o077 == 0)
    }

    pub fn replace_once(from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)?;
        if let Some(parent) = to.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    }

    pub fn is_transient(_e: &io::Error) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restrict_makes_dir_and_file_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("p");
        std::fs::create_dir(&sub).unwrap();
        restrict_to_owner(&sub).unwrap();
        assert!(is_owner_only(&sub).unwrap());
        let f = sub.join("f.txt");
        std::fs::write(&f, b"x").unwrap();
        restrict_to_owner(&f).unwrap();
        assert!(is_owner_only(&f).unwrap());
    }

    /// The checker must be able to say no, or every owner-only assertion above is hollow.
    #[test]
    fn a_default_folder_is_not_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("p");
        std::fs::create_dir(&sub).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&sub, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        assert!(!is_owner_only(&sub).unwrap());
    }

    #[test]
    fn replace_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"new").unwrap();
        std::fs::write(&b, b"old").unwrap();
        replace_file(&a, &b).unwrap();
        assert_eq!(std::fs::read(&b).unwrap(), b"new");
        assert!(!a.exists());
    }

    #[cfg(windows)]
    #[test]
    fn replace_retries_a_transient_failure() {
        let mut calls = 0;
        let r = replace_file_with(Path::new("C:\\a"), Path::new("C:\\b"), |_, _| {
            calls += 1;
            if calls < 3 {
                Err(io::Error::from_raw_os_error(32)) // ERROR_SHARING_VIOLATION
            } else {
                Ok(())
            }
        });
        assert!(r.is_ok());
        assert_eq!(calls, 3);
    }

    #[test]
    fn replace_does_not_retry_a_permanent_failure() {
        let mut calls = 0;
        let r = replace_file_with(Path::new("/a"), Path::new("/b"), |_, _| {
            calls += 1;
            Err(io::Error::new(io::ErrorKind::NotFound, "gone"))
        });
        assert!(r.is_err());
        assert_eq!(calls, 1);
    }

    #[test]
    fn refuses_relative_and_dot_paths() {
        assert!(check_plain_absolute(Path::new("relative/x")).is_err());
        let dir = tempfile::tempdir().unwrap();
        assert!(check_plain_absolute(&dir.path().join("..").join("x")).is_err());
        assert!(check_plain_absolute(&dir.path().join("x")).is_ok());
    }

    #[test]
    fn open_no_follow_reads_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f");
        std::fs::write(&f, b"x").unwrap();
        assert!(open_regular_no_follow(&f).is_ok());
        assert!(
            open_regular_no_follow(dir.path()).is_err(),
            "a folder is not a regular file"
        );
    }

    /// A junction needs no special privilege on Windows, so this always runs there.
    #[cfg(windows)]
    #[test]
    fn open_no_follow_refuses_a_junction_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("f"), b"x").unwrap();
        let link = dir.path().join("link");
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&real)
            .output()
            .unwrap();
        assert!(out.status.success(), "mklink /J failed: {out:?}");
        assert!(open_regular_no_follow(&real.join("f")).is_ok());
        let err = open_regular_no_follow(&link.join("f")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        // Remove the junction itself (not its target) before the temp dir is cleaned up.
        std::fs::remove_dir(&link).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn open_no_follow_refuses_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::write(&real, b"x").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(open_regular_no_follow(&link).is_err());
    }
}
