//! A small synchronous PostgreSQL client: just enough of the wire protocol to run SQL as an agent
//! would, and hand the rows back as data.
//!
//! What it speaks: the Simple Query protocol (several statements in one string are fine, and each
//! gets its own result), SCRAM-SHA-256 authentication, and TLS that is **always** on and **always**
//! verified against the host name (libpq's `sslmode=verify-full`). What it refuses: servers that do
//! not offer TLS, md5/cleartext/GSS authentication, `COPY`. No async runtime - a CLI that runs one
//! query does not need one (see AGENTS.md) - so this is `std::net` plus `rustls` plus the message
//! codec of `postgres-protocol`.
//!
//! The password is held by [`Target`] and used for the SCRAM exchange only. Nothing in here puts it
//! in an error, and the callers scrub it again before anything is printed.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use fallible_iterator::FallibleIterator;
use postgres_protocol::authentication::sasl::{ChannelBinding, ScramSha256};
use postgres_protocol::message::backend::{self, Message};
use postgres_protocol::message::frontend;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use serde_json::{Map, Number, Value};

/// How long TCP, TLS and login may take together. The query has its own budget (`Options::timeout`).
const CONNECT_BUDGET: Duration = Duration::from_secs(10);
/// How much later than the server's `statement_timeout` we give up waiting. The server's own
/// "canceling statement due to statement timeout" is the better error; ours is the backstop.
const GRACE: Duration = Duration::from_secs(5);
/// No backend message is this large unless something is wrong (a single `DataRow` is the biggest).
const MAX_MESSAGE: usize = 256 * 1024 * 1024;
/// Notices (`NOTICE: relation already exists, skipping`) are kept, a few of them.
const MAX_NOTICES: usize = 20;
/// Extra roots for a test server's CA; verification is otherwise unchanged. See AGENTS.md.
pub const CA_FILE_ENV: &str = "SLIPLANE_PG_CA_FILE";

// ---------------------------------------------------------------------------------------------
// what goes in and what comes out

pub struct Target {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub password: String,
}

impl fmt::Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Target")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("database", &self.database)
            .field("user", &self.user)
            .field("password", &"<hidden>")
            .finish()
    }
}

pub struct Options {
    /// Send `default_transaction_read_only=on`. A guard against accidents, not a sandbox: SQL can
    /// switch it off again, so the database role's own privileges are the real boundary.
    pub read_only: bool,
    /// Sent as `statement_timeout`, and the basis of the read timeout on the socket.
    pub timeout: Duration,
    /// Rows kept per statement; the rest are counted and dropped.
    pub max_rows: usize,
}

/// One column of a result set.
#[derive(Debug)]
pub struct Column {
    pub name: String,
    /// The key this column has in each row object: its name, made unique when a name repeats.
    pub key: String,
    pub oid: u32,
}

/// The result of one statement of the query string.
#[derive(Debug, Default)]
pub struct Statement {
    pub tag: String,
    /// `None` for a statement that returns no result set (`INSERT` without `RETURNING`, `CREATE`).
    pub columns: Option<Vec<Column>>,
    pub rows: Vec<Map<String, Value>>,
    /// Rows the statement returned (all of them, even those past `max_rows`), or rows it changed.
    pub row_count: u64,
    pub truncated: bool,
    pub notices: Vec<String>,
}

impl Statement {
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("commandTag".into(), Value::String(self.tag.clone()));
        m.insert("rowCount".into(), Value::from(self.row_count));
        if let Some(columns) = &self.columns {
            let columns: Vec<Value> = columns
                .iter()
                .map(|c| {
                    let mut o = Map::new();
                    o.insert("name".into(), Value::String(c.name.clone()));
                    if c.key != c.name {
                        o.insert("key".into(), Value::String(c.key.clone()));
                    }
                    o.insert("type".into(), type_name(c.oid).map_or(Value::Null, Value::from));
                    o.insert("oid".into(), Value::from(c.oid));
                    Value::Object(o)
                })
                .collect();
            m.insert("columns".into(), Value::Array(columns));
            m.insert("rows".into(), Value::Array(self.rows.iter().cloned().map(Value::Object).collect()));
            m.insert("truncated".into(), Value::Bool(self.truncated));
            let renamed: Vec<String> = self
                .columns
                .iter()
                .flatten()
                .enumerate()
                .filter(|(_, c)| c.key != c.name)
                .map(|(i, c)| {
                    format!(
                        "Column {} is named '{}' like an earlier one; its key in rows is '{}'.",
                        i + 1,
                        c.name,
                        c.key
                    )
                })
                .collect();
            if !renamed.is_empty() {
                m.insert("warnings".into(), Value::from(renamed));
            }
        }
        if !self.notices.is_empty() {
            m.insert("notices".into(), Value::from(self.notices.clone()));
        }
        Value::Object(m)
    }
}

/// An `ErrorResponse`, field by field.
#[derive(Debug, Default, Clone)]
pub struct ServerError {
    pub severity: String,
    pub code: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub position: Option<String>,
    pub context: Option<String>,
    /// Which statement of the query string failed, counting from 1; 0 when it was not a statement
    /// (a refused login).
    pub statement: usize,
}

