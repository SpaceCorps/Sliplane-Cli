//! `services ssh` and `services tunnel`: a login to a service over plain OpenSSH, with a key pair
//! that exists for one session.
//!
//! The flow is the official CLI's, minus its ControlMaster (Windows OpenSSH has none, so nothing
//! here multiplexes): generate an ed25519 key with the system `ssh-keygen` in a private directory,
//! register the public half with an expiry, attach it to the service (the response says where to
//! connect), wait until sshd accepts it, run `ssh`, and then *always* detach the key, delete it and
//! remove the directory. The expiry is only the fallback for when this process is killed outright.
//!
//! Host keys: `StrictHostKeyChecking=accept-new` against a `known_hosts` file of our own in the
//! config directory. The first connection to a host:port is trusted, which is the weak point (a
//! machine in the middle on that first connection goes unnoticed); every later one is verified,
//! and a changed key stops the command. A file per run would trust every connection blindly, and
//! the user's own `~/.ssh/known_hosts` is not ours to write into.
//!
//! The private key never leaves its file: not printed, and in argv only as the `-i` path.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{IsTerminal, Write};
use std::net::{IpAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::cli::ServiceRef;
use crate::client::{Client, seg};
use crate::commands::print;
use crate::error::{Error, ErrorCode, Result};
use crate::{config, obj, timeparse};

/// How often sshd is asked whether it knows the new key yet. The official CLI does the same.
const ATTEMPTS: u32 = 12;
const DEFAULT_RETRY: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_secs(45);
const TUNNEL_SETUP_TIMEOUT: Duration = Duration::from_secs(30);
const TICK: Duration = Duration::from_millis(25);

const DEFAULT_TTL_SECS: u64 = 3600;
/// The API wants a key to live at least 5 minutes from the moment it arrives.
const MIN_TTL_SECS: u64 = 360;
const MAX_TTL_SECS: u64 = 30 * 86400;

/// Set by Ctrl-C / SIGTERM / SIGHUP. The handler does nothing else: the loops below notice it,
/// stop ssh, and fall through to the cleanup, which is the only thing that matters then.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

fn check_interrupt() -> Result<()> {
    if interrupted() { Err(Error::other("Interrupted.")) } else { Ok(()) }
}

enum Mode {
    /// Run `command` on the service, or an interactive shell when there is none.
    Shell {
        command: Vec<String>,
    },
    Tunnel {
        local_port: u16,
        remote_port: u16,
        limit: Option<Duration>,
    },
}

pub fn shell(client: &Client, s: &ServiceRef, ttl: Option<&str>, command: Vec<String>) -> Result<()> {
    run(client, s, ttl_from(ttl)?, Mode::Shell { command })
}

pub fn tunnel(
    client: &Client,
    s: &ServiceRef,
    ttl: Option<&str>,
    local_port: u16,
    remote_port: u16,
    limit: Option<&str>,
) -> Result<()> {
    let limit = limit
        .map(|d| {
            timeparse::duration_secs(d)
                .map(Duration::from_secs)
                .ok_or_else(|| Error::invalid(format!("--for '{d}' is not a duration. Use 30s, 5m, 1h or 1d.")))
        })
        .transpose()?;
    run(client, s, ttl_from(ttl)?, Mode::Tunnel { local_port, remote_port, limit })
}

fn ttl_from(arg: Option<&str>) -> Result<Duration> {
    let secs = match arg {
        None => DEFAULT_TTL_SECS,
        Some(d) => timeparse::duration_secs(d)
            .ok_or_else(|| Error::invalid(format!("--ttl '{d}' is not a duration. Use 30m, 2h or 1d.")))?,
    };
    if !(MIN_TTL_SECS..=MAX_TTL_SECS).contains(&secs) {
        return Err(Error::invalid(
            "--ttl must be between 6m and 30d. The API refuses a key that lives under 5 minutes, and 6m leaves time for the requests.",
        ));
    }
    Ok(Duration::from_secs(secs))
}

// ---------------------------------------------------------------------------------------------
// The session

/// What has been created on the API side, so cleanup knows what to undo.
#[derive(Default)]
struct Created {
    key_id: Option<String>,
    expires_at: String,
    /// The attach request was sent. If it failed on the wire it may still have been applied.
    attach_sent: bool,
}

fn run(client: &Client, s: &ServiceRef, ttl: Duration, mode: Mode) -> Result<()> {
    // Everything local is checked before anything is created on the account.
    let tools = Tools::find()?;
    config::ensure_dir()?;
    let known_hosts = config::config_dir().join("known_hosts");
    if let Mode::Tunnel { local_port, .. } = &mode
        && port_in_use(*local_port)
    {
        return Err(Error::invalid(format!("Local port {local_port} is already in use."))
            .fix("Pick another with --local-port, or stop whatever is listening there."));
    }
    // Best effort: if no handler can be installed, a kill skips the cleanup and the key's
    // expiry removes it instead.
    let _ = ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::SeqCst));

    let dir = PrivateDir::create()?;
    let mut created = Created::default();
    let result = session(client, s, ttl, &mode, &tools, &known_hosts, &dir, &mut created);
    cleanup(client, s, &created, dir);
    result
}

