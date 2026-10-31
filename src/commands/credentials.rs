//! `credentials create`: where the registry token comes from. A token on the command line is in
//! the process list and the shell history (and in an agent's transcript), so `--token-stdin` is
//! the form to use. `--token` keeps working, with a warning.

use std::io::{IsTerminal, Read};

use crate::error::{Error, Result};

/// Registry tokens are short; a service-account JSON key is a few KiB. Anything bigger is a
/// mistake (or a hostile pipe), not a token.
const MAX_TOKEN_BYTES: u64 = 64 * 1024;

const STDIN_EXAMPLE: &str = "printf %s \"$REGISTRY_TOKEN\" | sliplane credentials create --name <name> \
                             --type <ghcr|dockerhub|dhi|generic> --username <user> --token-stdin -a <account>";

/// The token from `--token` or from stdin. Never prompts and never waits on a terminal.
pub fn token(argv: Option<String>, from_stdin: bool) -> Result<String> {
    match (argv, from_stdin) {
        (Some(_), true) => Err(Error::invalid("Pass --token or --token-stdin, not both.")),
        (Some(token), false) => {
            eprintln!(
                "warning: --token puts the registry token in the process list and the shell history. \
                 Pipe it in instead: {STDIN_EXAMPLE}"
            );
            Ok(token)
        }
        (None, true) => read_stdin(),
        (None, false) => Err(Error::invalid("No registry token given.").fix(STDIN_EXAMPLE)),
    }
}

fn read_stdin() -> Result<String> {
    let stdin = std::io::stdin();
    // An agent that forgot to pipe anything would otherwise wait here forever.
    if stdin.is_terminal() {
        return Err(Error::invalid("--token-stdin was given but stdin is a terminal, so there is no token to read.")
            .fix(STDIN_EXAMPLE));
    }
    let mut text = String::new();
    stdin
        .lock()
        .take(MAX_TOKEN_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| Error::invalid(format!("Could not read the token from stdin: {e}")))?;
    if text.len() as u64 > MAX_TOKEN_BYTES {
        return Err(Error::invalid("The data on stdin is larger than 64 KiB, which is not a registry token."));
    }
    // Only the line break `echo` or a file adds; the token itself may legitimately end in spaces.
    let token = text.trim_end_matches(['\r', '\n']);
    if token.is_empty() {
        return Err(Error::invalid("--token-stdin was given but stdin was empty.").fix(STDIN_EXAMPLE));
    }
    Ok(token.to_string())
}

/// An API error should never carry the token back into a transcript, whatever the server echoes.
pub fn redact(mut e: Error, token: &str) -> Error {
    if token.is_empty() {
        return e;
    }
    e.message = e.message.replace(token, "[redacted]");
    e.detail = e.detail.map(|d| d.replace(token, "[redacted]"));
    e.remediation = e.remediation.map(|r| r.replace(token, "[redacted]"));
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_one_source() {
        assert!(token(Some("t".into()), true).is_err());
        let err = token(None, false).unwrap_err();
        assert!(err.remediation.unwrap().contains("--token-stdin"));
        assert_eq!(token(Some("t".into()), false).unwrap(), "t");
    }

    #[test]
    fn redaction_covers_every_field_and_ignores_an_empty_token() {
        let e = Error::invalid("bad token s3cret").detail("body: s3cret").fix("retry with s3cret");
        let e = redact(e, "s3cret");
        assert_eq!(e.message, "bad token [redacted]");
        assert_eq!(e.detail.as_deref(), Some("body: [redacted]"));
        assert_eq!(e.remediation.as_deref(), Some("retry with [redacted]"));
        assert_eq!(redact(Error::invalid("same"), "").message, "same");
    }
}
