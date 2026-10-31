//! `sliplane s3 ...`: files in a bucket over the S3 protocol, and the stored key that makes that
//! possible.
//!
//! Two different credentials are in play and they never meet. The Sliplane API key (from the
//! account) is used to find the bucket's endpoint and to create or delete its S3 keys - requests
//! to `ctrl.sliplane.io`. The S3 key pair (made by `connect`, kept in the OS keystore) signs the
//! requests to the bucket's own endpoint. `crate::s3` is handed the second and has never heard of
//! the first.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::print;
use crate::cli::{BucketId, S3 as Cmd};
use crate::client::{Client, seg};
use crate::error::{Error, ErrorCode, Result};
use crate::s3::{Credentials, ListParams, MAX_PAGE, ObjectStore, Probe};
use crate::{config, obj, output, patharg, secrets};

/// The longest key S3 accepts, in bytes.
const MAX_KEY: usize = 1024;

pub fn run(mut c: Cmd) -> Result<()> {
    // Everything that needs no account, no keystore and no network is refused first.
    c.bucket_mut().check()?;
    match &c {
        Cmd::Ls { prefix, .. } => {
            if let Some(p) = prefix {
                patharg::check(p, "--prefix")?;
            }
        }
        Cmd::Upload { file, key, .. } => {
            local_file(Path::new(file))?;
            if let Some(k) = key {
                key_ok(k, "--key")?;
            }
        }
        Cmd::Download { key, .. } => key_ok(key, "--key")?,
        Cmd::Rm { keys, .. } => keys.iter().try_for_each(|k| key_ok(k, "--key"))?,
        Cmd::Connect { .. } | Cmd::Disconnect(_) => {}
    }

    match c {
        Cmd::Connect { mut s, timeout } => connect(&mut s.b, &s.region, timeout),
        Cmd::Disconnect(mut b) => disconnect(&mut b),
        Cmd::Ls { mut s, prefix, recursive, limit, token } => {
            ls(&mut s.b, &s.region, prefix.as_deref().unwrap_or(""), recursive, limit, token.as_deref())
        }
        Cmd::Upload { mut s, file, key, content_type, force } => {
            upload(&mut s.b, &s.region, Path::new(&file), key, &content_type, force)
        }
        Cmd::Download { mut s, key, out, force } => download(&mut s.b, &s.region, &key, out.as_deref(), force),
        Cmd::Rm { mut s, keys } => rm(&mut s.b, &s.region, keys),
    }
}

// ---------------------------------------------------------------------------------------------
// what is stored, and which bucket

/// What `connect` keeps in the keystore, as JSON, under [`secrets::s3_key`].
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    /// Sliplane's id for the key, so it can be revoked: `buckets delete-key --key-id`.
    key_id: String,
    access_key_id: String,
    secret_access_key: String,
}

impl Stored {
    fn credentials(&self) -> Credentials {
        Credentials { access_key_id: self.access_key_id.clone(), secret_access_key: self.secret_access_key.clone() }
    }
}

struct Target {
    /// The account's name as configured.
    account: String,
    bucket_id: String,
    bucket_name: String,
    endpoint: String,
}

impl Target {
    fn entry(&self) -> String {
        secrets::s3_key(&self.account, &self.bucket_id)
    }

    fn connect_command(&self) -> String {
        format!("sliplane s3 connect -a {} --bucket-id {}", self.account, self.bucket_id)
    }
}