#[allow(clippy::too_many_arguments)] // one call site, and a struct would only rename the list
fn session(
    client: &Client,
    s: &ServiceRef,
    ttl: Duration,
    mode: &Mode,
    tools: &Tools,
    known_hosts: &Path,
    dir: &PrivateDir,
    created: &mut Created,
) -> Result<()> {
    let identity = dir.0.join("key");
    let public_key = generate_key(&tools.keygen, &identity)?;
    check_interrupt()?;

    created.expires_at = timeparse::format_rfc3339(timeparse::now() + ttl.as_secs() as i64);
    let key = client.post(
        "ssh-keys",
        &obj! {
            "name" => format!("Sliplane CLI: temporary key for {}", s.service_id),
            "publicKey" => public_key,
            "expiresAt" => created.expires_at,
        },
    )?;
    let key_id = key.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()).ok_or_else(|| {
        Error::other("The API created an SSH key but did not return its id, so it cannot be attached or removed.")
            .detail(format!("It expires by itself at {}. 'sliplane ssh-keys list' shows it.", created.expires_at))
    })?;
    created.key_id = Some(key_id.to_string());
    check_interrupt()?;

    created.attach_sent = true;
    let attached = client.put_empty(&format!("{}/ssh-keys/{}", service_path(s), seg(key_id)))?;
    let connection = Connection::from_api(&attached)?;
    check_interrupt()?;

    let base = base_args(&connection, &identity, known_hosts);
    wait_for_access(tools, &connection, &base, known_hosts, &dir.0.join("probe.log"))?;
    match mode {
        Mode::Shell { command } => run_shell(tools, &connection, &base, command),
        Mode::Tunnel { local_port, remote_port, limit } => {
            run_tunnel(tools, s, &connection, &base, *local_port, *remote_port, *limit)
        }
    }
}

fn service_path(s: &ServiceRef) -> String {
    format!("projects/{}/services/{}", seg(&s.project_id), seg(&s.service_id))
}

/// Undo what the session created, whatever became of it. Each step runs even if an earlier one
/// failed, and each failure is a warning rather than silence: a key that stays attached is a
/// login that stays open until it expires.
fn cleanup(client: &Client, s: &ServiceRef, created: &Created, dir: PrivateDir) {
    if let Some(id) = &created.key_id {
        let mut clean = true;
        if created.attach_sent {
            let path = format!("{}/ssh-keys/{}", service_path(s), seg(id));
            clean &= settled(client.delete(&path), "detach the temporary SSH key from the service");
        }
        clean &= settled(client.delete(&format!("ssh-keys/{}", seg(id))), "delete the temporary SSH key");
        if !clean {
            let account = s.a.account.as_deref().unwrap_or("<account>");
            say(format!(
                "warning: the temporary SSH key {id} may still exist. It deletes itself at {}. To remove it now: \
                 sliplane services detach-ssh-key --project-id {} --service-id {} --key-id {id} -a {account}, then \
                 sliplane ssh-keys delete --key-id {id} -a {account}",
                created.expires_at, s.project_id, s.service_id
            ));
        }
    }
    let path = dir.0.clone();
    if let Err(e) = dir.remove() {
        say(format!("warning: could not remove {}, which holds the temporary private key: {e}", path.display()));
    }
}

/// A line on stderr that cannot panic. `eprintln!` does when stderr is gone (a closed terminal, an
/// agent that dropped the pipe), and that must not be able to stop the cleanup.
fn say(line: impl std::fmt::Display) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

