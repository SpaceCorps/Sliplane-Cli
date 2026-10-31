//! A small S3 client: path-style requests, Signature V4, the handful of operations the CLI needs.
//! No AWS SDK, no async runtime.
//!
//! **This module never sees the Sliplane API key**, and nothing here talks to the Sliplane API:
//! the endpoint, the signing region and the S3 key pair are handed in, and that is all it has. It
//! owns its own HTTP agent for the same reason - the API agent has a 100 s global timeout, which a
//! large transfer outlasts, and it follows redirects, which would carry a signed request somewhere
//! the signature was not made for.
//!
//! Payload hash: a PUT is hashed with SHA-256 in a first pass over the file and streamed in a second,
//! so the signature covers the body and the server rejects a file that changed in between. That is
//! the mode every S3-compatible backend accepts. `UNSIGNED-PAYLOAD` is only safe over TLS and not
//! every backend takes it, and the cost here is one more read of a local file.

use std::fs::File;
use std::io::{Read, Seek, Write};
use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use ureq::Agent;
use ureq::http::Response;

use crate::config;
use crate::error::{Error, ErrorCode, Result};
use crate::sigv4::{self, EMPTY_SHA256, Request, Scope};

/// A single PUT is capped at 5 GiB by S3. Larger files need multipart upload, which is not built.
pub const MAX_PUT: u64 = 5 * 1024 * 1024 * 1024;
/// S3 returns at most this many entries per listing page.
pub const MAX_PAGE: usize = 1000;
/// Listing pages and error bodies are small; this stops a hostile endpoint filling memory.
const MAX_XML: u64 = 32 * 1024 * 1024;

pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key_id", &"<redacted>")
            .field("secret_access_key", &"<redacted>")
            .finish()
    }
}

pub struct ObjectStore {
    agent: Agent,
    https: bool,
    /// `host` or `host:port`, with the scheme's default port left off - the exact text that is
    /// both sent as `Host` and signed.
    authority: String,
    region: String,
    creds: Credentials,
}

/// One request, before it is signed.
struct Call<'a> {
    method: &'a str,
    bucket: &'a str,
    key: Option<&'a str>,
    query: &'a [(&'a str, &'a str)],
    payload_hash: &'a str,
    /// Headers that are sent but not signed.
    extra: &'a [(&'a str, &'a str)],
    body: Option<&'a File>,
}

impl<'a> Call<'a> {
    /// A request with no body and nothing but the bucket and (maybe) the key.
    fn bare(method: &'a str, bucket: &'a str, key: Option<&'a str>) -> Call<'a> {
        Call { method, bucket, key, query: &[], payload_hash: EMPTY_SHA256, extra: &[], body: None }
    }
}

/// A failed request: what S3 called it (empty for a transport failure), the HTTP status (0 when
/// there was none) and the error to surface.
pub struct Failure {
    pub code: String,
    pub status: u16,
    pub error: Error,
}

impl From<Failure> for Error {
    fn from(f: Failure) -> Error {
        f.error
    }
}

type Attempt<T> = std::result::Result<T, Failure>;

pub struct Object {
    pub key: String,
    pub size: u64,
    pub last_modified: String,
    pub etag: String,
}

pub struct Page {
    pub objects: Vec<Object>,
    pub prefixes: Vec<String>,
    /// Present exactly when the listing continues.
    pub next_token: Option<String>,
}

#[derive(Default)]
pub struct ListParams<'a> {
    pub prefix: &'a str,
    pub delimiter: Option<&'a str>,
    pub token: Option<&'a str>,
    pub max_keys: usize,
}

pub struct Uploaded {
    pub size: u64,
    pub etag: String,
}

pub enum Probe {
    Ready,
    /// The key is not usable yet (or no longer): the error says what S3 answered.
    Pending(Error),
}

