//! `postgres query` and `postgres connect`: the two commands that talk to the database itself,
//! with the credentials `GET /postgres/{id}` hands out, instead of to the Sliplane API.
//!
//! `query` is for an agent: rows come back as data (see [`crate::pgwire`] for the wire protocol and
//! the value mapping), the session is **read-only unless `--write` is given**, and the database
//! password never reaches stdout, stderr or an error. `connect` is for a person: it only starts
//! `psql` with the connection in its environment.

use std::io::{self, IsTerminal, Read};
use std::process::Command;
use std::time::Duration;

use rustls::CertificateError;
use serde_json::Value;

use super::postgres::assess;
use super::{print, resolve};
use crate::cli::PgId;
use crate::client::seg;
use crate::error::{Error, ErrorCode, Result};
use crate::pgwire::{self, Options, PgError, ServerError, Statement, Target};
use crate::{obj, timeparse};

/// What it takes to reach one database, and enough about it to explain a failure.
struct Database {
    id: String,
    account: String,
    target: Target,
    /// `blocked`, `open`, `restricted`, or `unknown` when the API did not show the list.
    access: &'static str,
    /// The ready-made connection string; never printed, only scrubbed from anything that is.
    uri: String,
}

impl Database {
    fn secrets(&self) -> Vec<&str> {
        // A one- or two-character "password" would shred every message it is scrubbed from.
        [self.target.password.as_str(), self.uri.as_str()].into_iter().filter(|s| s.len() >= 4).collect()
    }

    fn get_cmd(&self) -> String {
        format!("sliplane postgres get --postgres-id {} -a {}", self.id, self.account)
    }
}

fn fetch(p: &PgId) -> Result<Database> {
    let account = resolve(&p.a)?;
    let db = account.client().get(&format!("postgres/{}", seg(&p.postgres_id)))?;
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    let (id, account) = (p.postgres_id.clone(), account.name);

    let Some(c) = db.get("connection").filter(|c| c.is_object()) else {
        let (name, status) = (text(&db, "name"), text(&db, "status"));
        let fix = if status == "paused" {
            format!("sliplane postgres unpause --postgres-id {id} -a {account}, then wait for status running")
        } else {
            format!("sliplane postgres get --postgres-id {id} -a {account}: repeat the command once status is running")
        };
        return Err(Error::other(format!("Database '{name}' has no connection details yet (status: {status})."))
            .detail("The API includes them only while the database is running.")
            .fix(fix));
    };
    let target = Target {
        host: text(c, "host"),
        port: c.get("port").and_then(Value::as_u64).and_then(|p| u16::try_from(p).ok()).unwrap_or(0),
        database: text(c, "database"),
        user: text(c, "user"),
        password: text(c, "password"),
    };
    if target.port == 0
        || [&target.host, &target.database, &target.user].iter().any(|f| f.is_empty() || f.contains('\0'))
    {
        return Err(Error::other("The API returned incomplete connection details for the database."));
    }
    if target.password.is_empty() {
        return Err(Error::new(ErrorCode::AuthRequired, "The API key cannot read this database's password.")
            .detail("The API blanks connection.password unless the key has the secrets:read scope.")
            .fix(format!(
                "`sliplane me -a {account}` lists the key's scopes. Create a key with Secrets: Read at \
                 https://sliplane.io/app/team/api, then: sliplane accounts add {account} --api-key <key> --force"
            )));
    }
    Ok(Database {
        access: db.get("ipAllowList").filter(|l| l.is_array()).map_or("unknown", |l| assess(l).0),
        uri: text(c, "uri"),
        id,
        account,
        target,
    })
}

// ---------------------------------------------------------------------------------------------
// query

/// Where the SQL comes from: `--sql`, `--file`, or standard input for either one given `-`.
fn read_sql(sql: Option<String>, file: Option<String>) -> Result<String> {
    let text = match (sql.as_deref(), file.as_deref()) {
        (Some(_), Some(_)) => return Err(Error::invalid("Pass --sql or --file, not both.")),
        (None, None) => {
            return Err(Error::invalid(
                "Pass the SQL: --sql \"<statement>\", --file <path>, or `-` to read it from stdin (--sql - or --file -).",
            ));
        }
        (Some("-"), None) | (None, Some("-")) => {
            if io::stdin().is_terminal() {
                return Err(Error::invalid("`-` reads the SQL from standard input, which is a terminal.").fix(
                    "Pipe it in: cat query.sql | sliplane postgres query --sql - ..., or pass --sql \"<statement>\".",
                ));
            }
            let mut text = String::new();
            io::stdin().read_to_string(&mut text).map_err(|e| Error::invalid(format!("Could not read stdin: {e}")))?;
            text
        }
        (Some(s), None) => s.to_string(),
        (None, Some(path)) => std::fs::read_to_string(path)
            .map_err(|e| Error::invalid(format!("Could not read the SQL file '{path}': {e}")))?,
    };
    if text.trim().is_empty() { Err(Error::invalid("The SQL is empty.")) } else { Ok(text) }
}