/// True when the step is done: it worked, or the thing it removes is already gone.
fn settled(result: Result<Value>, what: &str) -> bool {
    match result {
        Ok(_) => true,
        Err(e) if e.code == ErrorCode::NotFound => true,
        Err(e) => {
            let detail = e.detail.as_deref().map(|d| format!(" ({d})")).unwrap_or_default();
            say(format!("warning: could not {what}: {}{detail}", e.message));
            false
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The tools

struct Tools {
    ssh: PathBuf,
    keygen: PathBuf,
}

impl Tools {
    /// `SLIPLANE_SSH` / `SLIPLANE_SSH_KEYGEN` name another program (the tests use a fake); otherwise
    /// the one on PATH.
    fn find() -> Result<Tools> {
        Ok(Tools { ssh: find_tool("ssh", "SLIPLANE_SSH")?, keygen: find_tool("ssh-keygen", "SLIPLANE_SSH_KEYGEN")? })
    }
}

fn find_tool(name: &str, var: &str) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(var).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(path);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(Error::invalid(format!("{var} is {}, which is not a file.", path.display())))
        };
    }
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).map(|dir| dir.join(&file)).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| Error::other(format!("{name} was not found on PATH.")).fix(install_openssh()))
}

fn install_openssh() -> &'static str {
    if cfg!(windows) {
        "Install the OpenSSH client: Settings > System > Optional features > OpenSSH Client (or Git for Windows, which ships one), then open a new terminal."
    } else if cfg!(target_os = "macos") {
        "macOS ships OpenSSH; check that /usr/bin is on PATH."
    } else {
        "Install the OpenSSH client, e.g. 'sudo apt install openssh-client' or 'apk add openssh-client'."
    }
}

fn spawn_error(tool: &Path, e: std::io::Error) -> Error {
    Error::other(format!("Could not start {}: {e}", tool.display()))
}

/// A directory only this user can enter, which holds the private key for one session. 0700 on
/// Unix. On Windows it is a fresh directory under the per-user %TEMP% (not readable by other
/// non-admin users by default), and the key file's ACL is the one `ssh-keygen` gave it; `ssh`
/// itself refuses a private key that other users can read, so a bad ACL fails loudly. A process
/// killed without cleanup leaves the directory behind; the key in it stops working at its expiry.
struct PrivateDir(PathBuf);