impl ObjectStore {
    pub fn new(endpoint: &str, region: &str, creds: Credentials) -> Result<ObjectStore> {
        let (https, authority) = parse_endpoint(endpoint)?;
        if region.trim().is_empty() {
            return Err(Error::invalid("--region must not be empty."));
        }
        let agent: Agent = Agent::config_builder()
            // No global timeout: a 5 GiB transfer takes as long as it takes. A server that stops
            // answering is bounded by the connect and response-header timeouts.
            .timeout_global(None)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(600)))
            .http_status_as_error(false)
            // A redirect is an error here, never followed: it would resend a request signed for
            // another host and another path.
            .max_redirects(0)
            .user_agent(concat!("sliplane-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Ok(ObjectStore { agent, https, authority, region: region.trim().to_string(), creds })
    }

    fn request(&self, call: Call) -> Attempt<Response<ureq::Body>> {
        let Call { method, bucket, key, query, payload_hash, extra, body } = call;
        let mut path = format!("/{}", sigv4::uri_encode(bucket, false));
        if let Some(key) = key {
            path.push('/');
            path.push_str(&sigv4::uri_encode(key, true));
        }
        // `YYYYMMDDTHHMMSSZ`, from the `YYYY-MM-DDTHH:MM:SSZ` the config module already formats.
        let amz_date = config::now_utc().replace(['-', ':'], "");
        let signed = sigv4::sign(
            &Request {
                method,
                canonical_uri: &path,
                query,
                headers: &[
                    ("host", self.authority.as_str()),
                    ("x-amz-content-sha256", payload_hash),
                    ("x-amz-date", amz_date.as_str()),
                ],
                payload_hash,
            },
            &Scope {
                access_key: &self.creds.access_key_id,
                secret_key: &self.creds.secret_access_key,
                region: &self.region,
                service: "s3",
                amz_date: &amz_date,
            },
        );

        let mut url = format!("{}://{}{}", if self.https { "https" } else { "http" }, self.authority, path);
        if !query.is_empty() {
            url.push('?');
            // The same text that was signed, so the two cannot disagree about an encoding.
            url.push_str(&sigv4::canonical_query(query));
        }

        macro_rules! with_headers {
            ($req:expr) => {{
                let mut r = $req
                    .header("Host", &self.authority)
                    .header("x-amz-date", &amz_date)
                    .header("x-amz-content-sha256", payload_hash)
                    .header("Authorization", &signed.authorization);
                for (k, v) in extra {
                    r = r.header(*k, *v);
                }
                r
            }};
        }

        let sent = match method {
            "GET" => with_headers!(self.agent.get(&url)).call(),
            "HEAD" => with_headers!(self.agent.head(&url)).call(),
            "DELETE" => with_headers!(self.agent.delete(&url)).call(),
            "PUT" => {
                let file = body.expect("a PUT carries a file");
                with_headers!(self.agent.put(&url)).send(file)
            }
            other => unreachable!("no {other} requests are made"),
        };
        let mut response = sent.map_err(|e| transport(&e))?;

        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(response);
        }
        let bytes = response.body_mut().with_config().limit(MAX_XML).read_to_vec().unwrap_or_default();
        Err(failure(status, &bytes))
    }

    /// One page of keys under a prefix, folded by the delimiter if there is one.
    pub fn list(&self, bucket: &str, p: &ListParams) -> Result<Page> {
        Ok(self.list_page(bucket, p)?)
    }

    fn list_page(&self, bucket: &str, p: &ListParams) -> Attempt<Page> {
        let max_keys = p.max_keys.clamp(1, MAX_PAGE).to_string();
        let mut query = vec![("list-type", "2"), ("encoding-type", "url"), ("max-keys", max_keys.as_str())];
        if !p.prefix.is_empty() {
            query.push(("prefix", p.prefix));
        }
        if let Some(d) = p.delimiter {
            query.push(("delimiter", d));
        }
        if let Some(t) = p.token {
            query.push(("continuation-token", t));
        }
        let mut response = self.request(Call { query: &query, ..Call::bare("GET", bucket, None) })?;
        let bytes = response.body_mut().with_config().limit(MAX_XML).read_to_vec().map_err(|e| transport(&e))?;
        parse_listing(&String::from_utf8_lossy(&bytes)).map_err(|error| Failure {
            code: String::new(),
            status: 200,
            error,
        })
    }

    /// Whether the key can read the bucket yet. A key just made by the Sliplane API takes a moment
    /// to be known to the storage endpoint, during which it is refused as unknown or as not allowed.
    pub fn probe(&self, bucket: &str) -> Result<Probe> {
        let params = ListParams { max_keys: 1, ..Default::default() };
        match self.list_page(bucket, &params) {
            Ok(_) => Ok(Probe::Ready),
            Err(f) if matches!(f.code.as_str(), "InvalidAccessKeyId" | "AccessDenied") => Ok(Probe::Pending(f.error)),
            Err(f) => Err(f.error),
        }
    }

    pub fn exists(&self, bucket: &str, key: &str) -> Result<bool> {
        match self.request(Call::bare("HEAD", bucket, Some(key))) {
            Ok(_) => Ok(true),
            Err(f) if f.status == 404 => Ok(false),
            Err(f) => Err(f.error),
        }
    }

    pub fn delete(&self, bucket: &str, key: &str) -> Result<()> {
        self.request(Call::bare("DELETE", bucket, Some(key)))?;
        Ok(())
    }

    /// Streams the object into `out` and returns how many bytes that was.
    pub fn get(&self, bucket: &str, key: &str, out: &mut dyn Write) -> Result<u64> {
        let mut response = self.request(Call::bare("GET", bucket, Some(key)))?;
        // S3 always says how long an object is. The HTTP client drops that header when it silently
        // decompresses an object stored with `Content-Encoding: gzip`, and then what it hands over
        // is not what was stored. Wrong bytes are worse than no bytes, so that is refused.
        let expected: u64 = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse().ok())
            .ok_or_else(|| {
                Error::other("The response did not say how long the object is, so it cannot be checked.")
                    .detail(
                        "S3 always sends Content-Length. Its absence usually means the object is stored with \
                         Content-Encoding: gzip (or br), which the HTTP client would decompress, so the bytes \
                         would not be the ones stored.",
                    )
                    .fix("Fetch it with a tool that can keep the stored bytes, e.g. curl or the aws CLI.")
            })?;
        let mut reader = response.body_mut().as_reader();
        let mut buf = vec![0u8; 256 * 1024];
        let mut total = 0u64;
        loop {
            let n = reader.read(&mut buf).map_err(|e| {
                Error::new(ErrorCode::Network, "The download was interrupted.")
                    .detail(e.to_string())
                    .fix("Retry once, then stop.")
            })?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            total += n as u64;
        }
        if total != expected {
            return Err(Error::new(ErrorCode::Network, "The download ended early.")
                .detail(format!("Received {total} of {expected} bytes."))
                .fix("Retry once, then stop."));
        }
        Ok(total)
    }

    /// Uploads a local file with one PUT. Hashes it first, then streams it; see the module notes.
    pub fn put_file(&self, bucket: &str, key: &str, path: &Path, content_type: &str) -> Result<Uploaded> {
        let mut file =
            File::open(path).map_err(|e| Error::invalid(format!("Could not open {}: {e}", path.display())))?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(Error::invalid(format!("{} is not a regular file.", path.display())));
        }
        if meta.len() > MAX_PUT {
            return Err(Error::invalid(format!(
                "{} is {} bytes; one upload is limited to 5 GiB.",
                path.display(),
                meta.len()
            ))
            .detail("Larger files need a multipart upload, which this CLI does not do."));
        }

        let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
        let mut buf = vec![0u8; 1024 * 1024];
        let mut hashed = 0u64;
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            sha2::Digest::update(&mut hasher, &buf[..n]);
            hashed += n as u64;
        }
        if hashed != meta.len() {
            return Err(Error::other(format!("{} changed while it was being read.", path.display()))
                .fix("Retry once the file is stable."));
        }
        file.rewind()?;
        let payload_hash = sigv4::hex(&sha2::Digest::finalize(hasher));

        let response = self.request(Call {
            payload_hash: &payload_hash,
            extra: &[("Content-Type", content_type)],
            body: Some(&file),
            ..Call::bare("PUT", bucket, Some(key))
        })?;
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().trim_matches('"').to_string())
            .unwrap_or_default();
        Ok(Uploaded { size: hashed, etag })
    }
}