fn parse_timeout(s: &str) -> Result<Duration> {
    match timeparse::duration_secs(s) {
        Some(secs) if secs <= 86_400 => Ok(Duration::from_secs(secs)),
        Some(_) => Err(Error::invalid("--timeout is at most 1d.")),
        None => Err(Error::invalid(format!("--timeout takes a duration such as 30s, 5m or 1h, not '{s}'."))),
    }
}

pub fn query(
    p: &PgId,
    sql: Option<String>,
    file: Option<String>,
    write: bool,
    max_rows: u32,
    timeout: &str,
) -> Result<()> {
    let sql = read_sql(sql, file)?;
    let timeout = parse_timeout(timeout)?;
    let db = fetch(p)?;
    let opts = Options { read_only: !write, timeout, max_rows: max_rows as usize };
    match pgwire::run(&db.target, &opts, &sql) {
        Ok(statements) => {
            let statements: Vec<Value> = statements.iter().map(Statement::to_json).collect();
            print(obj! { "readOnly" => !write, "statements" => statements })
        }
        Err(e) => Err(scrub(explain(e, &db, write, timeout), &db.secrets())),
    }
}

/// The one place errors are cleaned: the password and the URI that contains it are replaced in the
/// message, the detail and the remediation, whatever put them there.
fn scrub(e: Error, secrets: &[&str]) -> Error {
    fn clean(s: String, secrets: &[&str]) -> String {
        secrets.iter().fold(s, |s, secret| s.replace(secret, "<hidden>"))
    }
    Error {
        code: e.code,
        message: clean(e.message, secrets),
        detail: e.detail.map(|d| clean(d, secrets)),
        remediation: e.remediation.map(|r| clean(r, secrets)),
    }
}

fn explain(e: PgError, db: &Database, write: bool, timeout: Duration) -> Error {
    let t = &db.target;
    let host = format!("{}:{}", t.host, t.port);
    match e {
        PgError::Resolve(source) => {
            Error::new(ErrorCode::Network, format!("Could not resolve the database host {}.", t.host))
                .detail(source.to_string())
                .fix("Check this machine's network and DNS, then retry once.")
        }
        PgError::Connect { source, v4 } => unreachable_host(db, &host, "Connecting", &source, v4),
        PgError::BeforeTls { source, v4 } => unreachable_host(db, &host, "Before TLS was up", &source, v4),
        PgError::Tls(tls) => tls_error(&tls, &host, &t.host),
        PgError::NoTls => Error::other(format!("The server at {host} does not offer TLS."))
            .detail("Credentials are never sent without TLS, and the certificate is always checked (verify-full)."),
        PgError::Auth(message) => Error::other(message),
        PgError::Io(source) => Error::new(ErrorCode::Network, format!("Lost the connection to {host}."))
            .detail(source.to_string())
            .fix("Retry once, then stop."),
        PgError::Timeout => Error::new(
            ErrorCode::Network,
            format!("The database did not answer within {}s (--timeout plus a few seconds' grace).", timeout.as_secs()),
        )
        .fix("Raise --timeout, or make the query cheaper."),
        PgError::Protocol(message) => Error::other(format!("Unexpected data from the database: {message}")),
        PgError::Unsupported(message) | PgError::Config(message) => Error::invalid(message),
        PgError::InvalidSql(message) => Error::invalid(message),
        PgError::Server(s) => server_error(&s, db, write, timeout),
    }
}

fn describe_access(access: &str) -> &'static str {
    match access {
        "blocked" => "empty, so every address is refused",
        "open" => "open to every address",
        "restricted" => "restricted to the addresses it lists",
        _ => "not shown by the API",
    }
}