impl PrivateDir {
    fn create() -> Result<PrivateDir> {
        let base = std::env::temp_dir();
        for _ in 0..8 {
            let mut random = [0u8; 8];
            getrandom::fill(&mut random)
                .map_err(|e| Error::other("The system could not provide random bytes.").detail(e.to_string()))?;
            let name: String = random.iter().map(|b| format!("{b:02x}")).collect();
            let path = base.join(format!("sliplane-ssh-{name}"));
            #[cfg_attr(not(unix), allow(unused_mut))]
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            match builder.create(&path) {
                Ok(()) => return Ok(PrivateDir(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(Error::other(format!(
                        "Could not create a private directory in {}: {e}",
                        base.display()
                    )));
                }
            }
        }
        Err(Error::other("Could not find an unused name for a private directory."))
    }

    fn remove(self) -> std::io::Result<()> {
        fs::remove_dir_all(&self.0)
    }
}

/// Makes the key pair and returns the public half. The private half is never read.
fn generate_key(keygen: &Path, identity: &Path) -> Result<String> {
    let out = Command::new(keygen)
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "sliplane-cli", "-f"])
        .arg(identity)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| spawn_error(keygen, e))?;
    if !out.status.success() {
        return Err(Error::other("ssh-keygen could not create the temporary key.")
            .detail(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    let mut public = identity.as_os_str().to_owned();
    public.push(".pub");
    let text = fs::read_to_string(&public)
        .map_err(|e| Error::other(format!("Could not read the public key {}: {e}", Path::new(&public).display())))?;
    let key = text.trim();
    if !key.starts_with("ssh-ed25519 ") {
        return Err(Error::other("ssh-keygen wrote something other than an ed25519 public key."));
    }
    Ok(key.to_string())
}

// ---------------------------------------------------------------------------------------------
// Where to connect

#[derive(Debug, PartialEq)]
struct Connection {
    host: String,
    port: u16,
    user: String,
}

impl Connection {
    /// The attach response, checked: it goes into ssh's command line, and a host that begins with
    /// `-` or a user with a space must not be able to turn into an option.
    fn from_api(v: &Value) -> Result<Connection> {
        let host = v.get("host").and_then(Value::as_str).unwrap_or("");
        let user = v.get("username").and_then(Value::as_str).unwrap_or("");
        let port = v.get("port").and_then(Value::as_u64).and_then(|p| u16::try_from(p).ok()).unwrap_or(0);
        let host_ok = host.parse::<IpAddr>().is_ok()
            || (!host.is_empty()
                && !host.starts_with('-')
                && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'));
        let user_ok =
            !user.is_empty() && !user.starts_with('-') && !user.chars().any(|c| c.is_whitespace() || c.is_control());
        if port == 0 || !host_ok || !user_ok {
            return Err(Error::other("The API returned SSH connection details that cannot be used.")
                .detail(format!("host {host:?}, port {port}, username {user:?}")));
        }
        Ok(Connection { host: host.into(), port, user: user.into() })
    }

    fn describe(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }
}

// ---------------------------------------------------------------------------------------------
// The ssh command lines

/// Options every ssh call shares. `-F none` ignores the user's and the system's ssh config, so a
/// `ProxyCommand` or `Host *` rule cannot change what this does; the identity is the one key we
/// made, with no agent in play; nothing may prompt.
fn base_args(c: &Connection, identity: &Path, known_hosts: &Path) -> Vec<OsString> {
    let mut a: Vec<OsString> = vec!["-F".into(), "none".into(), "-i".into(), identity.into()];
    for option in [
        "IdentitiesOnly=yes",
        "IdentityAgent=none",
        "BatchMode=yes",
        "PreferredAuthentications=publickey",
        "StrictHostKeyChecking=accept-new",
        "ConnectTimeout=10",
        "ConnectionAttempts=1",
    ] {
        a.extend(["-o".into(), option.into()]);
    }
    a.extend(["-o".into(), format!("UserKnownHostsFile={}", option_path(known_hosts)).into()]);
    a.extend(["-p".into(), c.port.to_string().into(), "-l".into(), c.user.clone().into()]);
    a
}

/// A path as the value of an `-o` option. ssh reads it like a line of ssh_config: spaces split
/// it, backslashes escape, `%` starts a token. Forward slashes work on Windows too.
fn option_path(p: &Path) -> String {
    let mut s = p.to_string_lossy().into_owned();
    if cfg!(windows) {
        s = s.replace('\\', "/");
    }
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%"))
}

/// Connects, authenticates, runs `exit 0`. There is no authenticate-only mode without a
/// ControlMaster, and a real command is the surest sign that the service accepts the key.
fn probe_args(base: &[OsString], c: &Connection) -> Vec<OsString> {
    let mut a = base.to_vec();
    a.extend(["-T".into(), "--".into(), c.host.clone().into(), "exit".into(), "0".into()]);
    a
}

fn shell_args(base: &[OsString], c: &Connection, tty: bool, command: &[String]) -> Vec<OsString> {
    let mut a = base.to_vec();
    a.extend([if tty { "-t" } else { "-T" }.into(), "--".into(), c.host.clone().into()]);
    if let Some(command) = remote_command(command) {
        a.push(command.into());
    }
    a
}

fn tunnel_args(base: &[OsString], c: &Connection, local_port: u16, remote_port: u16) -> Vec<OsString> {
    let mut a = base.to_vec();
    for option in ["-N", "-T", "-o", "ExitOnForwardFailure=yes", "-o", "ServerAliveInterval=30", "-L"] {
        a.push(option.into());
    }
    a.push(format!("127.0.0.1:{local_port}:localhost:{remote_port}").into());
    a.extend(["--".into(), c.host.clone().into()]);
    a
}

/// What the remote shell is asked to run. ssh joins its trailing arguments with spaces and hands
/// the result to the remote shell, so `-- sh -c 'a; b'` would arrive as `sh -c a; b`. Several
/// words are therefore quoted one by one (they run exactly as given); a single word is passed
/// through untouched, as the command line `ssh host 'ls | head'` would be.
fn remote_command(args: &[String]) -> Option<String> {
    match args {
        [] => None,
        [one] => Some(one.clone()),
        many => Some(many.iter().map(|a| sh_quote(a)).collect::<Vec<_>>().join(" ")),
    }
}

fn sh_quote(arg: &str) -> String {
    let plain = !arg.is_empty() && arg.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-./:=@%+,".contains(&b));
    if plain { arg.to_string() } else { format!("'{}'", arg.replace('\'', r"'\''")) }
}

// ---------------------------------------------------------------------------------------------
// Waiting for sshd

/// A new key takes a few seconds to reach the service's sshd, and until it does the answer is
/// "Permission denied (publickey)". That is the only failure worth waiting out; anything else
/// (refused, timed out, a changed host key) will not fix itself.
fn wait_for_access(tools: &Tools, c: &Connection, base: &[OsString], known_hosts: &Path, log: &Path) -> Result<()> {
    let interval = std::env::var("SLIPLANE_SSH_RETRY_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_RETRY);
    let mut last = String::new();
    for attempt in 1..=ATTEMPTS {
        if attempt > 1 {
            say(format!("note: waiting for the service to accept the new key (attempt {attempt} of {ATTEMPTS})"));
            nap(interval)?;
        }
        let Some(stderr) = probe(tools, &probe_args(base, c), log)? else { return Ok(()) };
        if !publickey_denied(&stderr) {
            return Err(connect_error(c, &stderr, known_hosts));
        }
        last = stderr;
    }
    Err(Error::other(format!("The service did not accept the temporary key after {ATTEMPTS} attempts."))
        .detail(format!("connection: {}\n{}", c.describe(), last.trim()))
        .fix("Repeat the command once. If it fails again, report it: the API accepted the key but the service's sshd did not."))
}

/// Runs one probe. `None` means ssh got in; `Some` is what it said instead.
fn probe(tools: &Tools, args: &[OsString], log: &Path) -> Result<Option<String>> {
    // A file, not a pipe: nothing to drain, and nothing that outlives ssh and holds a pipe open.
    let stderr = File::create(log)?;
    let mut child = Command::new(&tools.ssh)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|e| spawn_error(&tools.ssh, e))?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if interrupted() || started.elapsed() > PROBE_TIMEOUT {
            stop(&mut child);
            break None;
        }
        sleep(TICK);
    };
    check_interrupt()?;
    // ssh's warnings end their lines with CR LF; the CR only makes the envelope noisy.
    let said = fs::read(log).map(|b| String::from_utf8_lossy(&b).replace('\r', "").trim().to_string());
    let said = said.unwrap_or_default();
    match status {
        Some(status) if status.success() => Ok(None),
        Some(_) => Ok(Some(said)),
        None => Ok(Some(format!("ssh did not answer within {} seconds.\n{said}", PROBE_TIMEOUT.as_secs()))),
    }
}

fn publickey_denied(stderr: &str) -> bool {
    stderr.contains("Permission denied (publickey")
}

fn host_key_problem(stderr: &str) -> bool {
    stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED") || stderr.contains("Host key verification failed")
}

fn looks_like_network(stderr: &str) -> bool {
    ["Connection timed out", "Operation timed out", "Connection refused", "No route to host", "Network is unreachable"]
        .iter()
        .chain(&["Could not resolve hostname", "Connection reset", "ssh did not answer"])
        .any(|m| stderr.contains(m))
}

fn connect_error(c: &Connection, stderr: &str, known_hosts: &Path) -> Error {
    let said = stderr.trim();
    if host_key_problem(said) {
        // known_hosts names port 22 by the bare host and every other port as [host]:port.
        let entry = if c.port == 22 { c.host.clone() } else { format!("[{}]:{}", c.host, c.port) };
        return Error::other("The service's SSH host key is not the one remembered from an earlier connection.")
            .detail(format!("connection: {}\n{said}", c.describe()))
            .fix(format!(
                "If the server was rebuilt and the change is expected, forget the old key and repeat the command: \
                 ssh-keygen -R \"{entry}\" -f \"{}\"",
                known_hosts.display()
            ));
    }
    let network = looks_like_network(said);
    let e = Error::new(
        if network { ErrorCode::Network } else { ErrorCode::Error },
        "ssh could not connect to the service.",
    )
    .detail(format!("connection: {}\n{said}", c.describe()));
    if network { e.fix("Retry once, then stop.") } else { e }
}

/// Sleeps, but wakes up for an interrupt.
fn nap(duration: Duration) -> Result<()> {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        check_interrupt()?;
        sleep(TICK.min(until.saturating_duration_since(Instant::now())));
    }
    check_interrupt()
}

// ---------------------------------------------------------------------------------------------
// The session itself

fn run_shell(tools: &Tools, c: &Connection, base: &[OsString], command: &[String]) -> Result<()> {
    // A terminal on stdin means a person is at the keyboard; anything else is a script or an
    // agent, which wants the command's output exactly as it is, with no terminal in between.
    let tty = std::io::stdin().is_terminal();
    let mut child = Command::new(&tools.ssh)
        .args(shell_args(base, c, tty, command))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| spawn_error(&tools.ssh, e))?;
    match wait_child(&mut child, None, || Ok(()))? {
        Ended::Exited(status) => session_result(status.code(), c, !command.is_empty()),
        Ended::Interrupted | Ended::TimeUp => Err(Error::other("Interrupted before the session ended.")),
    }
}

