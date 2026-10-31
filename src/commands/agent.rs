//! `agent init`: a short, marked section in AGENTS.md (or another Markdown file) that points a
//! coding agent at `sliplane agent-readme`. It is a pointer and the three rules that matter most,
//! not a copy of the manual, so it cannot go stale: the manual is compiled into the binary.
//!
//! It needs no account and no network. Everything between the markers is ours and is replaced
//! on every run; everything outside them is left byte for byte as it was.

use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use crate::cli::Agent;
use crate::commands::print;
use crate::error::{Error, Result};
use crate::{obj, output};

const BEGIN: &str = "<!-- sliplane:begin -->";
const END: &str = "<!-- sliplane:end -->";

/// Quotes the rules in `readme.rs` (`RULES`) that matter most; keep the two in step.
const SECTION: &str = r#"<!-- sliplane:begin -->
<!-- Written by `sliplane agent init`. Edits between these markers are replaced when it runs again. -->
## Sliplane

This project runs on Sliplane, managed with the `sliplane` CLI. Before your first call, run
`sliplane agent-readme` and follow it: it is the manual, and it always matches the installed version.

- Always pass `--account <name>`. There is no default account and no environment variable. If you
  do not know which account to use, run `sliplane accounts list` and ask the human.
- Never ask the human for an API key and never put one in a command. If no account is configured,
  tell them to run `sliplane login <name>` themselves.
- Use `--json` when you are going to parse the output.
<!-- sliplane:end -->"#;

pub fn run(c: Agent) -> Result<()> {
    match c {
        Agent::Init { file, print: only } => init(&file, only),
    }
}

fn init(file: &str, print_only: bool) -> Result<()> {
    if print_only {
        if output::json() {
            return print(obj! { "path" => file, "content" => SECTION });
        }
        println!("{SECTION}");
        return Ok(());
    }

    let path = target(file)?;
    let existing = read(&path)?;
    let updated = merge(existing.as_deref().unwrap_or(""))?;
    let status = match &existing {
        None => "created",
        Some(old) if *old == updated => "unchanged",
        Some(_) => "updated",
    };
    if status != "unchanged" {
        write(&path, &updated, existing.is_some())?;
    }
    print(obj! { "status" => status, "path" => file })
}

/// The file's text with our section added, or replaced in place when it is already there.
fn merge(existing: &str) -> Result<String> {
    let nl = if existing.contains("\r\n") { "\r\n" } else { "\n" };
    let section = SECTION.replace('\n', nl);

    let (begins, ends) = (existing.matches(BEGIN).count(), existing.matches(END).count());
    match (begins, ends) {
        (0, 0) => {
            if existing.is_empty() {
                return Ok(format!("{section}{nl}"));
            }
            let blank = format!("{nl}{nl}");
            let gap = if existing.ends_with(&blank) {
                ""
            } else if existing.ends_with(nl) {
                nl
            } else {
                &blank
            };
            Ok(format!("{existing}{gap}{section}{nl}"))
        }
        (1, 1) => {
            let (b, e) = (existing.find(BEGIN).unwrap_or(0), existing.find(END).unwrap_or(0));
            if e < b {
                return Err(markers_error());
            }
            Ok(format!("{}{section}{}", &existing[..b], &existing[e + END.len()..]))
        }
        _ => Err(markers_error()),
    }
}

fn markers_error() -> Error {
    Error::invalid(format!(
        "The Sliplane markers in that file are incomplete, duplicated or out of order, so nothing was written. \
         There should be exactly one {BEGIN} followed by one {END}."
    ))
    .fix("Fix or delete the markers by hand, then run `sliplane agent init` again.")
}