/// No connection, or one that died before TLS: refused means nothing is listening; anything else
/// (timeouts, resets) is what an IP allow list that does not know this machine looks like.
fn unreachable_host(db: &Database, host: &str, phase: &str, source: &io::Error, v4: bool) -> Error {
    let refused = source.kind() == io::ErrorKind::ConnectionRefused;
    let e = Error::new(ErrorCode::Network, format!("Could not connect to the database at {host}."))
        .detail(format!("{phase}: {source}. The database's IP allow list is {}.", describe_access(db.access)));
    if refused {
        e.fix(format!(
            "Nothing is listening: the database may be paused or restarting. Check its status: {}",
            db.get_cmd()
        ))
    } else if db.access == "open" {
        e.fix(format!(
            "Retry once. The IP allow list is open, so it is not the cause; check the database's status: {}",
            db.get_cmd()
        ))
    } else {
        e.fix(format!(
            "If this machine's address is not on the IP allow list: sliplane postgres allowlist add --postgres-id {} --my-ip{} -a {}",
            db.id,
            if v4 { " --ipv4" } else { " --ipv6" },
            db.account
        ))
    }
}

fn tls_error(e: &rustls::Error, host: &str, name: &str) -> Error {
    let verdict = match e {
        rustls::Error::InvalidCertificate(CertificateError::NotValidForName)
        | rustls::Error::InvalidCertificate(CertificateError::NotValidForNameContext { .. }) => Error::other(format!(
            "The server at {host} presented a certificate that is not valid for the host name '{name}'."
        ))
        .fix("The host name must match the certificate; verification is never skipped. Connect with the name the certificate carries."),
        rustls::Error::InvalidCertificate(CertificateError::UnknownIssuer) => {
            Error::other(format!("The server at {host} presented a certificate from an authority this machine does not trust."))
                .fix(format!(
                    "For a private CA, save its certificate as PEM and set {}=<file>. Verification stays on.",
                    pgwire::CA_FILE_ENV
                ))
        }
        rustls::Error::InvalidCertificate(_) => Error::other(format!("The server at {host} presented a certificate that was rejected.")),
        _ => Error::new(ErrorCode::Network, format!("The TLS handshake with {host} failed.")).fix("Retry once, then stop."),
    };
    verdict.detail(e.to_string())
}

fn server_error(s: &ServerError, db: &Database, write: bool, timeout: Duration) -> Error {
    let message =
        if s.code.is_empty() { s.message.clone() } else { format!("Postgres error {}: {}", s.code, s.message) };
    let mut detail = vec![format!("severity: {}", s.severity), format!("sqlstate: {}", s.code)];
    for (key, value) in [("position", &s.position), ("detail", &s.detail), ("hint", &s.hint), ("where", &s.context)] {
        if let Some(v) = value {
            detail.push(format!("{key}: {v}"));
        }
    }
    if s.statement > 1 && write {
        detail.push(format!(
            "statement: {} (a query string with several statements is one transaction unless it commits itself, so the ones before it were rolled back)",
            s.statement
        ));
    } else if s.statement >= 1 {
        detail.push(format!("statement: {}", s.statement));
    }

    let lower = s.message.to_ascii_lowercase();
    let fix = match s.code.as_str() {
        "25006" if !write => Some(
            "Sessions are read-only unless --write is given. Repeat the command with --write if this statement is meant to change data."
                .to_string(),
        ),
        "25006" => Some("The server itself is read-only for this session (a replica, or default_transaction_read_only is set on the role or database).".to_string()),
        "57014" if lower.contains("statement timeout") => Some(format!(
            "The statement ran longer than --timeout ({}s). Raise it (--timeout 5m) or make the query cheaper.",
            timeout.as_secs()
        )),
        "28000" | "28P01" if lower.contains("pg_hba") => Some(format!(
            "If this machine's address is not on the IP allow list: sliplane postgres allowlist add --postgres-id {} --my-ip --ipv4 -a {}",
            db.id, db.account
        )),
        "28P01" | "28000" => Some(format!(
            "The password comes from the Sliplane API ({}). If the credentials were rotated a moment ago, retry once.",
            db.get_cmd()
        )),
        "57P03" | "53300" => Some("The database is starting up or has no free connections. Retry shortly.".to_string()),
        "08P01" if lower.contains("startup parameter") => Some(
            "A proxy in front of the database refused a startup parameter this command sends (statement_timeout, default_transaction_read_only)."
                .to_string(),
        ),
        _ => None,
    };
    let e = Error::other(message).detail(detail.join("\n"));
    match fix {
        Some(f) => e.fix(f),
        None => e,
    }
}

// ---------------------------------------------------------------------------------------------
// connect