/// How a finished session is reported. Our own exit codes (1-7) are the error `code:` field, so a
/// remote status cannot be passed through as the exit code without colliding with them. It is
/// named in the message and in `detail` instead, and the exit code is 1.
fn session_result(code: Option<i32>, c: &Connection, command: bool) -> Result<()> {
    let what = if command { "command" } else { "shell" };
    match code {
        Some(0) => Ok(()),
        Some(255) => {
            Err(Error::other("ssh itself failed (exit status 255); its message is on stderr above.").detail(format!(
                "ssh exit status: 255\nconnection: {}\n255 is also what a remote {what} can exit with; if \
                 ssh printed nothing, it was the remote {what}.",
                c.describe()
            )))
        }
        Some(n) => Err(Error::other(format!("The remote {what} exited with status {n}."))
            .detail(format!("remote exit status: {n}"))),
        None => Err(Error::other("ssh was ended by a signal.")),
    }
}

fn run_tunnel(
    tools: &Tools,
    s: &ServiceRef,
    c: &Connection,
    base: &[OsString],
    local_port: u16,
    remote_port: u16,
    limit: Option<Duration>,
) -> Result<()> {
    let mut child = Command::new(&tools.ssh)
        .args(tunnel_args(base, c, local_port, remote_port))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| spawn_error(&tools.ssh, e))?;
    let started = Instant::now();
    let mut up = false;
    let ended = wait_child(&mut child, limit.map(|l| started + l), || {
        if up {
            return Ok(());
        }
        // ssh gives no signal when its forward is ready, and connecting to the port would open a
        // connection to the service. The port refusing to be bound is the same news, for free.
        if port_in_use(local_port) {
            up = true;
            return print(obj! {
                "status" => "listening",
                "local" => format!("127.0.0.1:{local_port}"),
                "remote" => format!("localhost:{remote_port} inside the service"),
                "projectId" => s.project_id,
                "serviceId" => s.service_id,
                "stop" => "End this process (Ctrl-C or SIGTERM) to close the tunnel; the temporary key is removed then.",
            });
        }
        if started.elapsed() > TUNNEL_SETUP_TIMEOUT {
            return Err(Error::other("The tunnel did not come up within 30 seconds."));
        }
        Ok(())
    })?;
    match (ended, up) {
        (Ended::Exited(status), true) if status.success() => {
            say("note: the service closed the connection; tunnel closed");
            Ok(())
        }
        (Ended::Exited(status), true) => Err(Error::other("The SSH connection ended while the tunnel was open.")
            .detail(format!("ssh exit status: {}\nconnection: {}", code_text(status), c.describe()))),
        (Ended::Exited(status), false) => Err(Error::other(
            "ssh exited before the tunnel was up; its message is on stderr above.",
        )
        .detail(format!("ssh exit status: {}\nconnection: {}", code_text(status), c.describe()))),
        (Ended::Interrupted, true) => {
            say("note: interrupted; tunnel closed");
            Ok(())
        }
        (Ended::TimeUp, true) => {
            say("note: --for elapsed; tunnel closed");
            Ok(())
        }
        (Ended::Interrupted | Ended::TimeUp, false) => Err(Error::other("Stopped before the tunnel came up.")),
    }
}