/// Where to write: a regular Markdown file inside the current directory, never through `..`
/// and never through a symbolic link that leaves it.
fn target(file: &str) -> Result<PathBuf> {
    let given = Path::new(file);
    if file.trim().is_empty() {
        return Err(Error::invalid("--file is empty."));
    }
    if given.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(Error::invalid(format!(
            "--file '{file}' uses '..'. `agent init` only writes inside the current directory."
        )));
    }

    let cwd = std::env::current_dir().and_then(|d| d.canonicalize())?;
    let absolute = if given.is_absolute() { given.to_path_buf() } else { cwd.join(given) };
    let name = absolute
        .file_name()
        .ok_or_else(|| Error::invalid(format!("--file '{file}' does not name a file.")))?
        .to_owned();
    if !name.to_string_lossy().to_lowercase().ends_with(".md") {
        return Err(Error::invalid(format!("--file '{file}' is not a Markdown file (.md), such as AGENTS.md.")));
    }

    // The directory is resolved, so a link to somewhere else cannot lead out of the tree.
    let missing = || Error::invalid(format!("The directory of --file '{file}' does not exist."));
    let dir = absolute.parent().ok_or_else(missing)?.canonicalize().map_err(|_| missing())?;
    if !dir.starts_with(&cwd) {
        return Err(Error::invalid(format!(
            "--file '{file}' is outside the current directory. `agent init` only writes inside it."
        )));
    }
    let path = dir.join(name);

    match fs::symlink_metadata(&path) {
        Ok(m) if !m.file_type().is_file() => {
            Err(Error::invalid(format!("--file '{file}' is a directory or a symbolic link, not a regular file.")))
        }
        _ => Ok(path),
    }
}

fn read(path: &Path) -> Result<Option<String>> {
    match fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| Error::invalid(format!("{} is not valid UTF-8, so it was left alone.", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::invalid(format!("Could not read {}: {e}", path.display()))),
    }
}

/// A new file is created exclusively; an existing one is replaced through a temporary file in the
/// same directory, so a crash cannot leave half of someone's instructions behind.
fn write(path: &Path, text: &str, replace: bool) -> Result<()> {
    if !replace {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).open(path)?;
        return Ok(f.write_all(text.as_bytes())?);
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".sliplane-{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let result = fs::write(&tmp, text)
        .and_then(|()| fs::set_permissions(&tmp, fs::metadata(path)?.permissions()))
        .and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    Ok(result?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_section_is_delimited_short_and_quotes_the_rules() {
        assert!(SECTION.starts_with(BEGIN) && SECTION.ends_with(END));
        assert_eq!(SECTION.matches(BEGIN).count() + SECTION.matches(END).count(), 2);
        assert!(SECTION.lines().count() <= 16, "this is a pointer, not a copy of the manual");
        for needed in ["sliplane agent-readme", "--account", "accounts list", "API key", "sliplane login", "--json"] {
            assert!(SECTION.contains(needed), "{needed}");
        }
        assert!(!SECTION.contains('\r'));
    }

    #[test]
    fn a_missing_or_empty_file_gets_just_the_section() {
        assert_eq!(merge("").unwrap(), format!("{SECTION}\n"));
    }

    #[test]
    fn the_section_is_appended_after_existing_text() {
        assert_eq!(merge("# Title\n").unwrap(), format!("# Title\n\n{SECTION}\n"));
        assert_eq!(merge("# Title").unwrap(), format!("# Title\n\n{SECTION}\n"));
        assert_eq!(merge("# Title\n\n").unwrap(), format!("# Title\n\n{SECTION}\n"));
    }

    #[test]
    fn a_rerun_replaces_the_section_in_place_and_touches_nothing_else() {
        let before = "# Mine\r\nkeep   this \t\r\n\r\n";
        let after = "\r\n## Later\r\nalso mine, no trailing newline";
        let stale = format!("{BEGIN}\r\nold text\r\n{END}");
        let file = format!("{before}{stale}{after}");

        let merged = merge(&file).unwrap();
        assert!(merged.starts_with(before) && merged.ends_with(after));
        assert!(merged.contains("sliplane agent-readme") && !merged.contains("old text"));
        assert!(!merged.replace("\r\n", "").contains('\n'), "a CRLF file stays CRLF");
        assert_eq!(merge(&merged).unwrap(), merged, "idempotent");
    }

    #[test]
    fn the_second_run_changes_nothing() {
        let once = merge("# Title\n").unwrap();
        assert_eq!(merge(&once).unwrap(), once);
    }

    #[test]
    fn broken_markers_are_refused_rather_than_guessed_at() {
        for bad in [
            format!("x\n{BEGIN}\nno end\n"),
            format!("x\n{END}\nno begin\n"),
            format!("{END}\n{BEGIN}\n"),
            format!("{BEGIN}\n{END}\n{BEGIN}\n{END}\n"),
            format!("{BEGIN}\n{BEGIN}\n{END}\n"),
        ] {
            let err = merge(&bad).unwrap_err();
            assert!(err.message.contains("nothing was written"), "{bad:?}");
        }
    }
}
