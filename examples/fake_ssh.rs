//! A stand-in for `ssh`, for tests/cli.rs. Not part of the shipped program.
//!
//! The tests point `SLIPLANE_SSH` at this binary, so the same code path runs on Linux, macOS and
//! Windows without a shell script. It records how it was called and behaves as the environment
//! says:
//!
//! - `FAKE_SSH_LOG`: file that gets one JSON line per call (argv, whether the `-i` file existed,
//!   whether stdin was a terminal, what stdin held when `FAKE_SSH_ECHO_STDIN` is set).
//! - `FAKE_SSH_DENY_FIRST=n`: the first n probes answer "Permission denied (publickey)", as sshd
//!   does until it has the new key.
//! - `FAKE_SSH_PROBE_SAYS=text`: every probe fails with this message and status 255.
//! - `FAKE_SSH_EXIT=n`: the status of the session (default 0). `FAKE_SSH_ECHO_STDIN=1` copies
//!   stdin to stdout. `FAKE_SSH_HANG=1` writes `<log>.started` and then waits for a signal.
//! - With `-L`, it listens on the local side of the forward, as ssh would, for
//!   `FAKE_SSH_TUNNEL_SECONDS` (default 60); `FAKE_SSH_TUNNEL_EXIT=n` fails instead, immediately.

use std::io::{IsTerminal, Read, Write};
use std::net::TcpListener;
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env = |name: &str| std::env::var(name).ok();
    let number = |name: &str| env(name).and_then(|v| v.parse::<i32>().ok());

    let after = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let identity = after("-i").unwrap_or_default();
    let probe = args.ends_with(&["exit".to_string(), "0".to_string()]);
    let forward = after("-L");

    let mut echoed = String::new();
    if env("FAKE_SSH_ECHO_STDIN").is_some() && !probe {
        std::io::stdin().read_to_string(&mut echoed).unwrap();
    }

    // The probe counter lives next to the log; probes run one after another.
    let mut probe_number = 0;
    if let (Some(log), true) = (env("FAKE_SSH_LOG"), probe) {
        let counter = format!("{log}.probes");
        probe_number = std::fs::read_to_string(&counter).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0) + 1;
        std::fs::write(counter, probe_number.to_string()).unwrap();
    }

    if let Some(log) = env("FAKE_SSH_LOG") {
        let line = serde_json::json!({
            "tool": "ssh",
            "argv": args,
            "probe": probe,
            "identityExisted": std::path::Path::new(&identity).is_file(),
            "stdinIsTerminal": std::io::stdin().is_terminal(),
            "stdin": echoed,
        });
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(log).unwrap();
        writeln!(file, "{line}").unwrap();
    }

    if probe {
        if let Some(says) = env("FAKE_SSH_PROBE_SAYS") {
            eprintln!("{says}");
            std::process::exit(255);
        }
        if probe_number <= number("FAKE_SSH_DENY_FIRST").unwrap_or(0) {
            eprintln!("service_1@127.0.0.1: Permission denied (publickey).");
            std::process::exit(255);
        }
        return;
    }

    if let Some(spec) = forward {
        if let Some(code) = number("FAKE_SSH_TUNNEL_EXIT") {
            eprintln!("fake ssh: tunnel failed");
            std::process::exit(code);
        }
        // `127.0.0.1:<local>:localhost:<remote>`
        let local = spec.split(':').nth(1).unwrap();
        let listener = TcpListener::bind(format!("127.0.0.1:{local}")).unwrap();
        let seconds = number("FAKE_SSH_TUNNEL_SECONDS").unwrap_or(60);
        std::thread::sleep(Duration::from_secs(seconds as u64));
        drop(listener);
        return;
    }

    if env("FAKE_SSH_HANG").is_some() {
        if let Some(log) = env("FAKE_SSH_LOG") {
            std::fs::write(format!("{log}.started"), "hanging").unwrap();
        }
        std::thread::sleep(Duration::from_secs(60));
    }

    print!("{echoed}");
    println!("fake-ssh stdout");
    eprintln!("fake-ssh stderr");
    std::process::exit(number("FAKE_SSH_EXIT").unwrap_or(0));
}
