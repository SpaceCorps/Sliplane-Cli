//! Drives `sliplane update` end to end: the real binary, a copy of it in a temp directory (never
//! the cargo target binary), and an in-process stand-in for GitHub's releases page that serves a
//! fake release laid out the way the release workflow lays it out.
//!
//! The "new" executables in those releases are tiny programs compiled with `rustc` that print
//! `sliplane X.Y.Z`: a copy of the real binary would report the real version, which is exactly
//! what an update must not be tested with.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use semver::Version;
use serde_json::Value;
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------------------------
// the stand-in releases page

#[derive(Clone)]
struct Reply {
    status: u16,
    location: Option<String>,
    body: Vec<u8>,
}

#[derive(Clone, Debug)]
struct Seen {
    path: String,
    headers: Vec<(String, String)>,
}

struct Server {
    base: String,
    routes: Arc<Mutex<HashMap<String, Reply>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    fn start() -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes: Arc<Mutex<HashMap<String, Reply>>> = Arc::default();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let (routes2, seen2) = (routes.clone(), seen.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let (routes, seen) = (routes2.clone(), seen2.clone());
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                    let mut headers = Vec::new();
                    loop {
                        let mut h = String::new();
                        if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim_end().is_empty() {
                            break;
                        }
                        if let Some((k, v)) = h.trim_end().split_once(':') {
                            headers.push((k.trim().to_lowercase(), v.trim().to_string()));
                        }
                    }
                    seen.lock().unwrap().push(Seen { path: path.clone(), headers });
                    let reply = routes.lock().unwrap().get(&path).cloned();
                    let reply = reply.unwrap_or(Reply { status: 404, location: None, body: b"not found".to_vec() });
                    let location = reply.location.map(|l| format!("Location: {l}\r\n")).unwrap_or_default();
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {} X\r\n{location}Content-Length: {}\r\nConnection: close\r\n\r\n",
                        reply.status,
                        reply.body.len()
                    );
                    let _ = stream.write_all(&reply.body);
                });
            }
        });
        Server { base, routes, seen }
    }

    fn set(&self, path: impl Into<String>, status: u16, body: Vec<u8>) {
        self.routes.lock().unwrap().insert(path.into(), Reply { status, location: None, body });
    }

    fn remove(&self, path: &str) {
        self.routes.lock().unwrap().remove(path);
    }

    /// `/latest` redirects to the tag page, as GitHub's does.
    fn set_latest(&self, version: &Version) {
        let location = Some(format!("{}/tag/v{version}", self.base));
        self.routes.lock().unwrap().insert("/latest".into(), Reply { status: 302, location, body: vec![] });
    }

    fn paths(&self) -> Vec<String> {
        self.seen.lock().unwrap().iter().map(|s| s.path.clone()).collect()
    }
}

// ---------------------------------------------------------------------------------------------
// fake releases

/// The release archive name and executable path for the platform the tests run on.
struct Host {
    target: &'static str,
    zip: bool,
}

fn host() -> Host {
    let (target, zip) = if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        ("x86_64-pc-windows-msvc", true)
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        ("x86_64-unknown-linux-musl", false)
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        ("aarch64-unknown-linux-musl", false)
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        ("aarch64-apple-darwin", false)
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        ("x86_64-apple-darwin", false)
    } else {
        panic!("no release target for this platform");
    };
    Host { target, zip }
}

fn exe_name() -> &'static str {
    if cfg!(windows) { "sliplane.exe" } else { "sliplane" }
}

/// A program that answers `--version` with `sliplane <version>`, compiled once per version.
fn fake_binary(version: &str) -> Vec<u8> {
    static BUILT: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();
    let mut built = BUILT.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(bytes) = built.get(version) {
        return bytes.clone();
    }
    let dir = unique_dir("fake");
    let source = dir.join("fake.rs");
    std::fs::write(
        &source,
        format!("fn main() {{ if std::env::args().nth(1).as_deref() == Some(\"--version\") {{ println!(\"sliplane {version}\"); }} }}"),
    )
    .unwrap();
    let out = dir.join(exe_name());
    let status = Command::new("rustc")
        .args(["--edition", "2021", "-C", "debuginfo=0", "-C", "opt-level=0", "-o"])
        .arg(&out)
        .arg(&source)
        .output()
        .expect("rustc must be on PATH for the update tests");
    assert!(status.status.success(), "rustc failed: {}", String::from_utf8_lossy(&status.stderr));
    let bytes = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    built.insert(version.to_string(), bytes.clone());
    bytes
}