/// Everything that can go wrong. Callers turn these into the CLI's error envelopes; they decide the
/// remediation, because that depends on things only they know (the database, the account).
#[derive(Debug)]
pub enum PgError {
    Resolve(io::Error),
    /// No TCP connection. `v4`: an IPv4 address was among those tried.
    Connect {
        source: io::Error,
        v4: bool,
    },
    /// The connection died, or hung, before TLS was up. This is what a firewall or an IP allow list
    /// looks like from here. `v4`: the connection was over IPv4.
    BeforeTls {
        source: io::Error,
        v4: bool,
    },
    Tls(rustls::Error),
    /// The server does not do TLS (it answered `N`). Credentials are never sent without it.
    NoTls,
    /// An authentication method this client does not do.
    Auth(String),
    Io(io::Error),
    /// The server said nothing for longer than `timeout` plus a grace period.
    Timeout,
    Protocol(String),
    Server(Box<ServerError>),
    Unsupported(String),
    InvalidSql(String),
    /// The test CA file could not be used.
    Config(String),
}

// ---------------------------------------------------------------------------------------------
// the connection

/// Something that can be read and written with a time limit. A trait so that the protocol code can
/// be driven by canned bytes in a test.
pub trait Transport: Read + Write {
    fn limit(&mut self, left: Duration) -> io::Result<()>;
}

impl Transport for StreamOwned<ClientConnection, TcpStream> {
    fn limit(&mut self, left: Duration) -> io::Result<()> {
        self.sock.set_read_timeout(Some(left))?;
        self.sock.set_write_timeout(Some(left))
    }
}

struct Wire<T: Transport> {
    io: T,
    buf: BytesMut,
    deadline: Instant,
}

impl<T: Transport> Wire<T> {
    fn new(io: T, budget: Duration) -> Self {
        Wire { io, buf: BytesMut::with_capacity(8192), deadline: Instant::now() + budget }
    }

