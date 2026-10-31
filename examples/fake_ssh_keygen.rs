//! A stand-in for `ssh-keygen`, for tests/cli.rs. Not part of the shipped program.
//!
//! `SLIPLANE_SSH_KEYGEN` points at this binary. It writes a recognisable fake key pair where `-f`
//! says, and logs its argv to `FAKE_SSH_LOG` (one JSON line). `FAKE_KEYGEN_FAIL=1` makes it fail.

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Ok(log) = std::env::var("FAKE_SSH_LOG") {
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(log).unwrap();
        writeln!(file, "{}", serde_json::json!({ "tool": "ssh-keygen", "argv": args })).unwrap();
    }
    if std::env::var("FAKE_KEYGEN_FAIL").is_ok() {
        eprintln!("fake ssh-keygen: refusing");
        std::process::exit(1);
    }
    let path = args.iter().position(|a| a == "-f").and_then(|i| args.get(i + 1)).expect("-f <path>");
    std::fs::write(path, "FAKE-PRIVATE-KEY-MATERIAL\n").unwrap();
    std::fs::write(format!("{path}.pub"), "ssh-ed25519 AAAAFAKEPUBLICKEY sliplane-cli\n").unwrap();
}
