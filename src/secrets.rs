//! Where API keys live: the OS keystore, one entry per account under `account:{name}`.
//!
//! DPAPI on Windows, the Keychain on macOS, libsecret on Linux. When none is available the tool
//! refuses to start rather than silently writing a file - `SLIPLANE_ALLOW_PLAINTEXT_STORE=1` is
//! the explicit opt-out. Service names, entry keys and file formats match the .NET CLI this
//! replaced, so keys stored by it are found here.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use crate::config;
use crate::error::{Error, Result};

const SERVICE: &str = "sliplane-cli";

pub fn account_key(name: &str) -> String {
    format!("account:{}", name.to_lowercase())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Store {
    Dpapi,
    Keychain,
    LibSecret,
    Plaintext,
}

/// The backend for this process. Cached: changing `SLIPLANE_SECRET_STORE` mid-process has no
/// effect, and a test that wants another backend needs another process.
pub fn store() -> Result<Store> {
    static STORE: OnceLock<std::result::Result<Store, (String, String, String)>> = OnceLock::new();
    STORE
        .get_or_init(|| {
            build().map_err(|e| (e.message, e.detail.unwrap_or_default(), e.remediation.unwrap_or_default()))
        })
        .clone()
        .map_err(|(m, d, r)| {
            let mut e = Error::other(m);
            if !d.is_empty() {
                e = e.detail(d);
            }
            if !r.is_empty() {
                e = e.fix(r);
            }
            e
        })
}

fn build() -> Result<Store> {
    if let Some(forced) = std::env::var("SLIPLANE_SECRET_STORE").ok().filter(|s| !s.trim().is_empty()) {
        return match forced.to_lowercase().as_str() {
            "dpapi" if cfg!(windows) => Ok(Store::Dpapi),
            "keychain" => Ok(Store::Keychain),
            "libsecret" => Ok(Store::LibSecret),
            "plaintext" => Ok(Store::Plaintext),
            _ => Err(Error::invalid(format!(
                "SLIPLANE_SECRET_STORE='{forced}' is not a backend available on this platform."
            ))
            .fix("Unset SLIPLANE_SECRET_STORE, or set it to one of: dpapi, keychain, libsecret, plaintext.")),
        };
    }

    if cfg!(windows) {
        return Ok(Store::Dpapi);
    }

    if cfg!(target_os = "macos") {
        if Path::new("/usr/bin/security").exists() || which("security").is_some() {
            return Ok(Store::Keychain);
        }
        return fallback("The macOS 'security' command was not found.");
    }

    if which("secret-tool").is_some() {
        return Ok(Store::LibSecret);
    }
    fallback(
        "libsecret is not installed, so there is no OS keystore to hold your API keys. \
         Install it with: sudo apt install libsecret-tools  (or the equivalent for your distro).",
    )
}

/// Degrading silently to a plaintext file would quietly undo the reason for storing keys
/// outside the environment at all, so it has to be asked for explicitly.
fn fallback(reason: &str) -> Result<Store> {
    if std::env::var("SLIPLANE_ALLOW_PLAINTEXT_STORE").as_deref() == Ok("1") {
        return Ok(Store::Plaintext);
    }
    Err(Error::other("No secure credential store is available on this machine.")
        .detail(reason)
        .fix("Install a keystore, or set SLIPLANE_ALLOW_PLAINTEXT_STORE=1 to store API keys in a 0600 file instead."))
}

fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(cmd)).find(|p| p.is_file())
}

impl Store {
    /// Backend name, as reported by `sliplane accounts list`.
    pub fn name(self) -> &'static str {
        match self {
            Store::Dpapi => "dpapi",
            Store::Keychain => "keychain",
            Store::LibSecret => "libsecret",
            Store::Plaintext => "plaintext",
        }
    }

    pub fn get(self, key: &str) -> Result<Option<String>> {
        match self {
            Store::Keychain => {
                let (ok, out, _) = run("security", &["find-generic-password", "-s", SERVICE, "-a", key, "-w"], None)?;
                Ok(ok.then(|| out.trim_end_matches('\n').to_string()).filter(|s| !s.is_empty()))
            }
            Store::LibSecret => {
                let (ok, out, _) = run("secret-tool", &["lookup", "service", SERVICE, "account", key], None)?;
                Ok(ok.then(|| out.trim_end_matches('\n').to_string()).filter(|s| !s.is_empty()))
            }
            Store::Plaintext => {
                warn_plaintext();
                Ok(file_read(self)?.remove(key))
            }
            Store::Dpapi => Ok(file_read(self)?.remove(key)),
        }
    }

    pub fn set(self, key: &str, value: &str) -> Result<()> {
        match self {
            Store::Keychain => {
                // -U updates in place if the item already exists.
                let (ok, _, err) =
                    run("security", &["add-generic-password", "-U", "-s", SERVICE, "-a", key, "-w", value], None)?;
                if !ok {
                    return Err(Error::other("Could not write to the macOS Keychain.").detail(err.trim()));
                }
                Ok(())
            }
            Store::LibSecret => {
                // The secret goes in on stdin rather than argv, so it never appears in the
                // process list.
                let label = format!("--label={SERVICE}: {key}");
                let (ok, _, err) =
                    run("secret-tool", &["store", &label, "service", SERVICE, "account", key], Some(value))?;
                if !ok {
                    return Err(Error::other("Could not write to the system keyring.")
                        .detail(err.trim())
                        .fix("Check that a secret service (gnome-keyring, kwallet) is running and unlocked."));
                }
                Ok(())
            }
            Store::Plaintext | Store::Dpapi => {
                if self == Store::Plaintext {
                    warn_plaintext();
                }
                let mut all = file_read(self)?;
                all.insert(key.to_string(), value.to_string());
                file_write(self, &all)
            }
        }
    }

    pub fn delete(self, key: &str) -> Result<()> {
        match self {
            Store::Keychain => {
                run("security", &["delete-generic-password", "-s", SERVICE, "-a", key], None)?;
                Ok(())
            }
            Store::LibSecret => {
                run("secret-tool", &["clear", "service", SERVICE, "account", key], None)?;
                Ok(())
            }
            Store::Plaintext | Store::Dpapi => {
                let mut all = file_read(self)?;
                if all.remove(key).is_some() {
                    file_write(self, &all)?;
                }
                Ok(())
            }
        }
    }
}