fn transport(e: &ureq::Error) -> Failure {
    let error = match e {
        ureq::Error::Timeout(_) => Error::new(ErrorCode::Network, "The request to the storage endpoint timed out.")
            .fix("Retry once, then stop."),
        other => Error::new(ErrorCode::Network, "Could not reach the storage endpoint.")
            .detail(other.to_string())
            .fix("Retry once, then stop."),
    };
    Failure { code: String::new(), status: 0, error }
}

fn failure(status: u16, body: &[u8]) -> Failure {
    let text = String::from_utf8_lossy(body);
    let root = xml::parse(&text).filter(|n| n.name == "Error");
    let field = |name: &str| root.as_ref().and_then(|n| n.text_of(name)).unwrap_or("").to_string();
    let (code, message, request_id) = (field("Code"), field("Message"), field("RequestId"));
    Failure { error: map_error(status, &code, &message, &request_id), code, status }
}

/// Turns what S3 answered into our error codes. The S3 code and message are kept in `detail`.
pub fn map_error(status: u16, code: &str, message: &str, request_id: &str) -> Error {
    let mut detail = format!("HTTP {status}");
    if !code.is_empty() {
        detail.push_str(&format!(", S3 error {code}"));
    }
    if !message.is_empty() {
        detail.push_str(&format!(": {message}"));
    }
    if !request_id.is_empty() {
        detail.push_str(&format!(" (request id {request_id})"));
    }

    let e = match (status, code) {
        (_, "NoSuchBucket") => Error::new(ErrorCode::NotFound, "The storage endpoint has no such bucket."),
        (404, _) | (_, "NoSuchKey" | "NotFound") => Error::new(ErrorCode::NotFound, "The object does not exist."),
        (_, "InvalidAccessKeyId") => {
            Error::new(ErrorCode::AuthRequired, "The storage endpoint does not know this S3 access key.")
        }
        (_, "SignatureDoesNotMatch") => {
            detail.push_str(
                ". The stored secret may be wrong, or this endpoint may expect another signing region (--region).",
            );
            Error::new(ErrorCode::AuthRequired, "The storage endpoint rejected the request signature.")
        }
        (_, "RequestTimeTooSkewed") => Error::other("This machine's clock is too far off for the storage endpoint.")
            .fix("Correct the system clock and retry."),
        (_, "AuthorizationHeaderMalformed") => {
            Error::invalid("The signing region is not the one the storage endpoint expects.")
                .fix("The message in the detail names the region; pass it with --region.")
        }
        (_, "AccessDenied") | (401 | 403, _) => {
            Error::new(ErrorCode::AuthRequired, "The S3 key is not allowed to do that.")
        }
        (_, "SlowDown") | (429, _) => {
            Error::new(ErrorCode::RateLimited, "Rate limited by the storage endpoint.").fix("Back off before retrying.")
        }
        (300..=399, _) => Error::other("The storage endpoint answered with a redirect, which is not followed.")
            .fix("Check the bucket's endpoint with: sliplane buckets list --account <account>"),
        (500..=599, _) => Error::new(ErrorCode::Network, "The storage endpoint returned a server error.")
            .fix("Retry once, then stop."),
        (
            400,
            "EntityTooLarge"
            | "KeyTooLongError"
            | "InvalidArgument"
            | "InvalidRequest"
            | "InvalidBucketName"
            | "MalformedXML"
            | "MissingContentLength",
        )
        | (413, _) => Error::new(ErrorCode::InvalidInput, "The storage endpoint refused the request as invalid."),
        (400, "XAmzContentSHA256Mismatch" | "BadDigest") => {
            Error::other("The file changed while it was being uploaded.").fix("Retry once the file is stable.")
        }
        (400, _) => Error::new(ErrorCode::InvalidInput, "The storage endpoint refused the request as invalid."),
        _ => Error::new(ErrorCode::Error, "The storage endpoint refused the request."),
    };
    e.detail(detail)
}