    fn left(&self) -> Result<Duration, PgError> {
        self.deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()).ok_or(PgError::Timeout)
    }

    fn send(&mut self, out: &BytesMut) -> Result<(), PgError> {
        let left = self.left()?;
        self.io.limit(left).map_err(PgError::Io)?;
        self.io.write_all(out).and_then(|()| self.io.flush()).map_err(write_error)
    }

    /// The next backend message, reading as much as it takes.
    fn next(&mut self) -> Result<Message, PgError> {
        loop {
            if let Some(h) = backend::Header::parse(&self.buf).map_err(|e| PgError::Protocol(e.to_string()))?
                && h.len() as usize > MAX_MESSAGE
            {
                return Err(PgError::Protocol(format!("the server sent a {}-byte message", h.len())));
            }
            if let Some(m) = Message::parse(&mut self.buf).map_err(|e| PgError::Protocol(e.to_string()))? {
                return Ok(m);
            }
            let left = self.left()?;
            self.io.limit(left).map_err(PgError::Io)?;
            let mut chunk = [0u8; 16 * 1024];
            match self.io.read(&mut chunk) {
                Ok(0) => {
                    return Err(PgError::Io(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the server closed the connection",
                    )));
                }
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                    return Err(PgError::Timeout);
                }
                Err(e) => return Err(PgError::Io(e)),
            }
        }
    }

    /// Everything from the startup packet to the first `ReadyForQuery`.
    fn start(&mut self, target: &Target, opts: &Options) -> Result<(), PgError> {
        let timeout_ms = opts.timeout.as_millis().to_string();
        let mut params = vec![
            ("user", target.user.as_str()),
            ("database", target.database.as_str()),
            ("application_name", "sliplane-cli"),
            ("client_encoding", "UTF8"),
            ("statement_timeout", timeout_ms.as_str()),
        ];
        if opts.read_only {
            params.push(("default_transaction_read_only", "on"));
        }
        let mut out = BytesMut::new();
        frontend::startup_message(params, &mut out).map_err(PgError::Io)?;
        self.send(&out)?;
        self.authenticate(&target.password)
    }

    fn authenticate(&mut self, password: &str) -> Result<(), PgError> {
        let mut scram: Option<ScramSha256> = None;
        loop {
            let mut out = BytesMut::new();
            match self.next()? {
                Message::AuthenticationOk | Message::ParameterStatus(_) | Message::BackendKeyData(_) => {}
                Message::NoticeResponse(_) => {}
                Message::AuthenticationSasl(body) => {
                    let mut offered = Vec::new();
                    let mut mechanisms = body.mechanisms();
                    while let Some(m) = mechanisms.next().map_err(|e| PgError::Protocol(e.to_string()))? {
                        offered.push(m.to_string());
                    }
                    if !offered.iter().any(|m| m == "SCRAM-SHA-256") {
                        return Err(PgError::Auth(format!(
                            "The server offers SASL mechanisms [{}], and only SCRAM-SHA-256 is supported.",
                            offered.join(", ")
                        )));
                    }
                    let s = ScramSha256::new(password.as_bytes(), ChannelBinding::unsupported());
                    frontend::sasl_initial_response("SCRAM-SHA-256", s.message(), &mut out).map_err(PgError::Io)?;
                    scram = Some(s);
                    self.send(&out)?;
                }
                Message::AuthenticationSaslContinue(body) => {
                    let s = scram.as_mut().ok_or_else(|| PgError::Protocol("unexpected SASL continue".into()))?;
                    s.update(body.data()).map_err(|e| PgError::Protocol(format!("SCRAM: {e}")))?;
                    frontend::sasl_response(s.message(), &mut out).map_err(PgError::Io)?;
                    self.send(&out)?;
                }
                Message::AuthenticationSaslFinal(body) => {
                    let s = scram.as_mut().ok_or_else(|| PgError::Protocol("unexpected SASL final".into()))?;
                    s.finish(body.data()).map_err(|e| {
                        PgError::Protocol(format!(
                            "The server's SCRAM signature did not verify ({e}); refusing to continue."
                        ))
                    })?;
                }
                Message::AuthenticationMd5Password(_) => {
                    return Err(PgError::Auth(
                        "The server asked for md5 password authentication; only SCRAM-SHA-256 is supported.".into(),
                    ));
                }
                Message::AuthenticationCleartextPassword => {
                    return Err(PgError::Auth(
                        "The server asked for a cleartext password; only SCRAM-SHA-256 is supported.".into(),
                    ));
                }
                Message::AuthenticationGss
                | Message::AuthenticationKerberosV5
                | Message::AuthenticationScmCredential
                | Message::AuthenticationSspi
                | Message::AuthenticationGssContinue(_) => {
                    return Err(PgError::Auth(
                        "The server asked for GSS/Kerberos/SSPI authentication; only SCRAM-SHA-256 is supported."
                            .into(),
                    ));
                }
                Message::ErrorResponse(body) => return Err(PgError::Server(Box::new(parse_fields(body.fields(), 0)))),
                Message::ReadyForQuery(_) => return Ok(()),
                _ => return Err(PgError::Protocol("unexpected message while logging in".into())),
            }
        }
    }

    /// Runs `sql` (one or more statements) and collects a [`Statement`] for each.
    fn query(&mut self, sql: &str, max_rows: usize) -> Result<Vec<Statement>, PgError> {
        let mut out = BytesMut::new();
        frontend::query(sql, &mut out).map_err(|e| PgError::InvalidSql(e.to_string()))?;
        self.send(&out)?;

        let mut done: Vec<Statement> = Vec::new();
        let mut building: Option<Statement> = None;
        let mut notices: Vec<String> = Vec::new();
        // Statements the server has finished with, empty ones included: the failing one is the next.
        let mut finished = 0usize;
        loop {
            match self.next()? {
                Message::RowDescription(body) => {
                    let mut columns: Vec<Column> = Vec::new();
                    let mut fields = body.fields();
                    while let Some(f) = fields.next().map_err(|e| PgError::Protocol(e.to_string()))? {
                        let key = unique_key(f.name(), &columns);
                        columns.push(Column { name: f.name().to_string(), key, oid: f.type_oid() });
                    }
                    building = Some(Statement { columns: Some(columns), ..Statement::default() });
                }
                Message::DataRow(body) => {
                    let s =
                        building.as_mut().ok_or_else(|| PgError::Protocol("a row with no row description".into()))?;
                    s.row_count += 1;
                    if s.rows.len() >= max_rows {
                        s.truncated = true;
                        continue;
                    }
                    let columns = s.columns.as_deref().unwrap_or_default();
                    let mut row = Map::with_capacity(columns.len());
                    let mut ranges = body.ranges();
                    let mut i = 0;
                    while let Some(range) = ranges.next().map_err(|e| PgError::Protocol(e.to_string()))? {
                        let column =
                            columns.get(i).ok_or_else(|| PgError::Protocol("more values than columns".into()))?;
                        let value = match range {
                            Some(r) => decode(column.oid, &body.buffer()[r]),
                            None => Value::Null,
                        };
                        row.insert(column.key.clone(), value);
                        i += 1;
                    }
                    if i != columns.len() {
                        return Err(PgError::Protocol("fewer values than columns".into()));
                    }
                    s.rows.push(row);
                }
                Message::CommandComplete(body) => {
                    finished += 1;
                    let tag = body.tag().map_err(|e| PgError::Protocol(e.to_string()))?.to_string();
                    let mut s = building.take().unwrap_or_default();
                    if s.columns.is_none() {
                        s.row_count = tag_count(&tag);
                    }
                    s.tag = tag;
                    s.notices = std::mem::take(&mut notices);
                    done.push(s);
                }
                Message::EmptyQueryResponse => finished += 1,
                Message::NoticeResponse(body) => {
                    if notices.len() < MAX_NOTICES {
                        let e = parse_fields(body.fields(), 0);
                        notices.push(format!("{}: {}", e.severity, e.message));
                    }
                }
                Message::ErrorResponse(body) => {
                    return Err(PgError::Server(Box::new(parse_fields(body.fields(), finished + 1))));
                }
                Message::CopyInResponse(_) | Message::CopyOutResponse(_) => {
                    return Err(PgError::Unsupported(
                        "COPY is not supported by `postgres query`. Use SELECT, or run psql with `postgres connect`."
                            .into(),
                    ));
                }
                Message::ReadyForQuery(_) => return Ok(done),
                _ => {}
            }
        }
    }

    fn terminate(&mut self) {
        let mut out = BytesMut::new();
        frontend::terminate(&mut out);
        let _ = self.io.write_all(&out).and_then(|()| self.io.flush());
    }
}

fn write_error(e: io::Error) -> PgError {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => PgError::Timeout,
        _ => PgError::Io(e),
    }
}