fn unique_dir(what: &str) -> PathBuf {
    // A counter, not the clock: parallel tests must never share a directory.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "sliplane-update-{what}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An archive the way the release workflow builds one: `sliplane-vX-<target>/` holding the
/// executable, README.md and LICENSE.
fn archive(version: &str, binary: &[u8]) -> Vec<u8> {
    let host = host();
    let package = format!("sliplane-v{version}-{}", host.target);
    let exe = format!("{package}/{}", exe_name());
    entries_archive(&[
        (&format!("{package}/README.md"), b"readme"),
        (&exe, binary),
        (&format!("{package}/LICENSE"), b"mit"),
    ])
}

/// An archive of exactly these entries, in the host's archive format.
fn entries_archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    if host().zip {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, data) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    } else {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            // Written as raw bytes so that names the tar writer would refuse (`..`) can be used.
            header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder.append(&header, *data).unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&builder.into_inner().unwrap()).unwrap();
        gz.finish().unwrap()
    }
}

fn archive_name(version: &str) -> String {
    let host = host();
    format!("sliplane-v{version}-{}.{}", host.target, if host.zip { "zip" } else { "tar.gz" })
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Publishes `version` on the server with the given archive bytes and a matching checksums.txt,
/// and returns the checksums line's hash.
fn publish_archive(server: &Server, version: &str, archive: Vec<u8>) -> String {
    let hash = sha256(&archive);
    let name = archive_name(version);
    server.set(format!("/download/v{version}/{name}"), 200, archive);
    server.set(
        format!("/download/v{version}/checksums.txt"),
        200,
        format!("{} *install.sh\n{hash}  {name}\n", "a".repeat(64)).into_bytes(),
    );
    hash
}

/// Publishes a release whose executable really is `sliplane <version>`.
fn publish(server: &Server, version: &str) {
    publish_archive(server, version, archive(version, &fake_binary(version)));
}

// ---------------------------------------------------------------------------------------------
// the binary under test

fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap()
}

fn newer() -> Version {
    Version::new(current().major + 1, 0, 0)
}

fn older() -> Version {
    Version::new(0, 0, 1)
}

struct Install {
    dir: PathBuf,
    exe: PathBuf,
    original: Vec<u8>,
}

impl Install {
    /// A copy of the real binary in a directory of its own.
    fn new() -> Install {
        let dir = unique_dir("install");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(exe_name());
        std::fs::copy(env!("CARGO_BIN_EXE_sliplane"), &exe).unwrap();
        let original = std::fs::read(&exe).unwrap();
        Install { dir, exe, original }
    }

    fn bin(&self) -> &Path {
        self.exe.parent().unwrap()
    }

    fn command(&self, server: &Server, args: &[&str]) -> Command {
        let mut command = Command::new(&self.exe);
        command
            .args(args)
            .env("SLIPLANE_RELEASES_URL", &server.base)
            .env("SLIPLANE_CONFIG_DIR", self.dir.join("config"))
            .env("SLIPLANE_SECRET_STORE", "plaintext");
        for proxy in ["ALL_PROXY", "HTTPS_PROXY", "HTTP_PROXY", "all_proxy", "https_proxy", "http_proxy"] {
            command.env_remove(proxy);
        }
        command
    }

    fn output(&self, server: &Server, args: &[&str]) -> Output {
        run(&mut self.command(server, args))
    }

