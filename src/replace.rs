//! Replacing the running executable with a downloaded one, without ever leaving a broken or
//! half-written `sliplane` behind.
//!
//! The new file is staged in the same directory as the old one (a rename never crosses a
//! filesystem that way), run once to prove it is the version it should be, and only then moved into
//! place. On Unix the move is a `rename` over the running file, which is atomic and leaves the
//! running process on its old inode. Windows will not let a running `.exe` be overwritten or
//! deleted, but it does let one be renamed, so the old file is moved aside to `sliplane.old.exe`,
//! the new one takes its name, and the aside file is deleted now if it can be and on the next run
//! if not.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use semver::Version;

use crate::error::{Error, Result};

/// The file this process was started from. On Unix the path is resolved first, so that a symlink
/// to the binary (Homebrew keeps one) does not get swapped for a regular file while the real
/// binary stays old.
pub fn current_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe()
        .map_err(|e| Error::other("Cannot tell where this sliplane is installed.").detail(e.to_string()))?;
    #[cfg(unix)]
    let exe = fs::canonicalize(&exe).unwrap_or(exe);
    Ok(exe)
}

/// What the running binary is moved to on Windows: `sliplane.exe` -> `sliplane.old.exe`.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn aside_path(exe: &Path) -> PathBuf {
    let stem = exe.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "sliplane".into());
    let ext = exe.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    exe.with_file_name(format!("{stem}.old{ext}"))
}

/// Deletes the file an earlier `sliplane update` moved aside. A process still running from it
/// keeps the delete from working, so this is tried on every start until it does. Silent by design.
#[cfg(windows)]
pub fn remove_stale_aside() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = fs::remove_file(aside_path(&exe));
    }
}

/// The name of whatever probably manages the binary at this path, when it is not a plain
/// download: the update still works there, but the owner will not know it happened.
pub fn managed_by(exe: &Path) -> Option<&'static str> {
    let path = exe.to_string_lossy().replace('\\', "/").to_lowercase();
    let known = [
        ("/.cargo/bin/", "cargo install"),
        ("/cellar/", "Homebrew"),
        ("/homebrew/", "Homebrew"),
        ("/linuxbrew/", "Homebrew"),
        ("/nix/store/", "Nix"),
        ("/snap/", "Snap"),
        ("/scoop/", "Scoop"),
        ("/chocolatey/", "Chocolatey"),
        ("/windowsapps/", "the Microsoft Store"),
    ];
    if let Some((_, name)) = known.iter().find(|(part, _)| path.contains(part)) {
        return Some(name);
    }
    ["/usr/bin/", "/usr/lib/", "/bin/"].iter().any(|p| path.starts_with(p)).then_some("the system's package manager")
}

fn unwritable(exe: &Path, e: std::io::Error) -> Error {
    let dir = exe.parent().unwrap_or(Path::new("."));
    let reinstall = if cfg!(windows) {
        "irm https://github.com/SpaceCorps/Sliplane-Cli/releases/latest/download/install.ps1 | iex"
    } else {
        "curl -fsSL https://github.com/SpaceCorps/Sliplane-Cli/releases/latest/download/install.sh | sh"
    };
    Error::other(format!("Cannot replace {}: {} is not writable ({e}).", exe.display(), dir.display())).fix(format!(
        "Run the update as a user who may write to {}, or install into a directory you own: {reinstall}",
        dir.display()
    ))
}

/// Renames, trying a few times on Windows, where a virus scanner holding the fresh file for a
/// moment is the usual reason a rename fails.
fn rename(from: &Path, to: &Path) -> std::io::Result<()> {
    let attempts = if cfg!(windows) { 5 } else { 1 };
    let mut tried = 1;
    loop {
        match fs::rename(from, to) {
            Err(_) if tried < attempts => {
                tried += 1;
                std::thread::sleep(Duration::from_millis(150));
            }
            result => return result,
        }
    }
}