/// `(https, authority)`. Plain HTTP is accepted only for a loopback host: an S3 key pair and an
/// object body must not cross the internet unencrypted because an endpoint said so.
fn parse_endpoint(endpoint: &str) -> Result<(bool, String)> {
    let bad = |why: &str| Error::other(format!("'{endpoint}' is not a usable S3 endpoint.")).detail(why.to_string());
    let (https, rest) = if let Some(r) = endpoint.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = endpoint.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(bad("It must start with https://."));
    };
    let authority = rest.trim_end_matches('/');
    if authority.is_empty()
        || authority.contains(|c: char| matches!(c, '/' | '?' | '#' | '@' | '\\') || c.is_whitespace())
    {
        return Err(bad("It must be a bare host (and optional port): no path, query or credentials."));
    }

    let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
        let (addr, tail) = inner.split_once(']').ok_or_else(|| bad("Unclosed IPv6 address."))?;
        (format!("[{addr}]"), tail.strip_prefix(':').map(str::to_string).or_else(|| tail.is_empty().then(String::new)))
    } else if let Some((h, p)) = authority.rsplit_once(':') {
        (h.to_string(), Some(p.to_string()))
    } else {
        (authority.to_string(), Some(String::new()))
    };
    let port = port.ok_or_else(|| bad("Garbage after the IPv6 address."))?;
    if host.is_empty() || host.contains(':') && !host.starts_with('[') {
        return Err(bad("The host is empty or malformed."));
    }
    let port: Option<u16> =
        if port.is_empty() { None } else { Some(port.parse().map_err(|_| bad("The port is not a number."))?) };

    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = bare.eq_ignore_ascii_case("localhost")
        || bare.to_ascii_lowercase().ends_with(".localhost")
        || bare.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if !https && !loopback {
        return Err(Error::other(format!("Refusing the S3 endpoint '{endpoint}': it is plain HTTP.")).detail(
            "S3 keys and object data are only sent over HTTPS, except to a loopback address (a local test server).",
        ));
    }

    let default = if https { 443 } else { 80 };
    let mut authority = host.to_ascii_lowercase();
    if let Some(p) = port.filter(|p| *p != default) {
        authority.push_str(&format!(":{p}"));
    }
    Ok((https, authority))
}

