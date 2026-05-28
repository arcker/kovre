//! Windows Data Protection API wrapper.
//!
//! Provides `encrypt` / `decrypt` that operate on bytes using the
//! `CurrentUser` scope — the produced blob can only be decrypted by
//! the same Windows user on the same machine. Chrome/Edge/Outlook
//! and Windows Credential Manager all use this API for local
//! secret storage; we use it to store SMB share passwords on disk
//! without leaving them readable in plain text.
//!
//! On non-Windows platforms the module compiles but every function
//! returns an error — kovre is Windows-native today, so SMB
//! credential storage is a Windows-only feature.

use anyhow::{Context, Result};

#[cfg(windows)]
pub fn encrypt(plaintext: &[u8]) -> Result<Vec<u8>> {
    use std::ptr;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};

    let mut input = CRYPT_INTEGER_BLOB {
        cbData: plaintext.len() as u32,
        pbData: plaintext.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };

    // SAFETY: input/output are properly-sized blobs; the Windows API
    // writes into `output.pbData` and we LocalFree it after copying.
    // No null derefs because we check the success code.
    let ok = unsafe {
        CryptProtectData(
            &mut input,
            ptr::null(),       // description (optional)
            ptr::null_mut(),   // entropy (no extra key beyond user account)
            ptr::null_mut(),   // reserved
            ptr::null_mut(),   // prompt struct (none)
            0,                 // flags: CurrentUser scope (default)
            &mut output,
        )
    };

    if ok == 0 {
        let err = std::io::Error::last_os_error();
        return Err(anyhow::anyhow!("CryptProtectData failed: {err}"));
    }

    let blob_slice =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) };
    let blob = blob_slice.to_vec();
    unsafe {
        LocalFree(output.pbData as _);
    }
    Ok(blob)
}

#[cfg(windows)]
pub fn decrypt(ciphertext: &[u8]) -> Result<Vec<u8>> {
    use std::ptr;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    let mut input = CRYPT_INTEGER_BLOB {
        cbData: ciphertext.len() as u32,
        pbData: ciphertext.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };

    let ok = unsafe {
        CryptUnprotectData(
            &mut input,
            ptr::null_mut(), // description out (we don't use it)
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            0,
            &mut output,
        )
    };

    if ok == 0 {
        let err = std::io::Error::last_os_error();
        return Err(anyhow::anyhow!(
            "CryptUnprotectData failed (wrong user/machine, or tampered blob?): {err}"
        ));
    }

    let plain_slice =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) };
    let plain = plain_slice.to_vec();
    unsafe {
        LocalFree(output.pbData as _);
    }
    Ok(plain)
}

#[cfg(not(windows))]
pub fn encrypt(_plaintext: &[u8]) -> Result<Vec<u8>> {
    anyhow::bail!("DPAPI is only available on Windows")
}

#[cfg(not(windows))]
pub fn decrypt(_ciphertext: &[u8]) -> Result<Vec<u8>> {
    anyhow::bail!("DPAPI is only available on Windows")
}

/// Convenience: read a DPAPI blob from disk, decrypt, and return the
/// plaintext as a `String` (UTF-8). Most callers want a password
/// rather than raw bytes.
pub fn decrypt_file_as_string(path: &std::path::Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading DPAPI blob `{}`", path.display()))?;
    let plain = decrypt(&bytes)?;
    String::from_utf8(plain).context("DPAPI blob decrypted but content is not valid UTF-8")
}

/// Convenience: encrypt a `&str` and write the resulting blob to
/// disk. Caller is responsible for ACL-ing the resulting file (e.g.
/// via `icacls`) so other users can't even *read* the ciphertext.
pub fn encrypt_string_to_file(plaintext: &str, path: &std::path::Path) -> Result<()> {
    let blob = encrypt(plaintext.as_bytes())?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating parent of `{}`", path.display()))?;
        }
    }
    std::fs::write(path, &blob)
        .with_context(|| format!("writing DPAPI blob to `{}`", path.display()))?;
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn round_trip_via_blob() {
        let plain = b"my-smb-password-42!";
        let cipher = encrypt(plain).expect("encrypt");
        // Cipher should not contain the plaintext literally.
        assert!(!cipher.windows(plain.len()).any(|w| w == plain));
        let back = decrypt(&cipher).expect("decrypt");
        assert_eq!(back, plain);
    }

    #[test]
    fn round_trip_via_file() {
        let temp = TempDir::new().unwrap();
        let p = temp.path().join("test.dpapi");
        encrypt_string_to_file("hunter2", &p).unwrap();
        let back = decrypt_file_as_string(&p).unwrap();
        assert_eq!(back, "hunter2");
    }

    #[test]
    fn decrypt_of_garbage_fails_cleanly() {
        let garbage = vec![0u8; 32];
        let err = decrypt(&garbage).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("CryptUnprotectData failed"));
    }
}