    /// Runs the command with `--json` and returns (exit code, stdout JSON, stderr JSON).
    fn json(&self, server: &Server, args: &[&str]) -> (i32, Value, Value) {
        let mut all = args.to_vec();
        all.push("--json");
        let out = self.output(server, &all);
        let parse = |bytes: &[u8]| serde_json::from_slice(bytes).unwrap_or(Value::Null);
        (out.status.code().unwrap_or(-1), parse(&out.stdout), parse(&out.stderr))
    }

    /// What `<exe> --version` prints now.
    fn version(&self) -> String {
        let out = run(Command::new(&self.exe).arg("--version"));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The names in the install directory, so a test can see that nothing was left behind.
    fn files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.bin())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn untouched(&self) {
        assert_eq!(std::fs::read(&self.exe).unwrap(), self.original, "the installed binary changed");
        assert_eq!(self.files(), [exe_name()], "something was left in the install directory");
    }
}

impl Drop for Install {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Runs a command, retrying while the kernel says a just-copied executable is busy: another test
/// thread forking at the wrong moment can hold a write handle on it for an instant.
fn run(command: &mut Command) -> Output {
    for _ in 0..20 {
        match command.output() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(50))
            }
            result => return result.unwrap(),
        }
    }
    command.output().unwrap()
}

// ---------------------------------------------------------------------------------------------
// checking

#[test]
fn check_reports_a_newer_release_and_changes_nothing() {
    let server = Server::start();
    server.set_latest(&newer());
    let install = Install::new();
    let (code, out, _) = install.json(&server, &["update", "--check"]);
    assert_eq!(code, 0);
    assert_eq!(out["status"], "update_available");
    assert_eq!(out["from"], current().to_string());
    assert_eq!(out["to"], newer().to_string());
    assert_eq!(out["latest"], newer().to_string());
    // Compared by its tail: macOS resolves /var to /private/var, Windows may spell the temp dir 8.3.
    assert!(Path::new(out["path"].as_str().unwrap()).ends_with(Path::new("bin").join(exe_name())), "{out}");
    assert_eq!(out["source"], server.base, "a mirror in use is said so");
    assert_eq!(server.paths(), ["/latest"], "--check must not download anything");
    install.untouched();
}