/// A new executable waiting next to the one it will replace. Deleted on drop unless it was
/// installed, so every failure path leaves the directory as it found it.
pub struct Staged {
    path: PathBuf,
    installed: bool,
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.installed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl Staged {
    /// Creates the (empty) file. Doing this before the download means a directory that cannot be
    /// written to fails now, not after the megabytes.
    pub fn create(exe: &Path) -> Result<Staged> {
        let stem = exe.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "sliplane".into());
        let ext = exe.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        let path = exe.with_file_name(format!("{stem}.update-{}{ext}", std::process::id()));
        let _ = fs::remove_file(&path); // left by an earlier process that had this id
        fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| unwritable(exe, e))?;
        Ok(Staged { path, installed: false })
    }

    /// Writes the executable and gives it the permissions of the one it will replace.
    pub fn write(&self, bytes: &[u8], like: &Path) -> Result<()> {
        fs::write(&self.path, bytes).map_err(|e| unwritable(like, e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(like).map(|m| m.permissions().mode()).unwrap_or(0o755);
            fs::set_permissions(&self.path, fs::Permissions::from_mode((mode & 0o777) | 0o100))
                .map_err(|e| unwritable(like, e))?;
        }
        Ok(())
    }

    /// Runs the staged file with `--version` and demands exactly `sliplane <version>`: the checksum
    /// proves the download is the release, this proves the release is the one that was asked for
    /// (a forgotten version bump is the usual way they differ) and that the file runs here at all.
    pub fn verify_version(&self, expected: &Version) -> Result<()> {
        let reported = reported_version(&self.path).map_err(|why| {
            Error::other(format!("The downloaded executable does not run here: {why}."))
                .fix("The installed sliplane was not changed. Retry once; if it persists, install by hand from https://github.com/SpaceCorps/Sliplane-Cli/releases")
        })?;
        if reported != format!("sliplane {expected}") {
            return Err(Error::other(format!(
                "The downloaded executable reports '{reported}' instead of 'sliplane {expected}'."
            ))
            .detail("The installed sliplane was not changed.")
            .fix("Do not install this release; report it at https://github.com/SpaceCorps/Sliplane-Cli/issues"));
        }
        Ok(())
    }

    /// Puts the staged file where `exe` is.
    pub fn install(mut self, exe: &Path) -> Result<()> {
        #[cfg(not(windows))]
        rename(&self.path, exe).map_err(|e| unwritable(exe, e))?;

        #[cfg(windows)]
        {
            let aside = aside_path(exe);
            if aside.exists() {
                fs::remove_file(&aside).map_err(|e| {
                    Error::other(format!("{} is still in use ({e}).", aside.display())).fix(
                        "An earlier sliplane is still running. Close any running sliplane processes, then run the update again.",
                    )
                })?;
            }
            rename(exe, &aside).map_err(|e| unwritable(exe, e))?;
            if let Err(e) = rename(&self.path, exe) {
                let _ = rename(&aside, exe); // put the old one back
                return Err(unwritable(exe, e));
            }
            // This process is running from the aside file, so the delete usually fails; the next
            // run of any sliplane command removes it.
            let _ = fs::remove_file(&aside);
        }

        self.installed = true;
        Ok(())
    }
}

/// What `<exe> --version` prints, trimmed. It gets no stdin, has a deadline, and its output is
/// capped, so a broken binary cannot hang an agent.
fn reported_version(exe: &Path) -> std::result::Result<String, String> {
    let mut attempts = 0;
    let mut child = loop {
        match Command::new(exe)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => break child,
            // A freshly written executable can be "busy" for a moment on Linux.
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 10 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    let mut stdout = child.stdout.take().ok_or("no output pipe")?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout).take(64 * 1024).read_to_end(&mut out);
        out
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status,
            None if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("it did not answer --version within 15 seconds".into());
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let out = reader.join().map_err(|_| "reading its output failed")?;
    if !status.success() {
        return Err(format!("`--version` exited with {status}"));
    }
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_aside_file_sits_next_to_the_executable() {
        assert_eq!(aside_path(Path::new("/c/bin/sliplane.exe")).file_name().unwrap(), "sliplane.old.exe");
        assert_eq!(aside_path(Path::new("/home/me/.sliplane-cli/bin/sliplane")).file_name().unwrap(), "sliplane.old");
        assert_eq!(aside_path(Path::new("/a/b/sliplane")).parent(), Path::new("/a/b").into());
    }

    #[test]
    fn package_managed_paths_are_recognised() {
        let managed = |p: &str| managed_by(Path::new(p));
        assert_eq!(managed("/home/me/.cargo/bin/sliplane"), Some("cargo install"));
        assert_eq!(managed(r"C:\Users\me\.cargo\bin\sliplane.exe"), Some("cargo install"));
        assert_eq!(managed("/opt/homebrew/Cellar/sliplane/1.2.0/bin/sliplane"), Some("Homebrew"));
        assert_eq!(managed("/home/linuxbrew/.linuxbrew/bin/sliplane"), Some("Homebrew"));
        assert_eq!(managed(r"C:\Users\me\scoop\apps\sliplane\current\sliplane.exe"), Some("Scoop"));
        assert_eq!(managed("/nix/store/abc-sliplane/bin/sliplane"), Some("Nix"));
        assert_eq!(managed("/usr/bin/sliplane"), Some("the system's package manager"));
        // Where the installers put it, and ordinary places, are not "managed".
        assert_eq!(managed("/home/me/.sliplane-cli/bin/sliplane"), None);
        assert_eq!(managed(r"C:\Users\me\AppData\Local\sliplane-cli\bin\sliplane.exe"), None);
        assert_eq!(managed("/usr/local/bin/sliplane"), None);
    }
}
