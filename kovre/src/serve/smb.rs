//! SMB authentication helper.
//!
//! For each repository whose `path` is a UNC (`\\host\share\...`) and
//! whose `smb_user` is set in `kovre.yaml`, call `WNetAddConnection2`
//! at boot to authenticate the current session against the share.
//! The password is read from the DPAPI-encrypted file at
//! `smb_password_file` — it lives in RAM for the duration of the API
//! call and is dropped immediately after.
//!
//! Windows-only. On other platforms the public functions are no-ops
//! that log a warning if any repo had SMB credentials set.

use kovre_core::config::{Config, Repository};
use std::path::Path;
use tracing::{info, warn};

/// Attempt to authenticate against every UNC share referenced by a
/// repo with `smb_user` set. Errors are logged but never propagated:
/// a failing connection is reflected as "unreachable" in the
/// dashboard's repository status, not as a server boot failure.
pub fn setup_connections(cfg: &Config) {
    for (name, repo) in &cfg.repositories {
        let Some(user) = repo.smb_user.as_deref() else {
            continue;
        };
        let Some(pwd_file) = repo.smb_password_file.as_deref() else {
            warn!(
                repo = name,
                "smb_user set but smb_password_file missing — skipping SMB auth"
            );
            continue;
        };
        match connect_to_share(repo, user, pwd_file) {
            Ok(target) => {
                info!(repo = name, target = %target, "SMB share authenticated");
            }
            Err(e) => {
                warn!(repo = name, "SMB auth failed: {e:#}");
            }
        }
    }
}

#[cfg(windows)]
fn connect_to_share(
    repo: &Repository,
    user: &str,
    pwd_file: &Path,
) -> anyhow::Result<String> {
    use anyhow::Context;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use windows_sys::Win32::NetworkManagement::WNet::{
        WNetAddConnection2W, CONNECT_TEMPORARY, NETRESOURCEW, RESOURCETYPE_DISK,
    };

    let target = unc_share_root(&repo.path)
        .context("repository path is not a UNC `\\\\host\\share\\...`")?;

    let password = kovre_core::dpapi::decrypt_file_as_string(pwd_file)
        .context("decrypting SMB password file")?;

    let mut target_w: Vec<u16> = OsStr::new(&target).encode_wide().chain([0]).collect();
    let mut user_w: Vec<u16> = OsStr::new(user).encode_wide().chain([0]).collect();
    let mut password_w: Vec<u16> = OsStr::new(&password).encode_wide().chain([0]).collect();

    let mut netres = NETRESOURCEW {
        dwScope: 0,
        dwType: RESOURCETYPE_DISK,
        dwDisplayType: 0,
        dwUsage: 0,
        lpLocalName: ptr::null_mut(),
        lpRemoteName: target_w.as_mut_ptr(),
        lpComment: ptr::null_mut(),
        lpProvider: ptr::null_mut(),
    };

    // SAFETY: all pointer arguments are properly sized null-terminated
    // wide-strings owned by this function; the API only reads them.
    // CONNECT_TEMPORARY = the connection is not persisted in the user
    // profile (cleaner: no surprise mappings left after kovre exits).
    let rc = unsafe {
        WNetAddConnection2W(
            &netres,
            password_w.as_mut_ptr(),
            user_w.as_mut_ptr(),
            CONNECT_TEMPORARY,
        )
    };

    // Best-effort zeroize of the password buffer.
    for w in password_w.iter_mut() {
        *w = 0;
    }
    drop(password);

    match rc {
        0 => Ok(target),
        // ERROR_SESSION_CREDENTIAL_CONFLICT (1219) — already
        // authenticated to this share with different creds. Treat
        // as success: the existing session is usable.
        1219 => {
            info!(target = %target, "SMB share already authenticated in this session");
            Ok(target)
        }
        rc => Err(anyhow::anyhow!(
            "WNetAddConnection2 failed (code {rc}) — check user/password and SMB protocol settings on the share"
        )),
    }
}

#[cfg(not(windows))]
fn connect_to_share(
    _repo: &Repository,
    _user: &str,
    _pwd_file: &Path,
) -> anyhow::Result<String> {
    anyhow::bail!("SMB authentication is only supported on Windows")
}

/// Extract `\\host\share` from a UNC like `\\host\share\sub\path`.
/// Returns `None` for non-UNC paths.
pub fn unc_share_root(path: &Path) -> Option<String> {
    let s = path.to_string_lossy();
    if !s.starts_with(r"\\") {
        return None;
    }
    let after = &s[2..];
    let mut parts = after.splitn(3, '\\');
    let host = parts.next()?;
    let share = parts.next()?;
    if host.is_empty() || share.is_empty() {
        return None;
    }
    Some(format!(r"\\{host}\{share}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn extracts_share_root_from_unc() {
        let p = PathBuf::from(r"\\diskstation\disque\kovre\photos");
        assert_eq!(unc_share_root(&p).unwrap(), r"\\diskstation\disque");
    }

    #[test]
    fn extracts_share_root_minimal() {
        let p = PathBuf::from(r"\\nas\share");
        assert_eq!(unc_share_root(&p).unwrap(), r"\\nas\share");
    }

    #[test]
    fn refuses_non_unc_paths() {
        assert!(unc_share_root(&PathBuf::from(r"C:\backup\kovre")).is_none());
        assert!(unc_share_root(&PathBuf::from("/home/me/backup")).is_none());
        assert!(unc_share_root(&PathBuf::from(r"\notunc")).is_none());
    }

    #[test]
    fn refuses_unc_missing_share() {
        assert!(unc_share_root(&PathBuf::from(r"\\diskstation")).is_none());
        assert!(unc_share_root(&PathBuf::from(r"\\diskstation\")).is_none());
    }
}