/// Resolves the account and the bucket (a name costs a listing, as everywhere) and reads the
/// bucket's endpoint from the API. Returns the API client too, which `connect` needs again.
fn locate(b: &mut BucketId) -> Result<(Target, Client)> {
    let account = super::resolve(&b.a)?;
    let client = account.client();
    if b.needs_lookup() {
        b.resolve(&client)?;
    }
    let listing = client.get("buckets")?;
    let found = listing
        .as_array()
        .and_then(|all| all.iter().find(|x| x.get("id").and_then(Value::as_str) == Some(b.bucket_id.as_str())))
        .ok_or_else(|| {
            Error::new(ErrorCode::NotFound, format!("No bucket has the id '{}'.", b.bucket_id))
                .fix(format!("sliplane buckets list -a {}", account.name))
        })?;
    let text = |k: &str| found.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let status = text("status");
    if !status.is_empty() && status != "active" {
        return Err(Error::invalid(format!("The bucket '{}' is {status}, not active.", text("name"))));
    }
    let endpoint = text("endpoint");
    if endpoint.is_empty() {
        return Err(Error::other(format!("The API gave no endpoint for the bucket '{}'.", text("name"))));
    }
    // Before a key is made for it: a bucket whose endpoint cannot be used should cost nothing.
    crate::s3::check_endpoint(&endpoint)?;
    Ok((Target { account: account.name, bucket_id: text("id"), bucket_name: text("name"), endpoint }, client))
}

fn read_stored(t: &Target) -> Result<Option<Stored>> {
    let Some(raw) = secrets::store()?.get(&t.entry())?.filter(|s| !s.trim().is_empty()) else {
        return Ok(None);
    };
    let stored: Stored = serde_json::from_str(&raw)
        .ok()
        .filter(|s: &Stored| !s.access_key_id.is_empty() && !s.secret_access_key.is_empty())
        .ok_or_else(|| {
            Error::other(format!("The stored S3 key for bucket '{}' is unreadable.", t.bucket_name)).fix(format!(
                "Forget it and make a new one: sliplane s3 disconnect -a {a} --bucket-id {id} && {}",
                t.connect_command(),
                a = t.account,
                id = t.bucket_id
            ))
        })?;
    Ok(Some(stored))
}

/// The bucket's store, signed with the key `connect` kept. Never creates a key: an explicit
/// `connect` does that, so a key is never made as a side effect of listing files.
fn open(t: &Target, region: &str) -> Result<ObjectStore> {
    let stored = read_stored(t)?.ok_or_else(|| {
        Error::new(
            ErrorCode::AuthRequired,
            format!("No S3 key is stored for bucket '{}' on account '{}'.", t.bucket_name, t.account),
        )
        .detail("Files are read and written with an S3 key of their own, made once per bucket.")
        .fix(t.connect_command())
    })?;
    ObjectStore::new(&t.endpoint, region, stored.credentials())
}

/// An S3 refusal of the key gets the same way out, whatever the operation was.
fn guard(e: Error, t: &Target) -> Error {
    if e.code == ErrorCode::AuthRequired && e.remediation.is_none() {
        return e.fix(format!(
            "A key made a moment ago may not be active yet: retry shortly. If it was revoked: \
             sliplane s3 disconnect -a {a} --bucket-id {id} && {}",
            t.connect_command(),
            a = t.account,
            id = t.bucket_id
        ));
    }
    e
}

// ---------------------------------------------------------------------------------------------
// arguments

fn key_ok(key: &str, option: &str) -> Result<()> {
    if key.is_empty() {
        return Err(Error::invalid(format!("{option} must not be empty.")));
    }
    if key.len() > MAX_KEY {
        return Err(Error::invalid(format!("{option} is {} bytes; S3 keys are at most {MAX_KEY}.", key.len())));
    }
    // A key starting with "/" is mangled by Git Bash before this program sees it.
    patharg::check(key, option)?;
    Ok(())
}

