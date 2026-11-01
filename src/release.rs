//! What `sliplane update` needs to find, download and trust a release: the version of the latest
//! one, the archive for this platform, its checksum, and the one executable inside it.
//!
//! The release layout is a contract shared with `scripts/install.sh`, `scripts/install.ps1` and the
//! release workflow (AGENTS.md, "Releasing"). This module talks to the releases page only - never
//! to the Sliplane API, so no API key is in reach of it.

use std::io::{Cursor, Read};
use std::time::Duration;

use semver::Version;
use sha2::{Digest, Sha256};
use ureq::Agent;

use crate::error::{Error, ErrorCode, Result};

const DEFAULT_RELEASES: &str = "https://github.com/SpaceCorps/Sliplane-Cli/releases";

/// Upper bounds on what a release server can make this process hold in memory.
pub const MAX_ARCHIVE: u64 = 128 * 1024 * 1024;
pub const MAX_BINARY: u64 = 128 * 1024 * 1024;
pub const MAX_CHECKSUMS: u64 = 1024 * 1024;

/// The target triple of the release archive for a platform, as `std::env::consts` names it.
/// Linux builds are the static musl ones. Windows on ARM has no native release; the installer
/// hands those machines the x64 build, but `update` only replaces a binary with its own kind, so
/// a native ARM64 build made from source gets no answer here.
pub fn target_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

/// The target of the running binary, decided when it was compiled.
pub fn target() -> Option<&'static str> {
    target_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// Where to find one release's archive and the executable inside it.
#[derive(Debug, PartialEq, Eq)]
pub struct Asset {
    /// `sliplane-v1.3.0-x86_64-unknown-linux-musl.tar.gz`
    pub archive: String,
    /// `sliplane-v1.3.0-x86_64-unknown-linux-musl/sliplane`
    pub binary: String,
    pub zip: bool,
}

impl Asset {
    pub fn new(version: &Version, target: &str) -> Asset {
        let windows = target.contains("windows");
        let package = format!("sliplane-v{version}-{target}");
        Asset {
            archive: format!("{package}{}", if windows { ".zip" } else { ".tar.gz" }),
            binary: format!("{package}/sliplane{}", if windows { ".exe" } else { "" }),
            zip: windows,
        }
    }
}

/// `1.3.0` or `1.3.0-rc.1`. Build metadata is refused: it is not part of any release name, and the
/// version ends up in URLs.
fn plain_version(text: &str) -> Option<Version> {
    Version::parse(text).ok().filter(|v| v.build.is_empty())
}

/// Parses `v1.3.0` or `1.3.0`, pre-releases such as `1.3.0-rc.1` included.
pub fn parse_version(text: &str) -> Result<Version> {
    plain_version(text.strip_prefix('v').unwrap_or(text)).ok_or_else(|| {
        Error::invalid(format!("'{text}' is not a release version."))
            .fix("Use a version such as v1.3.0. 'sliplane update --check' shows the latest one.")
    })
}

/// The version in a `.../releases/tag/v1.3.0` redirect target. Only the version is taken from it:
/// every download URL is built from the releases base, never from what the redirect says.
fn version_from_location(location: &str) -> Option<Version> {
    let path = location.split(['?', '#']).next()?;
    let (_, tag) = path.rsplit_once("/tag/")?;
    plain_version(tag.strip_prefix('v')?)
}

/// The releases page, GitHub's by default. `SLIPLANE_RELEASES_URL` points it at a mirror, the same
/// variable the installers read; it also allows plain http, which GitHub's never is.
pub struct Releases {
    base: String,
    custom: bool,
    lookup: Agent,
    download: Agent,
}