fn code_text(status: ExitStatus) -> String {
    status.code().map_or_else(|| "none (ended by a signal)".into(), |c| c.to_string())
}

fn port_in_use(port: u16) -> bool {
    matches!(TcpListener::bind(("127.0.0.1", port)), Err(e) if e.kind() == std::io::ErrorKind::AddrInUse)
}

enum Ended {
    Exited(ExitStatus),
    Interrupted,
    TimeUp,
}

/// Waits for `child`, calling `tick` every few milliseconds. An interrupt or the deadline stops
/// it first.
fn wait_child(child: &mut Child, deadline: Option<Instant>, mut tick: impl FnMut() -> Result<()>) -> Result<Ended> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // A Ctrl-C from the terminal reaches ssh and this process together, and ssh can
                // be gone before our handler has recorded it. Give it a moment before calling a
                // failed exit anything but an interrupt.
                if !status.success() {
                    sleep(Duration::from_millis(100));
                }
                return Ok(if interrupted() { Ended::Interrupted } else { Ended::Exited(status) });
            }
            Ok(None) => {}
            Err(e) => {
                stop(child);
                return Err(Error::other(format!("Could not wait for ssh: {e}")));
            }
        }
        if interrupted() {
            stop(child);
            return Ok(Ended::Interrupted);
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            stop(child);
            return Ok(Ended::TimeUp);
        }
        if let Err(e) = tick() {
            stop(child);
            return Err(e);
        }
        sleep(TICK);
    }
}