fn run(program: &str, args: &[&str], stdin: Option<&str>) -> Result<(bool, String, String)> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::other(format!("Could not start '{program}'.")).detail(e.to_string()))?;

    if let Some(input) = stdin
        && let Some(mut pipe) = child.stdin.take()
    {
        let _ = pipe.write_all(input.as_bytes());
    }

    let out =
        child.wait_with_output().map_err(|e| Error::other(format!("'{program}' failed.")).detail(e.to_string()))?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

fn file_path(store: Store) -> PathBuf {
    config::config_dir().join(if store == Store::Dpapi { "secrets.dpapi" } else { "secrets.json" })
}

/// Warns on every process that reads the plaintext store rather than only when an account is
/// added, because the whole risk of this backend is that you forget you chose it.
fn warn_plaintext() {
    static WARNED: OnceLock<()> = OnceLock::new();
    WARNED.get_or_init(|| {
        eprintln!(
            "warning: API keys are stored unencrypted in {} (SLIPLANE_ALLOW_PLAINTEXT_STORE=1).",
            file_path(Store::Plaintext).display()
        );
    });
}

fn file_read(store: Store) -> Result<BTreeMap<String, String>> {
    let path = file_path(store);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e.into()),
    };
    let plain = if store == Store::Dpapi { dpapi::unprotect(&bytes, &path)? } else { bytes };
    serde_json::from_slice(&plain)
        .map_err(|e| Error::other(format!("{} is not valid.", path.display())).detail(e.to_string()))
}

fn file_write(store: Store, all: &BTreeMap<String, String>) -> Result<()> {
    config::ensure_dir()?;
    let path = file_path(store);
    let json = if store == Store::Dpapi { serde_json::to_vec(all) } else { serde_json::to_vec_pretty(all) }
        .expect("a string map always serializes");
    let data = if store == Store::Dpapi { dpapi::protect(&json)? } else { json };
    config::atomic_write(&path, &data)
}

#[cfg(windows)]
mod dpapi {
    //! A single JSON blob encrypted with DPAPI under the current user, so it can only be
    //! decrypted by this Windows account on this machine. No key material of ours to manage.

    use std::path::Path;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    use crate::error::{Error, Result};

    fn take(blob: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let v = unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec();
        unsafe { LocalFree(blob.pbData as _) };
        v
    }

    pub fn protect(plain: &[u8]) -> Result<Vec<u8>> {
        let input = CRYPT_INTEGER_BLOB { cbData: plain.len() as u32, pbData: plain.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: null_mut() };
        let ok =
            unsafe { CryptProtectData(&input, null(), null(), null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out) };
        if ok == 0 {
            return Err(Error::other("DPAPI could not encrypt the API keys.")
                .detail(std::io::Error::last_os_error().to_string()));
        }
        Ok(take(out))
    }

    pub fn unprotect(data: &[u8], path: &Path) -> Result<Vec<u8>> {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: null_mut() };
        let ok = unsafe {
            CryptUnprotectData(&input, null_mut(), null(), null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        };
        if ok == 0 {
            return Err(Error::other("The stored API keys could not be decrypted.")
                .detail(format!(
                    "DPAPI failed: {}. This happens when the file was written by a different Windows user or on a different machine.",
                    std::io::Error::last_os_error()
                ))
                .fix(format!("Delete {} and run: sliplane accounts add <name>", path.display())));
        }
        Ok(take(out))
    }
}

#[cfg(not(windows))]
mod dpapi {
    use std::path::Path;

    use crate::error::{Error, Result};

    pub fn protect(_: &[u8]) -> Result<Vec<u8>> {
        Err(Error::invalid("DPAPI is only available on Windows."))
    }

    pub fn unprotect(_: &[u8], _: &Path) -> Result<Vec<u8>> {
        Err(Error::invalid("DPAPI is only available on Windows."))
    }
}