/// What `psql` is given. All of it is environment, none of it argv: argv is visible to every
/// process on the machine, and the environment is not. `PGSSLROOTCERT=system` (psql 16 and newer)
/// makes `verify-full` check against the system's roots instead of `~/.postgresql/root.crt`.
fn psql_env(t: &Target, root_cert_set: bool) -> Vec<(&'static str, String)> {
    let mut env = vec![
        ("PGHOST", t.host.clone()),
        ("PGPORT", t.port.to_string()),
        ("PGDATABASE", t.database.clone()),
        ("PGUSER", t.user.clone()),
        ("PGPASSWORD", t.password.clone()),
        ("PGSSLMODE", "verify-full".to_string()),
        ("PGAPPNAME", "sliplane-cli".to_string()),
    ];
    if !root_cert_set {
        env.push(("PGSSLROOTCERT", "system".to_string()));
    }
    env
}

pub fn connect(p: &PgId) -> Result<()> {
    let db = fetch(p)?;
    let mut psql = Command::new("psql");
    // Settings that would send this connection somewhere else or log in as someone else.
    for var in ["PGSERVICE", "PGSERVICEFILE", "PGPASSFILE", "PGHOSTADDR", "PGOPTIONS", "PGSSLCERT", "PGSSLKEY"] {
        psql.env_remove(var);
    }
    psql.envs(psql_env(&db.target, std::env::var_os("PGSSLROOTCERT").is_some()));
    match psql.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(Error::other(format!("psql exited with {status}."))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(Error::other("psql was not found on PATH.")
            .detail("`postgres connect` starts psql; it does not contain a shell of its own.")
            .fix("Install the PostgreSQL client tools (psql 16 or newer), or run SQL without them: sliplane postgres query --sql \"<statement>\"")),
        Err(e) => Err(Error::other(format!("Could not start psql: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(access: &'static str) -> Database {
        Database {
            id: "pg_1".into(),
            account: "work".into(),
            target: Target {
                host: "pg-1.db.sliplane.app".into(),
                port: 5432,
                database: "app".into(),
                user: "app".into(),
                password: "hunter2hunter2".into(),
            },
            access,
            uri: "postgres://app:hunter2hunter2@pg-1.db.sliplane.app:5432/app".into(),
        }
    }

    fn server(code: &str, message: &str) -> ServerError {
        ServerError {
            severity: "ERROR".into(),
            code: code.into(),
            message: message.into(),
            statement: 1,
            ..ServerError::default()
        }
    }

    #[test]
    fn a_read_only_refusal_says_to_pass_write() {
        let e = server_error(
            &server("25006", "cannot execute INSERT in a read-only transaction"),
            &db("open"),
            false,
            Duration::from_secs(30),
        );
        assert_eq!(e.code, ErrorCode::Error);
        assert!(e.message.contains("25006") && e.message.contains("read-only transaction"), "{}", e.message);
        assert!(e.remediation.unwrap().contains("--write"));
        // With --write already given, the advice is not to pass it again.
        let e = server_error(
            &server("25006", "cannot execute INSERT in a read-only transaction"),
            &db("open"),
            true,
            Duration::from_secs(30),
        );
        assert!(!e.remediation.unwrap().contains("Repeat the command with --write"));
    }

    #[test]
    fn server_error_fields_land_in_the_detail() {
        let mut s = server("42P01", "relation \"nope\" does not exist");
        s.position = Some("15".into());
        s.hint = Some("Perhaps you meant yes.".into());
        s.statement = 3;
        // Read-only, nothing can have been written, so there is nothing to say was rolled back.
        let e = server_error(&s, &db("open"), false, Duration::from_secs(30));
        let d = e.detail.unwrap();
        assert!(d.contains("statement: 3") && !d.contains("rolled back"), "{d}");
        let e = server_error(&s, &db("open"), true, Duration::from_secs(30));
        let d = e.detail.unwrap();
        for want in [
            "severity: ERROR",
            "sqlstate: 42P01",
            "position: 15",
            "hint: Perhaps you meant yes.",
            "statement: 3",
            "rolled back",
        ] {
            assert!(d.contains(want), "{want} missing from {d}");
        }
        assert!(e.remediation.is_none());
    }

    #[test]
    fn a_statement_timeout_names_the_flag() {
        let e = server_error(
            &server("57014", "canceling statement due to statement timeout"),
            &db("open"),
            false,
            Duration::from_secs(7),
        );
        let fix = e.remediation.unwrap();
        assert!(fix.contains("--timeout") && fix.contains("7s"), "{fix}");
    }

    #[test]
    fn a_silent_network_points_at_the_allow_list_unless_it_is_open_or_refused() {
        let timeout = || io::Error::new(io::ErrorKind::TimedOut, "timed out");
        let e = explain(
            PgError::Connect { source: timeout(), v4: true },
            &db("restricted"),
            false,
            Duration::from_secs(30),
        );
        assert_eq!(e.code, ErrorCode::Network);
        assert_eq!(
            e.remediation.unwrap(),
            "If this machine's address is not on the IP allow list: sliplane postgres allowlist add --postgres-id pg_1 --my-ip --ipv4 -a work"
        );
        let e = explain(
            PgError::BeforeTls { source: timeout(), v4: false },
            &db("blocked"),
            false,
            Duration::from_secs(30),
        );
        assert!(e.remediation.unwrap().contains("--ipv6"));
        let e = explain(PgError::Connect { source: timeout(), v4: true }, &db("open"), false, Duration::from_secs(30));
        assert!(!e.remediation.unwrap().contains("allowlist add"));
        let refused = io::Error::new(io::ErrorKind::ConnectionRefused, "refused");
        let e =
            explain(PgError::Connect { source: refused, v4: true }, &db("restricted"), false, Duration::from_secs(30));
        assert!(e.remediation.unwrap().contains("paused or restarting"));
    }

    #[test]
    fn a_pg_hba_rejection_is_an_allow_list_problem_too() {
        let e = server_error(
            &server(
                "28000",
                "no pg_hba.conf entry for host \"203.0.113.9\", user \"app\", database \"app\", SSL encryption",
            ),
            &db("restricted"),
            false,
            Duration::from_secs(30),
        );
        assert!(e.remediation.unwrap().contains("allowlist add"));
    }

    #[test]
    fn the_password_and_the_uri_are_scrubbed_from_every_part_of_an_error() {
        let d = db("open");
        let e = Error::other("login failed for hunter2hunter2")
            .detail(format!("dsn {} failed", d.uri))
            .fix("retry with hunter2hunter2");
        let e = scrub(e, &d.secrets());
        let all = format!("{} {} {}", e.message, e.detail.unwrap(), e.remediation.unwrap());
        assert!(!all.contains("hunter2"), "{all}");
        assert!(all.contains("<hidden>"));
    }

    #[test]
    fn tls_verdicts_name_the_way_out() {
        let name = rustls::Error::InvalidCertificate(CertificateError::NotValidForName);
        let e = tls_error(&name, "127.0.0.1:5432", "127.0.0.1");
        assert!(e.message.contains("not valid for the host name '127.0.0.1'"));
        assert!(e.remediation.unwrap().contains("never skipped"));
        let issuer = rustls::Error::InvalidCertificate(CertificateError::UnknownIssuer);
        assert!(tls_error(&issuer, "h:1", "h").remediation.unwrap().contains(pgwire::CA_FILE_ENV));
    }

    #[test]
    fn psql_gets_the_password_in_its_environment_only() {
        let t = db("open").target;
        let env = psql_env(&t, false);
        assert!(env.contains(&("PGPASSWORD", "hunter2hunter2".to_string())));
        assert!(env.contains(&("PGSSLMODE", "verify-full".to_string())));
        assert!(env.contains(&("PGSSLROOTCERT", "system".to_string())));
        // A root certificate the user configured is theirs to keep.
        assert!(!psql_env(&t, true).iter().any(|(k, _)| *k == "PGSSLROOTCERT"));
    }

    #[test]
    fn timeouts_parse_and_are_bounded() {
        assert_eq!(parse_timeout("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_timeout("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_timeout("0s").unwrap_err().code, ErrorCode::InvalidInput);
        assert_eq!(parse_timeout("30").unwrap_err().code, ErrorCode::InvalidInput);
        assert_eq!(parse_timeout("2d").unwrap_err().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn the_sql_has_to_be_given_once() {
        assert!(read_sql(None, None).is_err());
        assert!(read_sql(Some("select 1".into()), Some("x.sql".into())).is_err());
        assert_eq!(read_sql(Some("select 1".into()), None).unwrap(), "select 1");
        assert!(read_sql(Some("   ".into()), None).unwrap_err().message.contains("empty"));
        assert!(read_sql(None, Some("/no/such/file.sql".into())).unwrap_err().message.contains("Could not read"));
    }
}