fn percent_decode_plus(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() => {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).expect("two hex digits"));
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_listing(text: &str) -> Result<Page> {
    let unreadable = || {
        Error::other("The storage endpoint sent a listing this CLI could not read.")
            .detail(text.chars().take(300).collect::<String>())
    };
    let root = xml::parse(text).filter(|n| n.name == "ListBucketResult").ok_or_else(unreadable)?;
    // Asked for with encoding-type=url, so a key with a control character survives XML. Decode only
    // when the response says it was done: a backend that ignores the request sends plain text.
    let encoded = root.text_of("EncodingType") == Some("url");
    let decode = |s: &str| if encoded { percent_decode_plus(s) } else { s.to_string() };

    let mut objects = Vec::new();
    for c in root.children_named("Contents") {
        let size = c.text_of("Size").unwrap_or("0").trim().parse().map_err(|_| unreadable())?;
        objects.push(Object {
            key: decode(c.text_of("Key").ok_or_else(unreadable)?),
            size,
            last_modified: c.text_of("LastModified").unwrap_or("").to_string(),
            etag: c.text_of("ETag").unwrap_or("").trim().trim_matches('"').to_string(),
        });
    }
    let prefixes = root.children_named("CommonPrefixes").filter_map(|p| p.text_of("Prefix")).map(&decode).collect();

    let truncated = root.text_of("IsTruncated").is_some_and(|t| t.trim().eq_ignore_ascii_case("true"));
    let next_token = match (truncated, root.text_of("NextContinuationToken").filter(|t| !t.is_empty())) {
        (true, Some(t)) => Some(t.to_string()),
        (true, None) => {
            return Err(Error::other("The listing says there is more but gives no way to continue.")
                .detail("IsTruncated is true and NextContinuationToken is missing."));
        }
        (false, _) => None,
    };
    Ok(Page { objects, prefixes, next_token })
}

/// Just enough XML for S3's answers: elements, text, entities, CDATA. Not a validating parser, and
/// it never expands anything but the five predefined entities and numeric references.
mod xml {
    pub struct Node {
        pub name: String,
        pub text: String,
        pub children: Vec<Node>,
    }

    impl Node {
        pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
            self.children.iter().filter(move |c| c.name == name)
        }