/// SIGTERM first on Unix, so ssh can put the terminal back as it found it; then the hard stop.
fn stop(child: &mut Child) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        if let Ok(pid) = i32::try_from(child.id())
            && kill(Pid::from_raw(pid), Signal::SIGTERM).is_ok()
        {
            let until = Instant::now() + Duration::from_secs(2);
            while Instant::now() < until {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return;
                }
                sleep(TICK);
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        Connection { host: "203.0.113.1".into(), port: 22222, user: "service_123456".into() }
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    fn base() -> Vec<OsString> {
        base_args(&conn(), Path::new("/tmp/d/key"), Path::new("/home/me/.config/sliplane-cli/known_hosts"))
    }

    #[test]
    fn connection_details_are_checked_before_they_reach_ssh() {
        let ok = Connection::from_api(
            &serde_json::json!({"host": "203.0.113.1", "port": 22222, "username": "service_123456"}),
        );
        assert_eq!(ok.unwrap(), conn());
        assert!(
            Connection::from_api(&serde_json::json!({"host": "ssh.example.com", "port": 22, "username": "u"})).is_ok()
        );
        assert!(Connection::from_api(&serde_json::json!({"host": "2001:db8::1", "port": 22, "username": "u"})).is_ok());
        for bad in [
            serde_json::json!({"host": "-oProxyCommand=x", "port": 22, "username": "u"}),
            serde_json::json!({"host": "a b", "port": 22, "username": "u"}),
            serde_json::json!({"host": "", "port": 22, "username": "u"}),
            serde_json::json!({"host": "h", "port": 0, "username": "u"}),
            serde_json::json!({"host": "h", "port": 70000, "username": "u"}),
            serde_json::json!({"host": "h", "port": 22, "username": "-oX"}),
            serde_json::json!({"host": "h", "port": 22, "username": "a b"}),
            serde_json::json!({"host": "h", "port": 22, "username": ""}),
            serde_json::json!({"host": "h", "port": 22}),
            serde_json::json!({}),
        ] {
            assert!(Connection::from_api(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn several_words_keep_their_boundaries_and_one_word_is_a_command_line() {
        let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(remote_command(&[]), None);
        assert_eq!(remote_command(&words(&["ls | head -1"])).unwrap(), "ls | head -1");
        assert_eq!(remote_command(&words(&["ls", "-la", "/app"])).unwrap(), "ls -la /app");
        assert_eq!(remote_command(&words(&["sh", "-c", "echo a; echo b"])).unwrap(), "sh -c 'echo a; echo b'");
        assert_eq!(remote_command(&words(&["echo", "it's", "$HOME", ""])).unwrap(), r#"echo 'it'\''s' '$HOME' ''"#);
        assert_eq!(remote_command(&words(&["cat", "/my dir/f"])).unwrap(), "cat '/my dir/f'");
    }

    #[test]
    fn option_paths_survive_ssh_config_parsing() {
        assert_eq!(option_path(Path::new("/home/me/kh")), "\"/home/me/kh\"");
        assert_eq!(option_path(Path::new("/home/my dir/k\"h%")), "\"/home/my dir/k\\\"h%%\"");
        if cfg!(windows) {
            assert_eq!(option_path(Path::new(r"C:\Users\Jo Smith\kh")), "\"C:/Users/Jo Smith/kh\"");
        }
    }

    #[test]
    fn every_call_isolates_the_identity_and_keeps_host_key_checking_on() {
        let a = strings(&base());
        let joined = a.join(" ");
        assert!(joined.starts_with("-F none -i /tmp/d/key "), "{joined}");
        for want in [
            "IdentitiesOnly=yes",
            "IdentityAgent=none",
            "BatchMode=yes",
            "PreferredAuthentications=publickey",
            "StrictHostKeyChecking=accept-new",
            "UserKnownHostsFile=\"/home/me/.config/sliplane-cli/known_hosts\"",
        ] {
            assert!(a.iter().any(|x| x == want), "missing {want}: {joined}");
        }
        assert!(!joined.contains("StrictHostKeyChecking=no"), "{joined}");
        assert!(joined.ends_with("-p 22222 -l service_123456"), "{joined}");
        // The key file is named exactly once, as the -i value.
        assert_eq!(a.iter().filter(|x| x.contains("/tmp/d/key")).count(), 1);
    }

    #[test]
    fn the_probe_runs_exit_zero_without_a_terminal() {
        let a = strings(&probe_args(&base(), &conn()));
        assert_eq!(&a[a.len() - 5..], ["-T", "--", "203.0.113.1", "exit", "0"]);
    }

    #[test]
    fn a_terminal_is_allocated_only_when_asked_for() {
        let words = vec!["ls".to_string(), "-la".to_string()];
        let tail = |tty, cmd: &[String]| {
            let a = strings(&shell_args(&base(), &conn(), tty, cmd));
            a[a.len() - cmd.len().min(1) - 3..].to_vec()
        };
        assert_eq!(tail(false, &words), ["-T", "--", "203.0.113.1", "ls -la"]);
        assert_eq!(tail(true, &words), ["-t", "--", "203.0.113.1", "ls -la"]);
        assert_eq!(tail(false, &[]), ["-T", "--", "203.0.113.1"]);
        // The command is never read as an ssh option, whatever it begins with.
        let dash = strings(&shell_args(&base(), &conn(), false, &["-oProxyCommand=x".to_string()]));
        let at = dash.iter().position(|x| x == "--").unwrap();
        assert_eq!(dash[at + 1..], ["203.0.113.1", "-oProxyCommand=x"]);
    }

    #[test]
    fn a_tunnel_listens_on_loopback_only_and_fails_loudly() {
        let a = strings(&tunnel_args(&base(), &conn(), 5433, 5432));
        let joined = a.join(" ");
        assert!(joined.contains("-N -T"), "{joined}");
        assert!(joined.contains("ExitOnForwardFailure=yes"), "{joined}");
        assert!(joined.ends_with("-L 127.0.0.1:5433:localhost:5432 -- 203.0.113.1"), "{joined}");
    }

    #[test]
    fn a_remote_status_is_reported_without_becoming_our_exit_code() {
        let c = conn();
        assert!(session_result(Some(0), &c, true).is_ok());
        let e = session_result(Some(3), &c, true).unwrap_err();
        assert_eq!(e.code, ErrorCode::Error);
        assert_eq!(e.message, "The remote command exited with status 3.");
        assert_eq!(e.detail.as_deref(), Some("remote exit status: 3"));
        assert!(session_result(Some(1), &c, false).unwrap_err().message.contains("remote shell"));
        let e = session_result(Some(255), &c, true).unwrap_err();
        assert!(e.message.contains("ssh itself failed") && e.detail.unwrap().contains("203.0.113.1"));
        assert!(session_result(None, &c, true).is_err());
    }

    #[test]
    fn ssh_messages_are_told_apart() {
        assert!(publickey_denied("svc@h: Permission denied (publickey)."));
        assert!(publickey_denied("svc@h: Permission denied (publickey,keyboard-interactive)."));
        assert!(!publickey_denied("svc@h: Permission denied (password)."));
        assert!(host_key_problem("@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@"));
        assert!(host_key_problem("Host key verification failed."));
        assert!(looks_like_network("ssh: connect to host h port 22: Connection timed out"));
        assert!(looks_like_network("ssh: Could not resolve hostname h: Name or service not known"));
        assert!(!looks_like_network("Permission denied (publickey)."));

        let known = Path::new("/c/known_hosts");
        let e = connect_error(&conn(), "Host key verification failed.", known);
        assert!(e.remediation.unwrap().contains("ssh-keygen -R \"[203.0.113.1]:22222\" -f \"/c/known_hosts\""));
        let port22 = Connection { port: 22, ..conn() };
        let e = connect_error(&port22, "Host key verification failed.", known);
        assert!(e.remediation.unwrap().contains("ssh-keygen -R \"203.0.113.1\" -f"), "port 22 is stored bare");
        assert_eq!(connect_error(&conn(), "Connection refused", known).code, ErrorCode::Network);
        assert_eq!(
            connect_error(&conn(), "kex_exchange_identification: read: Connection closed", known).code,
            ErrorCode::Error
        );
    }

    #[test]
    fn the_key_lifetime_is_bounded() {
        assert_eq!(ttl_from(None).unwrap(), Duration::from_secs(3600));
        assert_eq!(ttl_from(Some("6m")).unwrap(), Duration::from_secs(360));
        assert_eq!(ttl_from(Some("30d")).unwrap(), Duration::from_secs(30 * 86400));
        for bad in ["5m", "31d", "soon", "0s", "720h30m"] {
            assert_eq!(ttl_from(Some(bad)).unwrap_err().code, ErrorCode::InvalidInput, "{bad}");
        }
    }
}