/// The file to upload exists and is a regular file. Cheap, so it is checked before any request.
fn local_file(path: &Path) -> Result<u64> {
    let meta = fs::metadata(path).map_err(|e| Error::invalid(format!("Could not read {}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(Error::invalid(format!("{} is not a regular file.", path.display())));
    }
    Ok(meta.len())
}

/// Entries per request. Only the tests change it, to see a listing continue without 1001 objects.
fn page_size() -> usize {
    std::env::var("SLIPLANE_S3_PAGE_SIZE")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .map_or(MAX_PAGE, |n| n.clamp(1, MAX_PAGE))
}

// ---------------------------------------------------------------------------------------------
// connect, disconnect

fn connect(b: &mut BucketId, region: &str, timeout: u64) -> Result<()> {
    let (t, client) = locate(b)?;
    let store = secrets::store()?;

    let (stored, reused) = match read_stored(&t)? {
        Some(s) => (s, true),
        None => (create_key(&client, &t, store)?, false),
    };

    let objects = ObjectStore::new(&t.endpoint, region, stored.credentials())?;
    wait_until_active(&objects, &t, timeout)?;

    print(obj! {
        "status" => "connected",
        "reused" => reused,
        "bucket" => t.bucket_name,
        "bucketId" => t.bucket_id,
        "endpoint" => t.endpoint,
        "keyId" => stored.key_id,
        "accessKeyId" => stored.access_key_id,
        "secretStore" => store.name(),
        "note" => "The secret key is in the OS keystore and is never printed. 'sliplane s3 disconnect' forgets it locally \
                   without revoking it; revoke it with 'sliplane buckets delete-key'.",
    })
}

/// Makes a key through the API and keeps it. If it cannot be kept, it is revoked again: a key
/// whose secret was never stored is a credential nobody can use and nobody will remember.
fn create_key(client: &Client, t: &Target, store: secrets::Store) -> Result<Stored> {
    let body = json!({ "name": format!("sliplane-cli ({})", t.account) });
    let created = client.post(&format!("buckets/{}/keys", seg(&t.bucket_id)), &body)?;
    let text = |k: &str| created.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let stored =
        Stored { key_id: text("id"), access_key_id: text("accessKeyId"), secret_access_key: text("secretAccessKey") };

    if stored.access_key_id.is_empty() || stored.secret_access_key.is_empty() {
        let revoked = revoke(client, t, &stored.key_id);
        return Err(Error::other("The API created a key but did not return a usable key pair.")
            .detail(format!("Expected id, accessKeyId and secretAccessKey. {revoked}")));
    }

    let value = serde_json::to_string(&stored).expect("a struct of strings serializes");
    // Two `connect`s at once would each make a key and the later write would hide the earlier key
    // for good; so the check and the write are one step, and the loser revokes its own key.
    let kept = (|| -> Result<bool> {
        let _lock = config::lock()?;
        if store.get(&t.entry())?.is_some_and(|s| !s.trim().is_empty()) {
            return Ok(false);
        }
        store.set(&t.entry(), &value)?;
        Ok(true)
    })();

    match kept {
        Ok(true) => Ok(stored),
        Ok(false) => {
            let _ = revoke(client, t, &stored.key_id);
            read_stored(t)?.ok_or_else(|| Error::other("The stored S3 key vanished while it was being read."))
        }
        Err(e) => {
            let revoked = revoke(client, t, &stored.key_id);
            let detail = format!("{}{revoked}", e.detail.map(|d| format!("{d} ")).unwrap_or_default());
            let mut out = Error::other(format!("The new S3 key could not be stored: {}", e.message)).detail(detail);
            if let Some(fix) = e.remediation {
                out = out.fix(fix);
            }
            Err(out)
        }
    }
}

/// Deletes a key at Sliplane; says in words how that went, for an error's detail.
fn revoke(client: &Client, t: &Target, key_id: &str) -> String {
    if key_id.is_empty() {
        return "The key's id was not returned, so it could not be revoked: check 'sliplane buckets keys'.".into();
    }
    match client.delete(&format!("buckets/{}/keys/{}", seg(&t.bucket_id), seg(key_id))) {
        Ok(_) => format!("The unused key {key_id} was revoked."),
        Err(e) => format!("The unused key {key_id} could not be revoked ({}): sliplane buckets delete-key.", e.message),
    }
}

/// A new key is not accepted at once: the storage endpoint refuses it as unknown or as not
/// allowed until it has caught up. Anything else is a real answer and ends the wait.
fn wait_until_active(objects: &ObjectStore, t: &Target, timeout: u64) -> Result<()> {
    let start = Instant::now();
    let limit = Duration::from_secs(timeout);
    let mut last;
    loop {
        match objects.probe(&t.bucket_name).map_err(|e| guard(e, t))? {
            Probe::Ready => return Ok(()),
            Probe::Pending(e) => last = e,
        }
        if start.elapsed() + Duration::from_secs(1) > limit {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err(Error::other(format!("The S3 key is stored but did not become usable within {timeout}s."))
        .detail(last.detail.unwrap_or(last.message))
        .fix(format!("Wait a minute, then repeat {} (it reuses the stored key and waits again).", t.connect_command())))
}

fn disconnect(b: &mut BucketId) -> Result<()> {
    // With --bucket-id this needs no network at all, so it still works for a bucket that is gone.
    let account = super::resolve(&b.a)?;
    if b.needs_lookup() {
        b.resolve(&account.client())?;
    }
    let entry = secrets::s3_key(&account.name, &b.bucket_id);
    let store = secrets::store()?;
    let stored =
        store.get(&entry)?.filter(|s| !s.trim().is_empty()).and_then(|s| serde_json::from_str::<Stored>(&s).ok());

    {
        let _lock = config::lock()?;
        store.delete(&entry)?;
    }

    let Some(stored) = stored else {
        return print(obj! {
            "status" => "not_connected",
            "bucketId" => b.bucket_id,
            "note" => "No S3 key was stored for this bucket on this account, so nothing was removed.",
        });
    };
    print(obj! {
        "status" => "disconnected",
        "bucketId" => b.bucket_id,
        "keyId" => stored.key_id,
        "accessKeyId" => stored.access_key_id,
        // The key is gone from this machine, not from Sliplane; anywhere else it was copied it
        // still works until it is deleted there.
        "note" => format!(
            "The S3 key was deleted locally only. It still works until it is revoked: sliplane buckets delete-key -a {} --bucket-id {} --key-id {}",
            account.name, b.bucket_id, stored.key_id
        ),
    })
}

// ---------------------------------------------------------------------------------------------
// files

fn ls(b: &mut BucketId, region: &str, prefix: &str, recursive: bool, limit: usize, token: Option<&str>) -> Result<()> {
    let (t, _) = locate(b)?;
    let objects = open(&t, region)?;
    let delimiter = (!recursive).then_some("/");
    let page_size = page_size();

    let mut files = Vec::new();
    let mut folders = Vec::new();
    let mut token = token.map(str::to_string);
    let mut more = None;
    loop {
        let collected = files.len() + folders.len();
        let max_keys = if limit == 0 { page_size } else { page_size.min(limit - collected) };
        let page = objects
            .list(&t.bucket_name, &ListParams { prefix, delimiter, token: token.as_deref(), max_keys })
            .map_err(|e| guard(e, &t))?;
        for o in page.objects {
            files.push(obj! { "key" => o.key, "size" => o.size, "lastModified" => o.last_modified, "etag" => o.etag });
        }
        folders.extend(page.prefixes);

        let Some(next) = page.next_token else { break };
        if token.as_deref() == Some(next.as_str()) {
            return Err(Error::other("The storage endpoint returned the same continuation token twice.")
                .detail("Stopping rather than listing the same page forever."));
        }
        token = Some(next.clone());
        if limit != 0 && files.len() + folders.len() >= limit {
            more = Some(next);
            break;
        }
    }

    let mut out = obj! {
        "bucket" => t.bucket_name,
        "prefix" => prefix,
        "count" => files.len(),
        "objects" => files,
        "prefixes" => folders,
        "truncated" => more.is_some(),
    };
    if let Some(next) = more {
        out["nextToken"] = json!(next);
        out["next"] =
            json!("Repeat the command with --token <nextToken> (and the same --prefix and --recursive) for the rest.");
    }
    print(out)
}

fn upload(
    b: &mut BucketId,
    region: &str,
    file: &Path,
    key: Option<String>,
    content_type: &str,
    force: bool,
) -> Result<()> {
    let key = match key {
        Some(k) => k,
        None => {
            let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            key_ok(&name, "the file name").map_err(|e| e.fix("Name the object with --key <key>."))?;
            name
        }
    };
    let (t, _) = locate(b)?;
    let objects = open(&t, region)?;

    if !force && objects.exists(&t.bucket_name, &key).map_err(|e| guard(e, &t))? {
        return Err(Error::invalid(format!("The object '{key}' already exists in '{}'.", t.bucket_name))
            .fix("Pick another --key, or add --force if replacing it is what the human wants."));
    }
    let done = objects.put_file(&t.bucket_name, &key, file, content_type).map_err(|e| guard(e, &t))?;
    print(obj! {
        "status" => "uploaded",
        "bucket" => t.bucket_name,
        "key" => key,
        "size" => done.size,
        "etag" => done.etag,
    })
}

enum Dest {
    Stdout,
    File(PathBuf),
}

fn destination(key: &str, out: Option<&str>) -> Result<Dest> {
    let name = key.replace('\\', "/").rsplit('/').next().unwrap_or("").to_string();
    let named = || {
        if matches!(name.as_str(), "" | "." | "..") {
            Err(Error::invalid(format!("The key '{key}' has no file name to save under."))
                .fix("Say where to write it: --out <path>."))
        } else {
            Ok(PathBuf::from(&name))
        }
    };
    Ok(match out {
        Some("-") => Dest::Stdout,
        Some(p) if Path::new(p).is_dir() => Dest::File(Path::new(p).join(named()?)),
        Some(p) => Dest::File(PathBuf::from(p)),
        None => Dest::File(named()?),
    })
}

/// A half-written download, removed unless it was moved into place.
struct Partial(Option<PathBuf>);

impl Partial {
    /// The file was moved to its real name; there is nothing left to clean up.
    fn keep(mut self) {
        self.0 = None;
    }
}

impl Drop for Partial {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

fn download(b: &mut BucketId, region: &str, key: &str, out: Option<&str>, force: bool) -> Result<()> {
    let dest = destination(key, out)?;
    if let Dest::File(path) = &dest
        && !force
        && fs::symlink_metadata(path).is_ok()
    {
        return Err(exists(path));
    }
    let (t, _) = locate(b)?;
    let objects = open(&t, region)?;

    match dest {
        Dest::Stdout => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let size = objects.get(&t.bucket_name, key, &mut lock).map_err(|e| guard(e, &t))?;
            lock.flush()?;
            // Stdout is the object, so the receipt goes to stderr.
            eprintln!(
                "{}",
                output::render(
                    &obj! { "status" => "downloaded", "bucket" => t.bucket_name, "key" => key, "path" => "-", "size" => size }
                )
            );
            Ok(())
        }
        Dest::File(path) => {
            let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("download");
            let tmp = dir.join(format!(".{name}.sliplane-{}.part", std::process::id()));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .map_err(|e| Error::invalid(format!("Could not create {}: {e}", tmp.display())))?;
            let partial = Partial(Some(tmp.clone()));

            let size = objects.get(&t.bucket_name, key, &mut file).map_err(|e| guard(e, &t))?;
            file.flush()?;
            drop(file);

            move_into_place(&tmp, &path, force)?;
            partial.keep();
            print(obj! {
                "status" => "downloaded",
                "bucket" => t.bucket_name,
                "key" => key,
                "path" => path.display().to_string(),
                "size" => size,
            })
        }
    }
}

fn exists(path: &Path) -> Error {
    Error::invalid(format!("{} already exists.", path.display()))
        .fix("Pick another --out, or add --force if replacing it is what the human wants.")
}

/// Without `--force` a file that appears while the download runs is not overwritten: a hard link
/// fails if the name is taken, where a rename would silently replace it.
fn move_into_place(tmp: &Path, dest: &Path, force: bool) -> Result<()> {
    if force {
        return Ok(fs::rename(tmp, dest)?);
    }
    match fs::hard_link(tmp, dest) {
        Ok(()) => {
            let _ = fs::remove_file(tmp);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(tmp);
            Err(exists(dest))
        }
        // A filesystem without hard links: the best that is left is to look, then rename.
        Err(_) if fs::symlink_metadata(dest).is_err() => Ok(fs::rename(tmp, dest)?),
        Err(_) => {
            let _ = fs::remove_file(tmp);
            Err(exists(dest))
        }
    }
}

fn rm(b: &mut BucketId, region: &str, mut keys: Vec<String>) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    keys.retain(|k| seen.insert(k.clone()));

    let (t, _) = locate(b)?;
    let objects = open(&t, region)?;

    // S3 answers a delete of a missing key with success, which would hide a typo. So every key is
    // looked up first, and nothing is deleted unless all of them exist.
    let mut missing = Vec::new();
    for k in &keys {
        if !objects.exists(&t.bucket_name, k).map_err(|e| guard(e, &t))? {
            missing.push(k.as_str());
        }
    }
    if !missing.is_empty() {
        return Err(Error::new(
            ErrorCode::NotFound,
            format!(
                "{} of {} objects do not exist in '{}'. Nothing was deleted.",
                missing.len(),
                keys.len(),
                t.bucket_name
            ),
        )
        .detail(format!("Missing: {}", missing.join(", ")))
        .fix(format!("sliplane s3 ls -a {} --bucket-id {} --prefix <prefix>", t.account, t.bucket_id)));
    }
    for k in &keys {
        objects.delete(&t.bucket_name, k).map_err(|e| guard(e, &t))?;
    }
    print(obj! { "status" => "deleted", "bucket" => t.bucket_name, "count" => keys.len(), "keys" => keys })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_checked_before_anything_is_sent() {
        assert!(key_ok("reports/2026/q3.pdf", "--key").is_ok());
        assert!(key_ok("caf\u{e9} \u{1f600}+x", "--key").is_ok());
        assert!(key_ok("", "--key").is_err());
        assert!(key_ok(&"k".repeat(1025), "--key").is_err());
        assert!(key_ok(&"k".repeat(1024), "--key").is_ok());
        // What Git Bash makes of a key that began with "/".
        let e = key_ok("C:/Program Files/Git/logs/a.txt", "--key").unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidInput);
        assert!(e.message.contains("Git installation directory"), "{}", e.message);
    }

    #[test]
    fn download_names_the_file_after_the_key() {
        let file = |d: Dest| match d {
            Dest::File(p) => p.display().to_string(),
            Dest::Stdout => "<stdout>".into(),
        };
        assert_eq!(file(destination("a/b/report.pdf", None).unwrap()), "report.pdf");
        assert_eq!(file(destination("a\\b\\report.pdf", None).unwrap()), "report.pdf");
        assert_eq!(file(destination("a/b/report.pdf", Some("out.bin")).unwrap()), "out.bin");
        assert_eq!(file(destination("a/b/report.pdf", Some("-")).unwrap()), "<stdout>");
        // A key that names a folder, or climbs, has no file name; --out must say.
        for key in ["logs/", "..", "a/..", "a/."] {
            assert!(destination(key, None).is_err(), "{key}");
        }
        assert_eq!(file(destination("logs/", Some("here.txt")).unwrap()), "here.txt");
        // The key never decides the directory: only its last part is used.
        assert_eq!(file(destination("../../etc/passwd", None).unwrap()), "passwd");
    }

    #[test]
    fn stored_key_round_trips_without_surprises() {
        let s = Stored { key_id: "sk_1".into(), access_key_id: "AK".into(), secret_access_key: "SK".into() };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"{"keyId":"sk_1","accessKeyId":"AK","secretAccessKey":"SK"}"#);
        let back: Stored = serde_json::from_str(&json).unwrap();
        assert_eq!(back.access_key_id, "AK");
    }
}