        /// The text of the first child with this name.
        pub fn text_of(&self, name: &str) -> Option<&str> {
            self.children.iter().find(|c| c.name == name).map(|c| c.text.as_str())
        }
    }

    const MAX_DEPTH: usize = 32;

    pub fn parse(s: &str) -> Option<Node> {
        let mut stack: Vec<Node> = Vec::new();
        let mut rest = s;
        while let Some(lt) = rest.find('<') {
            if let Some(top) = stack.last_mut() {
                top.text.push_str(&unescape(&rest[..lt]));
            }
            rest = &rest[lt..];

            if let Some(r) = rest.strip_prefix("<!--") {
                rest = &r[r.find("-->")? + 3..];
            } else if let Some(r) = rest.strip_prefix("<![CDATA[") {
                let end = r.find("]]>")?;
                stack.last_mut()?.text.push_str(&r[..end]);
                rest = &r[end + 3..];
            } else if rest.starts_with("<?") || rest.starts_with("<!") {
                rest = &rest[rest.find('>')? + 1..];
            } else if let Some(r) = rest.strip_prefix("</") {
                let end = r.find('>')?;
                let done = stack.pop()?;
                if done.name != local(r[..end].trim()) {
                    return None;
                }
                rest = &r[end + 1..];
                match stack.last_mut() {
                    Some(parent) => parent.children.push(done),
                    None => return Some(done),
                }
            } else {
                // A start tag; find its end, skipping quoted attribute values.
                let tag = &rest[1..];
                let mut quote = None;
                let mut end = None;
                for (i, c) in tag.char_indices() {
                    match (quote, c) {
                        (None, '"' | '\'') => quote = Some(c),
                        (Some(q), c) if c == q => quote = None,
                        (None, '>') => {
                            end = Some(i);
                            break;
                        }
                        _ => {}
                    }
                }
                let end = end?;
                let inner = &tag[..end];
                let (inner, empty) = match inner.strip_suffix('/') {
                    Some(i) => (i, true),
                    None => (inner, false),
                };
                let name = local(inner.split(|c: char| c.is_whitespace()).next().filter(|n| !n.is_empty())?);
                let node = Node { name: name.to_string(), text: String::new(), children: Vec::new() };
                rest = &tag[end + 1..];
                if empty {
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(node),
                        None => return Some(node),
                    }
                } else {
                    if stack.len() >= MAX_DEPTH {
                        return None;
                    }
                    stack.push(node);
                }
            }
        }
        None
    }

    /// `ns:Name` is `Name`.
    fn local(name: &str) -> &str {
        name.rsplit_once(':').map_or(name, |(_, n)| n)
    }

    pub fn unescape(s: &str) -> String {
        if !s.contains('&') {
            return s.to_string();
        }
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(amp) = rest.find('&') {
            out.push_str(&rest[..amp]);
            rest = &rest[amp..];
            let Some(semi) = rest.find(';').filter(|i| *i <= 10) else {
                out.push('&');
                rest = &rest[1..];
                continue;
            };
            let entity = &rest[1..semi];
            let decoded = match entity {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                e if e.starts_with("#x") || e.starts_with("#X") => {
                    u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32)
                }
                e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
                _ => None,
            };
            match decoded {
                Some(c) => {
                    out.push(c);
                    rest = &rest[semi + 1..];
                }
                None => {
                    out.push('&');
                    rest = &rest[1..];
                }
            }
        }
        out.push_str(rest);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_reads_elements_entities_and_cdata() {
        let n = xml::parse(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- c --><a xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
             <b>x &amp; y &lt;z&gt; &quot;q&quot; &apos;s&apos; &#65;&#x42;</b><c/><d><![CDATA[<raw>]]></d>\
             <ns:e>1</ns:e><e>2</e></a>",
        )
        .unwrap();
        assert_eq!(n.name, "a");
        assert_eq!(n.text_of("b"), Some("x & y <z> \"q\" 's' AB"));
        assert_eq!(n.text_of("c"), Some(""));
        assert_eq!(n.text_of("d"), Some("<raw>"));
        let e: Vec<_> = n.children_named("e").map(|c| c.text.as_str()).collect();
        assert_eq!(e, ["1", "2"]);
    }

    #[test]
    fn xml_refuses_what_is_not_well_formed() {
        for bad in ["", "plain text", "<a><b></a></b>", "<a>", "<a><b>", "</a>", "<a b=\"c></a>", "<!-- x"] {
            assert!(xml::parse(bad).is_none(), "{bad:?}");
        }
        // Whatever the nesting, the depth is bounded.
        let deep = "<a>".repeat(100) + &"</a>".repeat(100);
        assert!(xml::parse(&deep).is_none());
        // An unknown entity is left as it is, never expanded.
        assert_eq!(xml::parse("<a>&xxe;&#99999999999;</a>").unwrap().text, "&xxe;&#99999999999;");
    }

    #[test]
    fn parses_a_listing_with_folders() {
        let page = parse_listing(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>b</Name><Prefix>logs/</Prefix>
<KeyCount>3</KeyCount><MaxKeys>1000</MaxKeys><Delimiter>/</Delimiter><IsTruncated>true</IsTruncated>
<NextContinuationToken>1ueGcxLPRx1Tr/XYExHnhbYLgveDs2J/wm36Hy4vbOwM=</NextContinuationToken>
<Contents><Key>logs/a.txt</Key><LastModified>2026-10-31T12:00:00.000Z</LastModified><ETag>&quot;d41d8cd98f00b204e9800998ecf8427e&quot;</ETag><Size>12</Size><StorageClass>STANDARD</StorageClass></Contents>
<CommonPrefixes><Prefix>logs/2026/</Prefix></CommonPrefixes><CommonPrefixes><Prefix>logs/2027/</Prefix></CommonPrefixes>
</ListBucketResult>"#,
        )
        .unwrap();
        assert_eq!(page.objects.len(), 1);
        assert_eq!(page.objects[0].key, "logs/a.txt");
        assert_eq!(page.objects[0].size, 12);
        assert_eq!(page.objects[0].last_modified, "2026-10-31T12:00:00.000Z");
        assert_eq!(page.objects[0].etag, "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(page.prefixes, ["logs/2026/", "logs/2027/"]);
        assert_eq!(page.next_token.as_deref(), Some("1ueGcxLPRx1Tr/XYExHnhbYLgveDs2J/wm36Hy4vbOwM="));
    }

    #[test]
    fn url_encoded_keys_are_decoded_only_when_the_response_says_so() {
        let body = |enc: &str, key: &str| {
            format!(
                "<ListBucketResult>{enc}<IsTruncated>false</IsTruncated><Contents><Key>{key}</Key><Size>1</Size></Contents>\
                 <CommonPrefixes><Prefix>{key}/</Prefix></CommonPrefixes></ListBucketResult>"
            )
        };
        // Spaces as `+` or `%20`, a literal plus as `%2B`, UTF-8 as bytes.
        let page = parse_listing(&body("<EncodingType>url</EncodingType>", "a+b%20c%2Bd/caf%C3%A9%0A")).unwrap();
        assert_eq!(page.objects[0].key, "a b c+d/caf\u{e9}\n");
        assert_eq!(page.prefixes[0], "a b c+d/caf\u{e9}\n/");
        assert!(page.next_token.is_none());
        // A backend that ignored encoding-type sends the key as it is.
        let page = parse_listing(&body("", "100%+real")).unwrap();
        assert_eq!(page.objects[0].key, "100%+real");
        // A bare percent is not an escape.
        assert_eq!(percent_decode_plus("50%"), "50%");
        assert_eq!(percent_decode_plus("%zz%4"), "%zz%4");
        assert_eq!(percent_decode_plus("%41"), "A");
    }

    #[test]
    fn a_broken_listing_is_an_error_not_an_empty_one() {
        assert!(parse_listing("<html>502</html>").is_err());
        assert!(parse_listing("<ListBucketResult><Contents><Size>1</Size></Contents></ListBucketResult>").is_err());
        assert!(
            parse_listing("<ListBucketResult><Contents><Key>k</Key><Size>big</Size></Contents></ListBucketResult>")
                .is_err()
        );
        let e = parse_listing("<ListBucketResult><IsTruncated>true</IsTruncated></ListBucketResult>").err().unwrap();
        assert!(e.detail.unwrap().contains("NextContinuationToken"));
        // An empty bucket is a valid listing.
        let page = parse_listing(
            "<ListBucketResult><KeyCount>0</KeyCount><IsTruncated>false</IsTruncated></ListBucketResult>",
        )
        .unwrap();
        assert!(page.objects.is_empty() && page.prefixes.is_empty() && page.next_token.is_none());
    }

    #[test]
    fn endpoints() {
        let ok = |e: &str| parse_endpoint(e).unwrap();
        assert_eq!(
            ok("https://eu-central-2.storage.impossibleapi.net"),
            (true, "eu-central-2.storage.impossibleapi.net".into())
        );
        assert_eq!(ok("https://S3.Example.com/"), (true, "s3.example.com".into()));
        // The default port is left off, since that is what is sent as Host and what is signed.
        assert_eq!(ok("https://s3.example.com:443"), (true, "s3.example.com".into()));
        assert_eq!(ok("https://s3.example.com:8443"), (true, "s3.example.com:8443".into()));
        assert_eq!(ok("http://127.0.0.1:9000"), (false, "127.0.0.1:9000".into()));
        assert_eq!(ok("http://localhost:9000"), (false, "localhost:9000".into()));
        assert_eq!(ok("http://[::1]:9000"), (false, "[::1]:9000".into()));
        assert_eq!(ok("http://127.0.0.1:80"), (false, "127.0.0.1".into()));

        for bad in [
            "s3.example.com",
            "ftp://s3.example.com",
            "https://",
            "https://s3.example.com/prefix",
            "https://s3.example.com?x=1",
            "https://user:pw@s3.example.com",
            "https://s3.example.com:port",
            "https://s3.example.com:99999",
            "https://[::1",
            "http://s3.example.com",
            "http://10.0.0.5:9000",
        ] {
            assert!(parse_endpoint(bad).is_err(), "{bad}");
        }
        assert!(parse_endpoint("http://s3.example.com").unwrap_err().message.contains("plain HTTP"));
    }

    #[test]
    fn s3_errors_map_to_our_codes_and_keep_the_s3_code_in_the_detail() {
        let code = |status, s3: &str| map_error(status, s3, "m", "r1").code;
        assert_eq!(code(404, "NoSuchKey"), ErrorCode::NotFound);
        assert_eq!(code(404, "NoSuchBucket"), ErrorCode::NotFound);
        assert_eq!(code(404, ""), ErrorCode::NotFound);
        assert_eq!(code(403, "InvalidAccessKeyId"), ErrorCode::AuthRequired);
        assert_eq!(code(403, "SignatureDoesNotMatch"), ErrorCode::AuthRequired);
        assert_eq!(code(403, "AccessDenied"), ErrorCode::AuthRequired);
        assert_eq!(code(403, ""), ErrorCode::AuthRequired);
        assert_eq!(code(503, "SlowDown"), ErrorCode::RateLimited);
        assert_eq!(code(429, ""), ErrorCode::RateLimited);
        assert_eq!(code(400, "EntityTooLarge"), ErrorCode::InvalidInput);
        assert_eq!(code(400, "AuthorizationHeaderMalformed"), ErrorCode::InvalidInput);
        assert_eq!(code(400, "XAmzContentSHA256Mismatch"), ErrorCode::Error);
        assert_eq!(code(409, "BucketNotEmpty"), ErrorCode::Error);
        assert_eq!(code(500, "InternalError"), ErrorCode::Network);
        assert_eq!(code(503, "ServiceUnavailable"), ErrorCode::Network);
        assert_eq!(code(307, ""), ErrorCode::Error);
        assert_eq!(code(403, "RequestTimeTooSkewed"), ErrorCode::Error);

        let e = map_error(403, "AccessDenied", "Access Denied.", "4442587FB7D0A2F9");
        assert_eq!(e.detail.unwrap(), "HTTP 403, S3 error AccessDenied: Access Denied. (request id 4442587FB7D0A2F9)");
    }

    #[test]
    fn failure_reads_the_xml_error_body() {
        let f = failure(
            403,
            b"<?xml version=\"1.0\"?><Error><Code>InvalidAccessKeyId</Code><Message>The key is unknown.</Message><RequestId>abc</RequestId></Error>",
        );
        assert_eq!(f.code, "InvalidAccessKeyId");
        assert_eq!(f.status, 403);
        assert_eq!(f.error.code, ErrorCode::AuthRequired);
        assert!(f.error.detail.unwrap().contains("The key is unknown."));
        // No body (a HEAD), or something that is not S3's error document, still maps by status.
        assert_eq!(failure(404, b"").error.code, ErrorCode::NotFound);
        assert_eq!(failure(502, b"<html>bad gateway</html>").error.code, ErrorCode::Network);
    }

    #[test]
    fn credentials_do_not_print() {
        let c = Credentials { access_key_id: "AKIAEXAMPLE".into(), secret_access_key: "s3cr3t".into() };
        let shown = format!("{c:?}");
        assert!(!shown.contains("AKIAEXAMPLE") && !shown.contains("s3cr3t"), "{shown}");
    }
}