#[test]
fn the_default_output_is_yaml_and_never_empty() {
    let server = Server::start();
    server.set_latest(&current());
    let install = Install::new();
    let out = install.output(&server, &["update"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("status: up_to_date"), "{text}");
    assert!(text.contains(&format!("from: {}", current())), "{text}");
    assert!(text.contains("path:"), "{text}");
}

#[test]
fn being_on_the_latest_release_is_up_to_date() {
    let server = Server::start();
    server.set_latest(&current());
    let install = Install::new();
    for args in [&["update", "--check"][..], &["update"]] {
        let (code, out, _) = install.json(&server, args);
        assert_eq!(code, 0);
        assert_eq!(out["status"], "up_to_date");
        assert_eq!(out["from"], out["to"]);
    }
    assert!(server.paths().iter().all(|p| p == "/latest"), "{:?}", server.paths());
    install.untouched();
}

#[test]
fn a_release_older_than_this_build_is_never_installed() {
    let server = Server::start();
    server.set_latest(&older());
    publish(&server, &older().to_string());
    let install = Install::new();
    let (code, out, _) = install.json(&server, &["update"]);
    assert_eq!(code, 0);
    assert_eq!(out["status"], "up_to_date");
    assert_eq!(out["to"], current().to_string(), "stays where it is");
    assert_eq!(out["latest"], older().to_string());
    assert_eq!(server.paths(), ["/latest"]);
    install.untouched();
}

#[test]
fn naming_the_running_version_changes_nothing() {
    let server = Server::start();
    let install = Install::new();
    let (code, out, _) = install.json(&server, &["update", "--version", &format!("v{}", current())]);
    assert_eq!(code, 0);
    assert_eq!(out["status"], "up_to_date");
    assert!(server.paths().is_empty(), "no request is needed to learn that");
    install.untouched();
}

// ---------------------------------------------------------------------------------------------
// updating

#[test]
fn update_replaces_the_binary_with_the_verified_release() {
    let server = Server::start();
    let version = newer().to_string();
    server.set_latest(&newer());
    publish(&server, &version);
    let install = Install::new();

    let (code, out, err) = install.json(&server, &["update"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["status"], "updated");
    assert_eq!(out["from"], current().to_string());
    assert_eq!(out["to"], version);
    assert_eq!(out["latest"], version);

    // The file at the old path is now the new release.
    assert_eq!(install.version(), format!("sliplane {version}"));
    assert_eq!(std::fs::read(&install.exe).unwrap(), fake_binary(&version));

    // Nothing is left behind but, on Windows, the old exe that the running process held open.
    let leftovers: Vec<String> = install.files().into_iter().filter(|f| f != exe_name()).collect();
    if cfg!(windows) {
        assert!(leftovers.iter().all(|f| f == "sliplane.old.exe"), "{leftovers:?}");
    } else {
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    // It asked the release server for the version, the checksums and the one archive, and carried
    // no credentials of any kind.
    assert_eq!(
        server.paths(),
        [
            "/latest".to_string(),
            format!("/download/v{version}/checksums.txt"),
            format!("/download/v{version}/{}", archive_name(&version))
        ]
    );
    for seen in server.seen.lock().unwrap().iter() {
        let names: Vec<&str> = seen.headers.iter().map(|(k, _)| k.as_str()).collect();
        assert!(!names.contains(&"authorization") && !names.contains(&"x-organization-id"), "{seen:?}");
    }
}

#[cfg(unix)]
#[test]
fn update_keeps_the_permissions_of_the_binary_it_replaces() {
    use std::os::unix::fs::PermissionsExt;
    let server = Server::start();
    let version = newer().to_string();
    server.set_latest(&newer());
    publish(&server, &version);
    let install = Install::new();
    std::fs::set_permissions(&install.exe, std::fs::Permissions::from_mode(0o750)).unwrap();
    let (code, _, err) = install.json(&server, &["update"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(std::fs::metadata(&install.exe).unwrap().permissions().mode() & 0o777, 0o750);
}

#[test]
fn an_explicit_version_may_be_older_than_the_running_one() {
    let server = Server::start();
    let version = older().to_string();
    publish(&server, &version);
    let install = Install::new();
    let (code, out, err) = install.json(&server, &["update", "--version", &format!("v{version}")]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out["status"], "updated");
    assert_eq!(out["from"], current().to_string());
    assert_eq!(out["to"], version);
    assert!(out.get("latest").is_none(), "an explicit version does not ask what the latest is");
    assert_eq!(install.version(), format!("sliplane {version}"));
    assert!(!server.paths().contains(&"/latest".to_string()));
}

// ---------------------------------------------------------------------------------------------
// refusing

#[test]
fn a_tampered_archive_is_rejected_and_the_old_binary_kept() {
    let server = Server::start();
    let version = newer().to_string();
    server.set_latest(&newer());
    publish(&server, &version);
    // The checksum no longer matches what is served.
    server.set(
        format!("/download/v{version}/checksums.txt"),
        200,
        format!("{}  {}\n", "0".repeat(64), archive_name(&version)).into_bytes(),
    );
    let install = Install::new();
    let (code, out, err) = install.json(&server, &["update"]);
    assert_eq!(code, 1);
    assert_eq!(out, Value::Null, "a failure prints no success output");
    assert_eq!(err["code"], "error");
    assert!(err["error"].as_str().unwrap().contains("does not match"), "{err}");
    assert!(err["remediation"].as_str().unwrap().contains("not changed"), "{err}");
    install.untouched();
}

#[test]
fn an_archive_that_changed_after_the_checksum_was_written_is_rejected() {
    let server = Server::start();
    let version = newer().to_string();
    server.set_latest(&newer());
    publish(&server, &version);
    // Valid archive, different bytes: a swapped release asset.
    let other = archive(&version, b"some other program");
    server.set(format!("/download/v{version}/{}", archive_name(&version)), 200, other);
    let install = Install::new();
    let (code, _, err) = install.json(&server, &["update"]);
    assert_eq!(code, 1);
    assert!(err["error"].as_str().unwrap().contains("does not match"), "{err}");
    install.untouched();
}

#[test]
fn a_release_without_checksums_is_refused_before_the_archive_is_fetched() {
    let server = Server::start();
    let version = newer().to_string();
    server.set_latest(&newer());
    publish(&server, &version);
    server.remove(&format!("/download/v{version}/checksums.txt"));
    let install = Install::new();
    let (code, _, err) = install.json(&server, &["update"]);
    assert_eq!(code, 4);
    assert_eq!(err["code"], "not_found");
    assert!(err["error"].as_str().unwrap().contains("checksums.txt"), "{err}");
    assert!(err["detail"].as_str().unwrap().contains("1.2.0"), "names the releases that have none: {err}");
    assert!(err["remediation"].as_str().unwrap().contains("sliplane update --check"), "{err}");
    assert!(!server.paths().iter().any(|p| p.ends_with(".tar.gz") || p.ends_with(".zip")), "{:?}", server.paths());
    install.untouched();
}

#[test]
fn checksums_that_do_not_cover_the_archive_are_refused() {
    let server = Server::start();
    let version = newer().to_string();
    server.set_latest(&newer());
    publish(&server, &version);
    server.set(
        format!("/download/v{version}/checksums.txt"),
        200,
        format!("{}  install.sh\n{}  sliplane-v{version}-some-other-target.tar.gz\n", "a".repeat(64), "b".repeat(64))
            .into_bytes(),
    );
    let install = Install::new();
    let (code, _, err) = install.json(&server, &["update"]);
    assert_eq!(code, 1);
    assert!(err["error"].as_str().unwrap().contains("no checksum for"), "{err}");
    install.untouched();
}

#[test]
fn a_release_with_no_build_for_this_platform_is_not_found() {
    let server = Server::start();
    let version = newer().to_string();
    server.set_latest(&newer());
    publish(&server, &version);
    server.remove(&format!("/download/v{version}/{}", archive_name(&version)));
    let install = Install::new();
    let (code, _, err) = install.json(&server, &["update"]);
    assert_eq!(code, 4);
    assert_eq!(err["code"], "not_found");
    install.untouched();
}

#[test]
fn an_executable_that_reports_another_version_is_refused() {
    let server = Server::start();
    let version = newer().to_string();
    server.set_latest(&newer());
    // A release tagged for `version` that holds a binary that says it is something else: the
    // forgotten version bump the installers also guard against.
    publish_archive(&server, &version, archive(&version, &fake_binary("0.0.2")));
    let install = Install::new();
    let (code, _, err) = install.json(&server, &["update"]);
    assert_eq!(code, 1);
    assert!(err["error"].as_str().unwrap().contains("reports 'sliplane 0.0.2'"), "{err}");
    install.untouched();
}

#[test]
fn an_executable_that_cannot_run_is_refused() {
    let server = Server::start();
    let version = newer().to_string();
    server.set_latest(&newer());
    publish_archive(&server, &version, archive(&version, b"this is not an executable"));
    let install = Install::new();
    let (code, _, err) = install.json(&server, &["update"]);
    assert_eq!(code, 1);
    assert!(err["error"].as_str().unwrap().contains("does not run here"), "{err}");
    install.untouched();
}

#[test]
fn a_hostile_archive_with_a_valid_checksum_is_refused() {
    let server = Server::start();
    let version = newer().to_string();
    server.set_latest(&newer());
    let host = host();
    let package = format!("sliplane-v{version}-{}", host.target);
    let name = exe_name();
    // Everything that tries to be the executable from somewhere it should not be.
    let hostile = entries_archive(&[
        (&format!("../{name}"), b"evil"),
        (&format!("../../{package}/{name}"), b"evil"),
        (&format!("/{package}/{name}"), b"evil"),
        (&format!("{package}/sub/{name}"), b"evil"),
        (&format!("{package}/README.md"), b"readme"),
    ]);
    publish_archive(&server, &version, hostile);
    let install = Install::new();
    let (code, _, err) = install.json(&server, &["update"]);
    assert_eq!(code, 1);
    assert!(err["error"].as_str().unwrap().contains("does not hold"), "{err}");
    install.untouched();
    // Nothing was written outside the install directory either.
    assert!(!install.dir.join(name).exists());
}

#[test]
fn a_garbled_version_is_rejected_before_any_request() {
    let server = Server::start();
    let install = Install::new();
    for bad in ["latest", "1.3", "v1.3.0/../x", "1.3.0+build"] {
        let (code, _, err) = install.json(&server, &["update", "--version", bad]);
        assert_eq!(code, 6, "{bad}");
        assert_eq!(err["code"], "invalid_input");
    }
    assert!(server.paths().is_empty());
    install.untouched();
}

#[test]
fn a_latest_page_that_is_not_a_redirect_is_an_error() {
    let server = Server::start();
    server.set("/latest", 200, b"<html>hello</html>".to_vec());
    let install = Install::new();
    let (code, _, err) = install.json(&server, &["update", "--check"]);
    assert_eq!(code, 1);
    assert!(err["error"].as_str().unwrap().contains("Could not tell which release"), "{err}");

    server.set("/latest", 404, b"nope".to_vec());
    let (code, _, err) = install.json(&server, &["update", "--check"]);
    assert_eq!(code, 4);
    assert_eq!(err["code"], "not_found");
}

#[test]
fn an_unreachable_release_server_is_a_network_error() {
    let server = Server::start();
    let dead = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let install = Install::new();
    let mut command = install.command(&server, &["update", "--check", "--json"]);
    command.env("SLIPLANE_RELEASES_URL", dead);
    let out = run(&mut command);
    assert_eq!(out.status.code(), Some(2));
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(err["code"], "network");
    assert_eq!(err["remediation"], "Retry once, then stop.");
}

#[cfg(unix)]
#[test]
fn an_install_directory_that_cannot_be_written_fails_before_downloading() {
    use std::os::unix::fs::PermissionsExt;
    let server = Server::start();
    server.set_latest(&newer());
    publish(&server, &newer().to_string());
    let install = Install::new();
    std::fs::set_permissions(install.bin(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let probe = install.bin().join("probe");
    if std::fs::write(&probe, b"x").is_ok() {
        // Running as root: permissions do not apply, so there is nothing to test.
        std::fs::remove_file(probe).unwrap();
        std::fs::set_permissions(install.bin(), std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let (code, _, err) = install.json(&server, &["update"]);
    std::fs::set_permissions(install.bin(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(code, 1);
    assert!(err["error"].as_str().unwrap().contains("not writable"), "{err}");
    assert!(err["remediation"].as_str().unwrap().contains("install.sh"), "{err}");
    assert_eq!(server.paths(), ["/latest"], "no download before the directory is known to be writable");
    install.untouched();
}

// ---------------------------------------------------------------------------------------------
// Windows: the running exe is renamed aside

#[cfg(windows)]
#[test]
fn the_old_exe_is_moved_aside_and_removed_by_a_later_run() {
    let server = Server::start();
    let version = newer().to_string();
    server.set_latest(&newer());
    publish(&server, &version);
    let install = Install::new();

    // A stale aside file from an earlier update does not get in the way of the next one.
    let aside = install.bin().join("sliplane.old.exe");
    std::fs::write(&aside, b"stale").unwrap();
    let (code, _, err) = install.json(&server, &["update"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(install.version(), format!("sliplane {version}"));
    // This process was running from the aside file, so Windows would not let it delete it.
    assert!(aside.exists(), "the running exe cannot delete itself");
    assert_eq!(std::fs::read(&aside).unwrap(), install.original, "the aside file is the old binary");

    // The next start of a real sliplane removes it.
    let real = install.bin().join("real.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_sliplane"), &real).unwrap();
    let stale = install.bin().join("real.old.exe");
    std::fs::write(&stale, b"stale").unwrap();
    assert!(run(Command::new(&real).arg("--version")).status.success());
    assert!(!stale.exists(), "a stale sliplane.old.exe is removed on the next run");
}