/// A key not yet used by `earlier`: the name itself, else `name_2`, `name_3`, ...
fn unique_key(name: &str, earlier: &[Column]) -> String {
    let taken = |k: &str| earlier.iter().any(|c| c.key == k);
    if !taken(name) {
        return name.to_string();
    }
    (2..).map(|n| format!("{name}_{n}")).find(|k| !taken(k)).expect("an unbounded range always yields")
}

/// The affected-rows count of a command tag: `INSERT 0 3` and `UPDATE 3` are 3, `CREATE TABLE` is 0.
fn tag_count(tag: &str) -> u64 {
    tag.rsplit(' ').next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

// ---------------------------------------------------------------------------------------------
// errors from the server

/// An `ErrorResponse` or a `NoticeResponse`: a run of `(field type, text)` pairs.
fn parse_fields(mut fields: backend::ErrorFields<'_>, statement: usize) -> ServerError {
    let mut e = ServerError { statement, ..ServerError::default() };
    let mut localized = String::new();
    while let Ok(Some(f)) = fields.next() {
        let v = String::from_utf8_lossy(f.value_bytes()).into_owned();
        match f.type_() {
            // `V` is the severity in English whatever `lc_messages` is; `S` may be translated.
            b'V' => e.severity = v,
            b'S' => localized = v,
            b'C' => e.code = v,
            b'M' => e.message = v,
            b'D' => e.detail = Some(v),
            b'H' => e.hint = Some(v),
            b'P' => e.position = Some(v),
            b'W' => e.context = Some(v),
            _ => {}
        }
    }
    if e.severity.is_empty() {
        e.severity = localized;
    }
    e
}

// ---------------------------------------------------------------------------------------------
// values

/// Text-format values, mapped to JSON where that cannot lose anything: booleans, integers, floats
/// and JSON. `numeric` stays a string, because a decimal with 30 digits is not an f64; everything
/// else (dates, uuids, arrays, bytea, ...) is the text Postgres printed.
pub fn decode(oid: u32, raw: &[u8]) -> Value {
    let text = String::from_utf8_lossy(raw);
    let as_text = || Value::String(text.to_string());
    match oid {
        16 => match &*text {
            "t" => Value::Bool(true),
            "f" => Value::Bool(false),
            _ => as_text(),
        },
        20 | 21 | 23 => text.parse::<i64>().map_or_else(|_| as_text(), Value::from),
        // NaN and the infinities are not JSON numbers; they stay what Postgres printed.
        700 | 701 => text.parse::<f64>().ok().and_then(Number::from_f64).map_or_else(as_text, Value::Number),
        114 | 3802 => serde_json::from_str(&text).unwrap_or_else(|_| as_text()),
        _ => as_text(),
    }
}

/// The names of the built-in types an agent is likely to meet, by OID. Others are `null`; the `oid`
/// is always there to look up (`SELECT typname FROM pg_type WHERE oid = ...`).
pub fn type_name(oid: u32) -> Option<&'static str> {
    Some(match oid {
        16 => "bool",
        17 => "bytea",
        18 => "char",
        19 => "name",
        20 => "int8",
        21 => "int2",
        23 => "int4",
        25 => "text",
        26 => "oid",
        114 => "json",
        142 => "xml",
        199 => "_json",
        650 => "cidr",
        700 => "float4",
        701 => "float8",
        790 => "money",
        829 => "macaddr",
        869 => "inet",
        1000 => "_bool",
        1001 => "_bytea",
        1005 => "_int2",
        1007 => "_int4",
        1009 => "_text",
        1014 => "_bpchar",
        1015 => "_varchar",
        1016 => "_int8",
        1021 => "_float4",
        1022 => "_float8",
        1042 => "bpchar",
        1043 => "varchar",
        1082 => "date",
        1083 => "time",
        1114 => "timestamp",
        1115 => "_timestamp",
        1182 => "_date",
        1184 => "timestamptz",
        1185 => "_timestamptz",
        1186 => "interval",
        1231 => "_numeric",
        1266 => "timetz",
        1560 => "bit",
        1562 => "varbit",
        1700 => "numeric",
        2205 => "regclass",
        2249 => "record",
        2278 => "void",
        2950 => "uuid",
        2951 => "_uuid",
        3802 => "jsonb",
        3807 => "_jsonb",
        3904 => "int4range",
        3906 => "numrange",
        3908 => "tsrange",
        3910 => "tstzrange",
        3912 => "daterange",
        3926 => "int8range",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------------------------
// connecting

/// The trust store: Mozilla's roots (the same ones `ureq` uses), plus one test CA from
/// `SLIPLANE_PG_CA_FILE` if that is set. Verification itself is never relaxed.
fn tls_config() -> Result<Arc<ClientConfig>, PgError> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = std::env::var_os(CA_FILE_ENV).filter(|p| !p.is_empty()) {
        let shown = path.to_string_lossy().into_owned();
        let certs = CertificateDer::pem_file_iter(&path)
            .map_err(|e| PgError::Config(format!("{CA_FILE_ENV} ({shown}) could not be read: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| PgError::Config(format!("{CA_FILE_ENV} ({shown}) is not valid PEM: {e}")))?;
        if certs.is_empty() {
            return Err(PgError::Config(format!("{CA_FILE_ENV} ({shown}) holds no certificate.")));
        }
        for cert in certs {
            roots.add(cert).map_err(|e| PgError::Config(format!("{CA_FILE_ENV}: not a usable CA certificate: {e}")))?;
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(PgError::Tls)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// A TCP connection to the first address that answers, IPv4 first (the family `allowlist add
/// --my-ip` defaults to), within the connect budget.
fn tcp_connect(host: &str, port: u16, budget: Duration) -> Result<(TcpStream, bool), PgError> {
    let mut addrs: Vec<SocketAddr> = (host, port).to_socket_addrs().map_err(PgError::Resolve)?.collect();
    addrs.sort_by_key(|a| !a.is_ipv4());
    let has_v4 = addrs.iter().any(SocketAddr::is_ipv4);
    let start = Instant::now();
    let mut last = io::Error::new(io::ErrorKind::NotFound, "the host has no addresses");
    for (i, addr) in addrs.iter().enumerate() {
        let share = budget.saturating_sub(start.elapsed()) / (addrs.len() - i) as u32;
        match TcpStream::connect_timeout(addr, share.max(Duration::from_millis(500))) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                return Ok((stream, addr.is_ipv4()));
            }
            Err(e) => last = e,
        }
    }
    Err(PgError::Connect { source: last, v4: has_v4 })
}

/// `SSLRequest`, then the TLS handshake, with the certificate checked against `host`.
fn upgrade(
    mut tcp: TcpStream,
    host: &str,
    v4: bool,
    budget: Duration,
) -> Result<StreamOwned<ClientConnection, TcpStream>, PgError> {
    let started = Instant::now();
    let remaining =
        || budget.checked_sub(started.elapsed()).filter(|d| !d.is_zero()).unwrap_or(Duration::from_millis(1));
    let early = |source: io::Error| PgError::BeforeTls { source, v4 };

    let mut request = BytesMut::new();
    frontend::ssl_request(&mut request);
    tcp.set_write_timeout(Some(remaining())).map_err(early)?;
    tcp.write_all(&request).map_err(early)?;
    let mut answer = [0u8; 1];
    tcp.set_read_timeout(Some(remaining())).map_err(early)?;
    tcp.read_exact(&mut answer).map_err(early)?;
    match answer[0] {
        b'S' => {}
        b'N' => return Err(PgError::NoTls),
        other => return Err(PgError::Protocol(format!("the server answered the SSL request with {other:#04x}"))),
    }

    let name = ServerName::try_from(host.to_string())
        .map_err(|_| PgError::Protocol(format!("'{host}' is not a valid host name to verify a certificate against")))?;
    let mut tls = ClientConnection::new(tls_config()?, name).map_err(PgError::Tls)?;
    while tls.is_handshaking() {
        tcp.set_read_timeout(Some(remaining())).map_err(early)?;
        tcp.set_write_timeout(Some(remaining())).map_err(early)?;
        if let Err(e) = tls.complete_io(&mut tcp) {
            // rustls reports its own errors wrapped in an `io::Error`; those are verdicts, not weather.
            return Err(match e.get_ref().and_then(|inner| inner.downcast_ref::<rustls::Error>()) {
                Some(tls_error) => PgError::Tls(tls_error.clone()),
                None => early(e),
            });
        }
    }
    Ok(StreamOwned::new(tls, tcp))
}

/// Connects, logs in, runs `sql`, and hangs up. Everything is bounded: the connection by
/// [`CONNECT_BUDGET`], the query by `opts.timeout` plus a grace period.
pub fn run(target: &Target, opts: &Options, sql: &str) -> Result<Vec<Statement>, PgError> {
    if sql.contains('\0') {
        return Err(PgError::InvalidSql("The SQL contains a NUL byte, which Postgres cannot take.".into()));
    }
    let budget = CONNECT_BUDGET.min(opts.timeout + GRACE);
    let (tcp, v4) = tcp_connect(&target.host, target.port, budget)?;
    let stream = upgrade(tcp, &target.host, v4, budget)?;

    let mut wire = Wire::new(stream, budget);
    wire.start(target, opts)?;
    wire.deadline = Instant::now() + opts.timeout + GRACE;
    let result = wire.query(sql, opts.max_rows);
    wire.terminate();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- canned servers -----------------------------------------------------------------------

    /// Bytes the "server" will send, and a record of what the client sent back.
    struct Canned {
        input: io::Cursor<Vec<u8>>,
        output: Vec<u8>,
        /// Hand out at most this many bytes per read, to split messages across reads.
        chunk: usize,
        /// Fail every read as the socket would when its timeout runs out.
        stall: bool,
    }

    impl Canned {
        fn new(script: Vec<u8>) -> Canned {
            Canned { input: io::Cursor::new(script), output: Vec::new(), chunk: usize::MAX, stall: false }
        }
    }

    impl Read for Canned {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.stall {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "stalled"));
            }
            let n = buf.len().min(self.chunk);
            self.input.read(&mut buf[..n])
        }
    }

    impl Write for Canned {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Transport for Canned {
        fn limit(&mut self, _: Duration) -> io::Result<()> {
            Ok(())
        }
    }

    fn msg(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut m = vec![tag];
        m.extend_from_slice(&((body.len() + 4) as u32).to_be_bytes());
        m.extend_from_slice(body);
        m
    }

    fn cstr(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }

    fn auth(code: u32, rest: &[u8]) -> Vec<u8> {
        let mut body = code.to_be_bytes().to_vec();
        body.extend_from_slice(rest);
        msg(b'R', &body)
    }

    fn ready() -> Vec<u8> {
        msg(b'Z', b"I")
    }

    fn row_description(columns: &[(&str, u32)]) -> Vec<u8> {
        let mut body = (columns.len() as u16).to_be_bytes().to_vec();
        for (name, oid) in columns {
            body.extend(cstr(name));
            body.extend(0u32.to_be_bytes()); // table oid
            body.extend(0u16.to_be_bytes()); // column number
            body.extend(oid.to_be_bytes());
            body.extend((-1i16).to_be_bytes()); // type size
            body.extend((-1i32).to_be_bytes()); // type modifier
            body.extend(0u16.to_be_bytes()); // text format
        }
        msg(b'T', &body)
    }

    fn data_row(values: &[Option<&str>]) -> Vec<u8> {
        let mut body = (values.len() as u16).to_be_bytes().to_vec();
        for v in values {
            match v {
                Some(v) => {
                    body.extend((v.len() as i32).to_be_bytes());
                    body.extend(v.as_bytes());
                }
                None => body.extend((-1i32).to_be_bytes()),
            }
        }
        msg(b'D', &body)
    }

    fn command_complete(tag: &str) -> Vec<u8> {
        msg(b'C', &cstr(tag))
    }

    fn fields(parts: &[(u8, &str)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (kind, text) in parts {
            body.push(*kind);
            body.extend(cstr(text));
        }
        body.push(0);
        body
    }

    fn error_response(code: &str, message: &str) -> Vec<u8> {
        msg(
            b'E',
            &fields(&[
                (b'S', "FEHLER"),
                (b'V', "ERROR"),
                (b'C', code),
                (b'M', message),
                (b'P', "8"),
                (b'H', "try less"),
            ]),
        )
    }

    fn target() -> Target {
        Target {
            host: "db.example".into(),
            port: 5432,
            database: "app".into(),
            user: "app".into(),
            password: "correct-horse-battery".into(),
        }
    }

    fn opts() -> Options {
        Options { read_only: true, timeout: Duration::from_secs(30), max_rows: 1000 }
    }

    fn wire(script: Vec<u8>) -> Wire<Canned> {
        Wire::new(Canned::new(script), Duration::from_secs(5))
    }

    fn logged_in() -> Vec<u8> {
        [auth(0, b""), msg(b'S', &[cstr("server_version"), cstr("18.0")].concat()), msg(b'K', &[0; 8]), ready()]
            .concat()
    }

    fn sent(w: &Wire<Canned>) -> String {
        String::from_utf8_lossy(&w.io.output).into_owned()
    }

    // --- logging in ---------------------------------------------------------------------------

    #[test]
    fn the_startup_packet_carries_the_timeout_and_the_read_only_default() {
        let mut w = wire(logged_in());
        w.start(&target(), &opts()).unwrap();
        let startup = sent(&w);
        for want in [
            "user\0app\0",
            "database\0app\0",
            "application_name\0sliplane-cli\0",
            "statement_timeout\x0030000\0",
            "default_transaction_read_only\0on\0",
        ] {
            assert!(startup.contains(want), "{want:?} missing from {startup:?}");
        }
        assert!(!startup.contains("correct-horse"), "the password is not part of the startup packet");

        let mut w = wire(logged_in());
        w.start(&target(), &Options { read_only: false, ..opts() }).unwrap();
        assert!(!sent(&w).contains("default_transaction_read_only"), "--write leaves the server's default alone");
    }

    #[test]
    fn scram_is_started_without_revealing_the_password() {
        let mechanisms = [cstr("SCRAM-SHA-256"), vec![0]].concat();
        // The script ends after the offer, so the exchange stops where the client waits for the server.
        let mut w = wire(auth(10, &mechanisms));
        let err = w.start(&target(), &opts()).unwrap_err();
        assert!(matches!(err, PgError::Io(ref e) if e.kind() == io::ErrorKind::UnexpectedEof), "{err:?}");
        let out = sent(&w);
        assert!(out.contains("SCRAM-SHA-256\0"), "{out:?}");
        assert!(out.contains("n,,n=,r="), "the client-first message: {out:?}");
        assert!(!out.contains("correct-horse"));
    }

    #[test]
    fn only_scram_sha_256_is_accepted() {
        let plus = [cstr("SCRAM-SHA-256-PLUS"), vec![0]].concat();
        let err = wire(auth(10, &plus)).start(&target(), &opts()).unwrap_err();
        assert!(matches!(err, PgError::Auth(ref m) if m.contains("SCRAM-SHA-256-PLUS")), "{err:?}");

        let err = wire(auth(5, &[1, 2, 3, 4])).start(&target(), &opts()).unwrap_err();
        assert!(matches!(err, PgError::Auth(ref m) if m.contains("md5")), "{err:?}");
        let err = wire(auth(3, b"")).start(&target(), &opts()).unwrap_err();
        assert!(matches!(err, PgError::Auth(ref m) if m.contains("cleartext")), "{err:?}");
        let err = wire(auth(7, b"")).start(&target(), &opts()).unwrap_err();
        assert!(matches!(err, PgError::Auth(ref m) if m.contains("GSS")), "{err:?}");
    }

    #[test]
    fn a_refused_login_carries_the_servers_fields() {
        let err =
            wire(error_response("28P01", "password authentication failed")).start(&target(), &opts()).unwrap_err();
        let PgError::Server(e) = err else { panic!("{err:?}") };
        assert_eq!(
            (e.severity.as_str(), e.code.as_str(), e.message.as_str()),
            ("ERROR", "28P01", "password authentication failed")
        );
        assert_eq!((e.position.as_deref(), e.hint.as_deref(), e.statement), (Some("8"), Some("try less"), 0));
    }

    // --- results ------------------------------------------------------------------------------

    #[test]
    fn rows_come_back_as_objects_with_json_types() {
        let columns =
            [("id", 23), ("n", 1700), ("ok", 16), ("j", 3802), ("big", 20), ("f", 701), ("nothing", 25), ("t", 25)];
        let script = [
            row_description(&columns),
            data_row(&[
                Some("1"),
                Some("12345678901234567890.123456789"),
                Some("t"),
                Some(r#"{"a":[1,{"b":null}]}"#),
                Some("9223372036854775807"),
                Some("1.5"),
                None,
                Some("hi"),
            ]),
            data_row(&[Some("2"), Some("0.1"), Some("f"), Some("null"), Some("-1"), Some("NaN"), None, Some("")]),
            command_complete("SELECT 2"),
            ready(),
        ]
        .concat();
        let result = wire(script).query("select ...", 1000).unwrap();
        assert_eq!(result.len(), 1);
        let s = &result[0];
        assert_eq!((s.tag.as_str(), s.row_count, s.truncated), ("SELECT 2", 2, false));
        let first = Value::Object(s.rows[0].clone());
        assert_eq!(first["id"], 1);
        assert_eq!(first["n"], "12345678901234567890.123456789", "numeric stays exact, as text");
        assert_eq!(first["ok"], true);
        assert_eq!(first["j"], serde_json::json!({"a": [1, {"b": null}]}));
        assert_eq!(first["big"].as_i64(), Some(i64::MAX));
        assert_eq!(first["f"], 1.5);
        assert!(first["nothing"].is_null());
        assert_eq!(first["t"], "hi");
        let second = Value::Object(s.rows[1].clone());
        assert_eq!((second["ok"].clone(), second["f"].clone()), (Value::Bool(false), Value::String("NaN".into())));
        assert!(second["j"].is_null(), "a JSON null is null");
        // Column order is the query's.
        assert_eq!(
            s.rows[0].keys().map(String::as_str).collect::<Vec<_>>(),
            ["id", "n", "ok", "j", "big", "f", "nothing", "t"]
        );

        let j = s.to_json();
        assert_eq!(j["columns"][0], serde_json::json!({"name": "id", "type": "int4", "oid": 23}));
        assert_eq!(j["columns"][3]["type"], "jsonb");
        assert_eq!((j["rowCount"].clone(), j["truncated"].clone()), (Value::from(2), Value::Bool(false)));
    }

    #[test]
    fn several_statements_each_get_a_result() {
        let script = [
            row_description(&[("x", 23)]),
            data_row(&[Some("1")]),
            command_complete("SELECT 1"),
            command_complete("CREATE TABLE"),
            command_complete("INSERT 0 3"),
            row_description(&[("a", 23)]),
            command_complete("SELECT 0"),
            ready(),
        ]
        .concat();
        let result = wire(script).query("...", 10).unwrap();
        let tags: Vec<_> = result.iter().map(|s| (s.tag.as_str(), s.row_count, s.columns.is_some())).collect();
        assert_eq!(
            tags,
            [("SELECT 1", 1, true), ("CREATE TABLE", 0, false), ("INSERT 0 3", 3, false), ("SELECT 0", 0, true)]
        );
        let j = result[1].to_json();
        assert!(j.get("rows").is_none() && j.get("columns").is_none(), "no result set, no rows: {j}");
        assert_eq!(result[3].to_json()["rows"], serde_json::json!([]), "an empty result set is still one");
    }

    #[test]
    fn rows_past_the_cap_are_counted_not_kept() {
        let mut script = vec![row_description(&[("g", 23)])];
        script.extend((1..=5).map(|i| data_row(&[Some(&i.to_string())])));
        script.extend([command_complete("SELECT 5"), ready()]);
        let result = wire(script.concat()).query("...", 2).unwrap();
        let s = &result[0];
        assert_eq!((s.rows.len(), s.row_count, s.truncated), (2, 5, true));
        assert_eq!(s.to_json()["truncated"], true);
        assert_eq!(s.rows[1]["g"], 2);
    }

    #[test]
    fn repeated_column_names_get_distinct_keys_and_a_warning() {
        let script = [
            row_description(&[("a", 23), ("a", 23), ("a_2", 23), ("b", 23)]),
            data_row(&[Some("1"), Some("2"), Some("3"), Some("4")]),
            command_complete("SELECT 1"),
            ready(),
        ]
        .concat();
        let s = &wire(script).query("...", 10).unwrap()[0];
        assert_eq!(s.rows[0].keys().map(String::as_str).collect::<Vec<_>>(), ["a", "a_2", "a_2_2", "b"]);
        assert_eq!(Value::Object(s.rows[0].clone()), serde_json::json!({"a": 1, "a_2": 2, "a_2_2": 3, "b": 4}));
        let j = s.to_json();
        assert_eq!(j["columns"][1], serde_json::json!({"name": "a", "key": "a_2", "type": "int4", "oid": 23}));
        assert!(j["columns"][3].get("key").is_none());
        assert_eq!(j["warnings"].as_array().unwrap().len(), 2);
        assert!(j["warnings"][0].as_str().unwrap().contains("'a_2'"));
    }

    #[test]
    fn an_error_names_the_statement_it_happened_in() {
        let script = [
            command_complete("SELECT 1"),
            msg(b'I', b""),
            error_response("42P01", "relation \"x\" does not exist"),
            ready(),
        ]
        .concat();
        let err = wire(script).query("select 1; ; select * from x", 10).unwrap_err();
        let PgError::Server(e) = err else { panic!("{err:?}") };
        assert_eq!((e.code.as_str(), e.statement), ("42P01", 3), "an empty statement counts as one");
    }

    #[test]
    fn notices_ride_along_with_their_statement() {
        let notice = msg(
            b'N',
            &fields(&[(b'S', "NOTICE"), (b'V', "NOTICE"), (b'M', "relation \"t\" already exists, skipping")]),
        );
        let script = [notice, command_complete("CREATE TABLE"), ready()].concat();
        let s = &wire(script).query("...", 10).unwrap()[0];
        assert_eq!(s.notices, ["NOTICE: relation \"t\" already exists, skipping"]);
        assert_eq!(s.to_json()["notices"][0], "NOTICE: relation \"t\" already exists, skipping");
    }

    #[test]
    fn copy_is_refused() {
        let copy_out = msg(b'H', &[0, 0, 1, 0, 0]);
        let err = wire([copy_out, ready()].concat()).query("copy t to stdout", 10).unwrap_err();
        assert!(matches!(err, PgError::Unsupported(_)), "{err:?}");
    }

    #[test]
    fn a_message_split_across_many_reads_is_still_one_message() {
        let script = [
            row_description(&[("x", 25)]),
            data_row(&[Some("a long enough value to span reads")]),
            command_complete("SELECT 1"),
            ready(),
        ]
        .concat();
        let mut w = wire(script);
        w.io.chunk = 3;
        let s = &w.query("...", 10).unwrap()[0];
        assert_eq!(s.rows[0]["x"], "a long enough value to span reads");
    }

    #[test]
    fn silence_is_a_timeout_and_an_absurd_message_is_a_protocol_error() {
        let mut w = wire(Vec::new());
        w.io.stall = true;
        assert!(matches!(w.query("select pg_sleep(100)", 10).unwrap_err(), PgError::Timeout));

        let mut w = wire(Vec::new());
        w.deadline = Instant::now();
        assert!(matches!(w.query("select 1", 10).unwrap_err(), PgError::Timeout), "no time left is no time");

        let mut huge = vec![b'D'];
        huge.extend(u32::MAX.to_be_bytes());
        let err = wire(huge).query("...", 10).unwrap_err();
        assert!(matches!(err, PgError::Protocol(_)), "{err:?}");
    }

    #[test]
    fn a_nul_byte_in_the_sql_is_refused_before_anything_is_sent() {
        let err = run(&target(), &opts(), "select 1\0").unwrap_err();
        assert!(matches!(err, PgError::InvalidSql(_)), "{err:?}");
    }

    // --- values -------------------------------------------------------------------------------

    #[test]
    fn decode_maps_only_what_it_can_map_exactly() {
        assert_eq!(decode(16, b"t"), true);
        assert_eq!(decode(16, b"f"), false);
        assert_eq!(decode(21, b"-7"), -7);
        assert_eq!(decode(23, b"42"), 42);
        assert_eq!(decode(20, b"-9223372036854775808").as_i64(), Some(i64::MIN));
        assert_eq!(decode(700, b"1.1"), 1.1);
        assert_eq!(decode(701, b"1e+20").as_f64(), Some(1e20));
        assert_eq!(decode(701, b"Infinity"), "Infinity");
        assert_eq!(decode(701, b"-Infinity"), "-Infinity");
        assert_eq!(decode(1700, b"0.10"), "0.10", "numeric keeps its scale");
        assert_eq!(decode(114, br#"[1,"a"]"#), serde_json::json!([1, "a"]));
        assert_eq!(decode(3802, b"not json"), "not json");
        assert_eq!(decode(2950, b"11111111-2222-3333-4444-555555555555"), "11111111-2222-3333-4444-555555555555");
        assert_eq!(decode(1007, b"{1,2}"), "{1,2}", "arrays stay text");
        assert_eq!(decode(25, "caf\u{e9}".as_bytes()), "caf\u{e9}");
        assert_eq!(decode(25, b"bad \xff byte"), "bad \u{fffd} byte");
    }

    #[test]
    fn type_names_cover_the_common_builtins() {
        assert_eq!(type_name(23), Some("int4"));
        assert_eq!(type_name(1184), Some("timestamptz"));
        assert_eq!(type_name(3802), Some("jsonb"));
        assert_eq!(type_name(99999), None);
    }

    #[test]
    fn tag_counts() {
        assert_eq!(tag_count("INSERT 0 3"), 3);
        assert_eq!(tag_count("UPDATE 12"), 12);
        assert_eq!(tag_count("CREATE TABLE"), 0);
        assert_eq!(tag_count("SET"), 0);
        assert_eq!(tag_count(""), 0);
    }

    #[test]
    fn the_target_does_not_debug_print_its_password() {
        let shown = format!("{:?}", target());
        assert!(!shown.contains("correct-horse") && shown.contains("<hidden>"), "{shown}");
    }
}