fn agent(timeout: Duration, https_only: bool, redirects: u32) -> Agent {
    // A bare agent: nothing here carries credentials, and no API key is in scope.
    Agent::config_builder()
        .timeout_global(Some(timeout))
        .timeout_connect(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .https_only(https_only)
        .max_redirects(redirects)
        .max_redirects_will_error(false)
        .user_agent(concat!("sliplane-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn network(url: &str, e: ureq::Error) -> Error {
    match e {
        ureq::Error::BodyExceedsLimit(_) => Error::other("The release server sent more data than any release holds.")
            .detail(url.to_string())
            .fix("Do not retry; report it at https://github.com/SpaceCorps/Sliplane-Cli/issues"),
        e => Error::new(ErrorCode::Network, "Could not reach the release server.")
            .detail(format!("{url}: {e}"))
            .fix("Retry once, then stop."),
    }
}

impl Releases {
    pub fn from_env() -> Releases {
        let custom = std::env::var("SLIPLANE_RELEASES_URL")
            .ok()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty());
        Releases {
            base: custom.clone().unwrap_or_else(|| DEFAULT_RELEASES.into()),
            custom: custom.is_some(),
            // The latest-release lookup reads a redirect instead of following it.
            lookup: agent(Duration::from_secs(30), custom.is_none(), 0),
            download: agent(Duration::from_secs(300), custom.is_none(), 10),
        }
    }

    /// The base URL when it is not GitHub's, so that output can say where a binary came from.
    pub fn custom_base(&self) -> Option<&str> {
        self.custom.then_some(self.base.as_str())
    }

    /// The newest published release. `/releases/latest` redirects to `/releases/tag/vX.Y.Z`;
    /// reading that redirect needs no GitHub API call, so there is no rate limit to hit. GitHub
    /// leaves pre-releases and drafts out of it.
    pub fn latest(&self) -> Result<Version> {
        let url = format!("{}/latest", self.base);
        let response = self.lookup.get(&url).call().map_err(|e| network(&url, e))?;
        let status = response.status().as_u16();
        if status == 404 {
            return Err(Error::new(ErrorCode::NotFound, "No release has been published.").detail(url));
        }
        let location = response.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("");
        match version_from_location(location) {
            Some(version) if (300..400).contains(&status) => Ok(version),
            _ => Err(Error::other("Could not tell which release is the latest.")
                .detail(format!("{url} answered HTTP {status} with location '{location}'"))
                .fix("Retry once, then name a version: sliplane update --version vX.Y.Z")),
        }
    }

    /// One file of a release. `None` means the server does not have it (HTTP 404).
    pub fn fetch(&self, version: &Version, file: &str, limit: u64) -> Result<Option<Vec<u8>>> {
        let url = format!("{}/download/v{version}/{file}", self.base);
        let mut response = self.download.get(&url).call().map_err(|e| network(&url, e))?;
        match response.status().as_u16() {
            200 => {}
            404 => return Ok(None),
            429 => {
                return Err(Error::new(ErrorCode::RateLimited, "The release server is rate limiting this machine.")
                    .detail(url)
                    .fix("Wait a minute, retry once, then stop."));
            }
            status => {
                return Err(Error::new(ErrorCode::Network, format!("The release server answered HTTP {status}."))
                    .detail(url)
                    .fix("Retry once, then stop."));
            }
        }
        let bytes = response.body_mut().with_config().limit(limit).read_to_vec().map_err(|e| network(&url, e))?;
        Ok(Some(bytes))
    }
}

/// The SHA-256 `checksums.txt` lists for `file`. The format is `sha256sum`'s: a 64-digit hash, a
/// space, an optional `*` for binary mode, then the file name. Exactly one line must name the file:
/// none means the release cannot be verified, and two would mean the list contradicts itself.
pub fn checksum_for(checksums: &str, file: &str) -> Result<String> {
    let hashes: Vec<String> = checksums
        .lines()
        .filter_map(|line| {
            let (hash, name) = line.trim_end().split_once(' ')?;
            let name = name.trim_start_matches([' ', '*']);
            (name == file && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| hash.to_ascii_lowercase())
        })
        .collect();
    match hashes.as_slice() {
        [hash] => Ok(hash.clone()),
        [] => Err(Error::other(format!("checksums.txt has no checksum for {file}, so it cannot be verified."))
            .fix("Do not install it. Retry later, or name another release: sliplane update --version vX.Y.Z")),
        _ => Err(Error::other(format!("checksums.txt lists {file} more than once, with {} entries.", hashes.len()))
            .fix("Do not install it; report it at https://github.com/SpaceCorps/Sliplane-Cli/issues")),
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn bad_archive(asset: &Asset, why: impl std::fmt::Display) -> Error {
    Error::other(format!("{} is not a usable release archive: {why}.", asset.archive))
        .fix("Do not install it; report it at https://github.com/SpaceCorps/Sliplane-Cli/issues")
}

/// Whether an archive entry is the executable. The comparison is on the name alone and exact (a
/// leading `./` and, in zips, `\` are tolerated): nothing else in the archive is read, let alone
/// written, so an entry named `../../x` or `/etc/x` is skipped like any other stray file.
fn is_binary(entry_name: &[u8], expected: &str) -> bool {
    let mut name = entry_name;
    while let Some(rest) = name.strip_prefix(b"./") {
        name = rest;
    }
    name == expected.as_bytes()
}

/// Reads the executable out of the archive, and nothing else. It must be a regular file, there
/// must be exactly one of it, and it must be of a believable size. The README and LICENSE that
/// ship beside it are ignored.
pub fn extract_binary(archive: &[u8], asset: &Asset) -> Result<Vec<u8>> {
    if asset.zip { from_zip(archive, asset) } else { from_tar_gz(archive, asset) }
}

fn read_limited(reader: impl Read, asset: &Asset) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    reader.take(MAX_BINARY + 1).read_to_end(&mut buf).map_err(|e| bad_archive(asset, e))?;
    if buf.len() as u64 > MAX_BINARY {
        return Err(bad_archive(asset, "the executable is implausibly large"));
    }
    if buf.is_empty() {
        return Err(bad_archive(asset, "the executable is empty"));
    }
    Ok(buf)
}

fn from_tar_gz(archive: &[u8], asset: &Asset) -> Result<Vec<u8>> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(Cursor::new(archive)));
    let mut found = None;
    for entry in tar.entries().map_err(|e| bad_archive(asset, e))? {
        let mut entry = entry.map_err(|e| bad_archive(asset, e))?;
        if !is_binary(&entry.path_bytes(), &asset.binary) {
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err(bad_archive(asset, format!("{} is not a regular file", asset.binary)));
        }
        if found.replace(read_limited(&mut entry, asset)?).is_some() {
            return Err(bad_archive(asset, format!("it holds {} twice", asset.binary)));
        }
    }
    found.ok_or_else(|| bad_archive(asset, format!("it does not hold {}", asset.binary)))
}

fn from_zip(archive: &[u8], asset: &Asset) -> Result<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(archive)).map_err(|e| bad_archive(asset, e))?;
    let mut found = None;
    for i in 0..zip.len() {
        let entry = zip.by_index(i).map_err(|e| bad_archive(asset, e))?;
        let name: Vec<u8> = entry.name_raw().iter().map(|&b| if b == b'\\' { b'/' } else { b }).collect();
        if !is_binary(&name, &asset.binary) {
            continue;
        }
        if !entry.is_file() || entry.is_symlink() {
            return Err(bad_archive(asset, format!("{} is not a regular file", asset.binary)));
        }
        if found.replace(read_limited(entry, asset)?).is_some() {
            return Err(bad_archive(asset, format!("it holds {} twice", asset.binary)));
        }
    }
    found.ok_or_else(|| bad_archive(asset, format!("it does not hold {}", asset.binary)))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn v(text: &str) -> Version {
        parse_version(text).unwrap()
    }

    #[test]
    fn versions_parse_with_or_without_v() {
        assert_eq!(v("v1.3.0"), v("1.3.0"));
        assert_eq!(v("1.3.0-rc.1").pre.as_str(), "rc.1");
        for bad in ["", "v", "1.3", "1.3.0.1", "latest", "v1.03.0", "1.3.0+build5", "V1.3.0", "../1.3.0", "1.3.0/x"] {
            let e = parse_version(bad).unwrap_err();
            assert_eq!(e.code, ErrorCode::InvalidInput, "{bad}");
            assert!(e.remediation.is_some(), "{bad}");
        }
    }

    #[test]
    fn versions_compare_as_semver_not_as_text() {
        assert!(v("1.10.0") > v("1.9.0"));
        assert!(v("1.3.0") > v("1.3.0-rc.1"));
        assert!(v("1.3.0-rc.2") > v("1.3.0-rc.1"));
        assert!(v("1.3.0-rc.1") > v("1.2.9"));
        assert!(v("2.0.0") > v("1.99.99"));
        assert_eq!(v("1.3.0"), v("v1.3.0"));
    }

    #[test]
    fn the_latest_redirect_yields_only_a_version() {
        let ok = |l: &str| version_from_location(l);
        let tag = Some(v("1.3.0"));
        assert_eq!(ok("https://github.com/SpaceCorps/Sliplane-Cli/releases/tag/v1.3.0"), tag);
        assert_eq!(ok("/SpaceCorps/Sliplane-Cli/releases/tag/v1.3.0"), tag);
        assert_eq!(ok("http://127.0.0.1:1/tag/v1.3.0?x=1#y"), tag);
        assert_eq!(ok("http://h/tag/v1.3.0-rc.1"), Some(v("1.3.0-rc.1")));
        for bad in [
            "",
            "https://github.com/x/y/releases",
            "https://github.com/x/y/releases/tag/",
            "https://github.com/x/y/releases/tag/1.3.0",
            "https://github.com/x/y/releases/tag/vnext",
            "https://github.com/x/y/releases/tag/v1.3.0/../../evil",
            "https://github.com/x/y/releases/tag/v1.3.0+evil",
        ] {
            assert_eq!(ok(bad), None, "{bad}");
        }
    }

    #[test]
    fn targets_follow_the_release_contract() {
        assert_eq!(target_for("macos", "aarch64"), Some("aarch64-apple-darwin"));
        assert_eq!(target_for("macos", "x86_64"), Some("x86_64-apple-darwin"));
        assert_eq!(target_for("linux", "x86_64"), Some("x86_64-unknown-linux-musl"));
        assert_eq!(target_for("linux", "aarch64"), Some("aarch64-unknown-linux-musl"));
        assert_eq!(target_for("windows", "x86_64"), Some("x86_64-pc-windows-msvc"));
        for (os, arch) in [("windows", "aarch64"), ("linux", "riscv64"), ("freebsd", "x86_64"), ("linux", "x86")] {
            assert_eq!(target_for(os, arch), None, "{os}/{arch}");
        }
        // The running platform is one the installers know, or the CI matrix has grown.
        assert!(target().is_some(), "no release target for {}/{}", std::env::consts::OS, std::env::consts::ARCH);
    }

    #[test]
    fn asset_names_follow_the_release_contract() {
        let unix = Asset::new(&v("1.3.0"), "x86_64-unknown-linux-musl");
        assert_eq!(unix.archive, "sliplane-v1.3.0-x86_64-unknown-linux-musl.tar.gz");
        assert_eq!(unix.binary, "sliplane-v1.3.0-x86_64-unknown-linux-musl/sliplane");
        assert!(!unix.zip);
        let windows = Asset::new(&v("1.3.0-rc.1"), "x86_64-pc-windows-msvc");
        assert_eq!(windows.archive, "sliplane-v1.3.0-rc.1-x86_64-pc-windows-msvc.zip");
        assert_eq!(windows.binary, "sliplane-v1.3.0-rc.1-x86_64-pc-windows-msvc/sliplane.exe");
        assert!(windows.zip);
    }

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn checksum_lines_follow_sha256sum() {
        let file = "sliplane-v1.3.0-x86_64-unknown-linux-musl.tar.gz";
        let other = format!("{} install.sh\n", "f".repeat(64));
        let text_mode = format!("{other}{HASH}  {file}\n");
        assert_eq!(checksum_for(&text_mode, file).unwrap(), HASH);
        let binary_mode = format!("{other}{HASH} *{file}\n");
        assert_eq!(checksum_for(&binary_mode, file).unwrap(), HASH);
        let crlf = format!("{}  {file}\r\n{other}", HASH.to_uppercase());
        assert_eq!(checksum_for(&crlf, file).unwrap(), HASH, "upper case and CRLF are accepted");
    }

    #[test]
    fn a_checksum_that_is_missing_or_ambiguous_is_refused() {
        let file = "a.tar.gz";
        for (text, why) in [
            ("".to_string(), "empty"),
            (format!("{HASH}  b.tar.gz\n"), "only another file"),
            (format!("{HASH}  a.tar.gz.sig\n{HASH}  xa.tar.gz\n"), "only look-alike names"),
            (format!("{}  a.tar.gz\n", &HASH[..63]), "hash too short"),
            (format!("{}x  a.tar.gz\n", &HASH[..63]), "hash not hex"),
            (format!("{HASH}  a.tar.gz\n{HASH}  a.tar.gz\n"), "listed twice"),
            (format!("{HASH}  a.tar.gz\n{}  a.tar.gz\n", "e".repeat(64)), "listed twice, disagreeing"),
        ] {
            let e = checksum_for(&text, file).unwrap_err();
            assert_eq!(e.code, ErrorCode::Error, "{why}");
            assert!(e.remediation.is_some(), "{why}");
        }
    }

    #[test]
    fn sha256_matches_the_known_answer() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    // --- archives ---------------------------------------------------------------------------

    const EXPECTED: &str = "sliplane-v1.3.0-x86_64-unknown-linux-musl/sliplane";

    fn asset(zip: bool) -> Asset {
        let target = if zip { "x86_64-pc-windows-msvc" } else { "x86_64-unknown-linux-musl" };
        Asset::new(&v("1.3.0"), target)
    }

    /// A tar entry with its name written into the header as raw bytes, so that names a well-behaved
    /// writer refuses (`..`, absolute paths) can be put in.
    fn tar_entry(builder: &mut tar::Builder<Vec<u8>>, name: &str, kind: tar::EntryType, data: &[u8], link: &str) {
        let mut header = tar::Header::new_gnu();
        header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        if !link.is_empty() {
            header.as_old_mut().linkname[..link.len()].copy_from_slice(link.as_bytes());
        }
        header.set_entry_type(kind);
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append(&header, data).unwrap();
    }

    fn tar_gz(entries: &[(&str, tar::EntryType, &[u8], &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, kind, data, link) in entries {
            tar_entry(&mut builder, name, *kind, data, link);
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&builder.into_inner().unwrap()).unwrap();
        gz.finish().unwrap()
    }

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    const FILE: tar::EntryType = tar::EntryType::Regular;

    #[test]
    fn the_binary_is_extracted_from_a_tar_gz_and_the_rest_is_ignored() {
        let archive = tar_gz(&[
            ("sliplane-v1.3.0-x86_64-unknown-linux-musl/", tar::EntryType::Directory, b"", ""),
            ("sliplane-v1.3.0-x86_64-unknown-linux-musl/README.md", FILE, b"readme", ""),
            (EXPECTED, FILE, b"the binary", ""),
            ("sliplane-v1.3.0-x86_64-unknown-linux-musl/LICENSE", FILE, b"mit", ""),
        ]);
        assert_eq!(extract_binary(&archive, &asset(false)).unwrap(), b"the binary");
        let dotted = tar_gz(&[("./sliplane-v1.3.0-x86_64-unknown-linux-musl/sliplane", FILE, b"dotted", "")]);
        assert_eq!(extract_binary(&dotted, &asset(false)).unwrap(), b"dotted");
    }

    #[test]
    fn a_hostile_tar_gz_yields_nothing_it_was_not_asked_for() {
        // Entries that try to land elsewhere are never the executable, so they are skipped unread.
        let escaping = tar_gz(&[
            ("../sliplane", FILE, b"evil", ""),
            ("../../sliplane-v1.3.0-x86_64-unknown-linux-musl/sliplane", FILE, b"evil", ""),
            ("/sliplane-v1.3.0-x86_64-unknown-linux-musl/sliplane", FILE, b"evil", ""),
            ("sliplane-v1.3.0-x86_64-unknown-linux-musl/../sliplane", FILE, b"evil", ""),
            ("sliplane-v1.3.0-x86_64-unknown-linux-musl/sub/sliplane", FILE, b"evil", ""),
            ("etc/passwd", FILE, b"evil", ""),
        ]);
        let e = extract_binary(&escaping, &asset(false)).unwrap_err();
        assert!(e.message.contains("does not hold"), "{}", e.message);

        // The right name with the wrong kind of entry.
        let symlink = tar_gz(&[(EXPECTED, tar::EntryType::Symlink, b"", "/bin/sh")]);
        assert!(extract_binary(&symlink, &asset(false)).unwrap_err().message.contains("not a regular file"));
        let hardlink = tar_gz(&[(EXPECTED, tar::EntryType::Link, b"", "etc/passwd")]);
        assert!(extract_binary(&hardlink, &asset(false)).unwrap_err().message.contains("not a regular file"));

        // Ambiguity is refused, not resolved by picking the first or the last.
        let twice = tar_gz(&[(EXPECTED, FILE, b"one", ""), (EXPECTED, FILE, b"two", "")]);
        assert!(extract_binary(&twice, &asset(false)).unwrap_err().message.contains("twice"));

        let empty = tar_gz(&[(EXPECTED, FILE, b"", "")]);
        assert!(extract_binary(&empty, &asset(false)).unwrap_err().message.contains("empty"));
    }

    #[test]
    fn something_that_is_not_an_archive_is_an_error_not_a_panic() {
        for junk in [&b""[..], b"not gzip at all", &[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3, 1, 2, 3]] {
            assert!(extract_binary(junk, &asset(false)).is_err());
            assert!(extract_binary(junk, &asset(true)).is_err());
        }
    }

    #[test]
    fn the_binary_is_extracted_from_a_zip_and_the_rest_is_ignored() {
        let name = "sliplane-v1.3.0-x86_64-pc-windows-msvc/sliplane.exe";
        let archive = zip_of(&[("sliplane-v1.3.0-x86_64-pc-windows-msvc/README.md", b"readme"), (name, b"MZ binary")]);
        assert_eq!(extract_binary(&archive, &asset(true)).unwrap(), b"MZ binary");
        // 7-Zip on Windows may write backslashes.
        let backslashes = zip_of(&[("sliplane-v1.3.0-x86_64-pc-windows-msvc\\sliplane.exe", b"MZ binary")]);
        assert_eq!(extract_binary(&backslashes, &asset(true)).unwrap(), b"MZ binary");
    }

    #[test]
    fn a_hostile_zip_yields_nothing_it_was_not_asked_for() {
        let escaping = zip_of(&[
            ("../sliplane.exe", b"evil"),
            ("..\\..\\sliplane-v1.3.0-x86_64-pc-windows-msvc\\sliplane.exe", b"evil"),
            ("C:/sliplane.exe", b"evil"),
            ("/sliplane-v1.3.0-x86_64-pc-windows-msvc/sliplane.exe", b"evil"),
            ("sliplane-v1.3.0-x86_64-pc-windows-msvc/sub/sliplane.exe", b"evil"),
        ]);
        let e = extract_binary(&escaping, &asset(true)).unwrap_err();
        assert!(e.message.contains("does not hold"), "{}", e.message);

        // The zip writer refuses duplicate names, so the "twice" case is only covered for tar.
        let directory = zip_of(&[("sliplane-v1.3.0-x86_64-pc-windows-msvc/sliplane.exe/", b"")]);
        assert!(extract_binary(&directory, &asset(true)).is_err());
    }
}
