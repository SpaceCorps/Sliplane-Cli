//! Environment variables from dotenv files: `--env-file` (plain) and `--secret-env-file` (secret).
//!
//! Syntax, deliberately the common core of dotenv and nothing more:
//!
//! - `KEY=VALUE`, one per line, with an optional `export ` in front. Blank lines and lines
//!   starting with `#` are skipped.
//! - `"double quoted"` values understand `\n \r \t \\ \"`; any other backslash stays as written.
//!   `'single quoted'` values are literal. Both may span lines (a PEM key, say). Only a comment
//!   may follow the closing quote.
//! - An unquoted value runs to the end of the line, trimmed. A ` #` inside one is refused rather
//!   than guessed at: it may be a comment or part of a password, and getting that wrong either
//!   way is silent.
//! - **No interpolation.** `$HOME`, `${X}` and `$(cmd)` are stored exactly as written.
//! - A bare `KEY` is an error. Elsewhere in this CLI (`--env KEY`) it means "empty value", and a
//!   file that silently did the same, or read the local environment, would hide a typo.
//!
//! A file may hold secrets, so no error here repeats a value or any text from a line that did
//! not parse: it names the file and the line number and says what is wrong.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;

use serde_json::Value;

use crate::cli::EnvFiles;
use crate::error::{Error, Result};
use crate::obj;

/// Nobody has a 1 MiB environment file; `/dev/zero` and friends should fail fast.
const MAX_BYTES: u64 = 1 << 20;

#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub value: String,
    /// 1-based line the entry starts on.
    pub line: usize,
}

fn fail(file: &str, line: usize, what: &str) -> Error {
    Error::invalid(format!("{file}, line {line}: {what}."))
}

fn valid_key(k: &str) -> bool {
    !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn strip_export(s: &str) -> &str {
    match s.strip_prefix("export") {
        Some(r) if r.starts_with([' ', '\t']) => r.trim_start_matches([' ', '\t']),
        _ => s,
    }
}

/// Whitespace followed by `#`: a comment in most dotenv dialects, part of the value in others.
fn has_inline_comment(v: &str) -> bool {
    v.as_bytes().windows(2).any(|w| matches!(w[0], b' ' | b'\t') && w[1] == b'#')
}

fn offset(base: &str, sub: &str) -> usize {
    sub.as_ptr() as usize - base.as_ptr() as usize
}

/// Reads a quoted value that starts at `start` (just past the opening quote). Returns the value,
/// the offset just past the closing quote and how many newlines the value contained.
fn read_quoted(text: &str, start: usize, quote: char) -> Option<(String, usize, usize)> {
    let mut out = String::new();
    let mut newlines = 0;
    let mut chars = text[start..].char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == quote {
            return Some((out, start + i + 1, newlines));
        }
        match c {
            '\\' if quote == '"' => match chars.next()?.1 {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                other => {
                    if other == '\n' {
                        newlines += 1;
                    }
                    out.push('\\');
                    out.push(other);
                }
            },
            '\n' => {
                newlines += 1;
                out.push('\n');
            }
            // A file written on Windows: keep the line break, drop the carriage return.
            '\r' if matches!(chars.peek(), Some((_, '\n'))) => {}
            c => out.push(c),
        }
    }
    None
}

/// Parses the text of one file. `file` is only used to name it in errors.
pub fn parse(text: &str, file: &str) -> Result<Vec<Entry>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if let Some(at) = text.find('\0') {
        let line = text[..at].matches('\n').count() + 1;
        return Err(fail(file, line, "contains a NUL byte, so this is not a text file"));
    }

    let mut entries = Vec::new();
    let (mut pos, mut line_no) = (0, 1);
    while pos < text.len() {
        let rest = &text[pos..];
        let eol = rest.find('\n').unwrap_or(rest.len());
        let line = rest[..eol].trim_end_matches('\r').trim_start();
        let start_line = line_no;
        let (mut used, mut lines) = (eol + 1, 1);

        if !line.is_empty() && !line.starts_with('#') {
            let Some((key, after)) = strip_export(line).split_once('=') else {
                return Err(fail(file, start_line, "has no '='. Write KEY= for an empty value, or KEY=value"));
            };
            let key = key.trim_end();
            if !valid_key(key) {
                return Err(fail(
                    file,
                    start_line,
                    "does not start with a variable name (letters, digits, '_', '.' and '-' only)",
                ));
            }
            let value_part = after.trim_start_matches([' ', '\t']);

            let value = match value_part.chars().next() {
                Some(quote @ ('"' | '\'')) => {
                    let begin = offset(rest, value_part) + 1;
                    let Some((value, end, newlines)) = read_quoted(rest, begin, quote) else {
                        return Err(fail(file, start_line, "opens a quote that is never closed"));
                    };
                    let tail_end = rest[end..].find('\n').map_or(rest.len(), |i| end + i);
                    let tail = rest[end..tail_end].trim();
                    if !tail.is_empty() && !tail.starts_with('#') {
                        return Err(fail(
                            file,
                            start_line + newlines,
                            "has text after the closing quote. Only a '# comment' may follow it",
                        ));
                    }
                    (used, lines) = (tail_end + 1, newlines + 1);
                    value
                }
                _ => {
                    if has_inline_comment(after) {
                        return Err(fail(
                            file,
                            start_line,
                            "has ' #' in an unquoted value. Quote the value to keep the '#', or move the comment to its own line",
                        ));
                    }
                    value_part.trim_end().to_string()
                }
            };
            entries.push(Entry { key: key.to_string(), value, line: start_line });
        }

        pos = (pos + used).min(text.len());
        line_no += lines;
    }
    Ok(entries)
}

/// Reads and parses a file. `flag` names the option that pointed at it, for the errors.
pub fn load(path: &str, flag: &str) -> Result<Vec<Entry>> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|f| f.take(MAX_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| Error::invalid(format!("Could not read {path} ({flag}): {e}")))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Error::invalid(format!("{path} ({flag}) is larger than 1 MiB, which is not an environment file.")));
    }
    let text = String::from_utf8(bytes).map_err(|e| {
        let line = e.as_bytes()[..e.utf8_error().valid_up_to()].iter().filter(|b| **b == b'\n').count() + 1;
        fail(path, line, "is not valid UTF-8")
    })?;
    let entries = parse(&text, path)?;
    if entries.is_empty() {
        return Err(Error::invalid(format!("{path} ({flag}) defines no variables.")));
    }
    Ok(entries)
}

/// Joins the variables from files with the ones given on the command line (`{key, value,
/// secret}` objects, in the order they were typed).
///
/// Precedence, lowest to highest: `--env-file` files, then `--secret-env-file` files (each in the
/// order given), then the command line. The flag wins, and any key that is replaced is named on
/// stderr, with where it came from but never its value.
pub fn combine(flags: Vec<Value>, files: &EnvFiles) -> Result<Vec<Value>> {
    if files.env_file.is_empty() && files.secret_env_file.is_empty() {
        return Ok(flags);
    }

    // Replaced entries become None, so positions (and the key index) stay valid.
    let mut slots: Vec<Option<(Value, String)>> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let sources = files
        .env_file
        .iter()
        .map(|p| (p, false, "--env-file"))
        .chain(files.secret_env_file.iter().map(|p| (p, true, "--secret-env-file")));
    for (path, secret, flag) in sources {
        for e in load(path, flag)? {
            let origin = format!("{path}:{}", e.line);
            let var = obj! { "key" => e.key, "value" => e.value, "secret" => secret };
            if let Some(&i) = index.get(&e.key) {
                let before = slots[i].as_ref().map_or("", |(_, o)| o.as_str());
                eprintln!("note: {} at {origin} replaces {} at {before}.", e.key, e.key);
                slots[i] = None;
            }
            index.insert(e.key, slots.len());
            slots.push(Some((var, origin)));
        }
    }

    for f in &flags {
        let Some(key) = f["key"].as_str() else { continue };
        if let Some(&i) = index.get(key)
            && let Some((_, origin)) = slots[i].take()
        {
            eprintln!("note: {key} at {origin} is replaced by the {key} given on the command line.");
        }
    }

    let mut out: Vec<Value> = slots.into_iter().flatten().map(|(v, _)| v).collect();
    out.extend(flags);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> Vec<(String, String)> {
        parse(text, "f.env").unwrap().into_iter().map(|e| (e.key, e.value)).collect()
    }

    fn kv(k: &str, v: &str) -> (String, String) {
        (k.to_string(), v.to_string())
    }

    #[test]
    fn plain_lines_comments_and_export() {
        let text = "# a comment\n\nA=1\n  B = two words  \nexport C=3\nexport\tD=4\nE=\nF=a=b\nG=#not-a-comment\n";
        assert_eq!(
            ok(text),
            vec![
                kv("A", "1"),
                kv("B", "two words"),
                kv("C", "3"),
                kv("D", "4"),
                kv("E", ""),
                kv("F", "a=b"),
                kv("G", "#not-a-comment"),
            ]
        );
        // A key that merely starts with the word export is a key.
        assert_eq!(ok("exporter=1\nexport=2"), vec![kv("exporter", "1"), kv("export", "2")]);
    }

    #[test]
    fn quotes_and_escapes() {
        assert_eq!(ok(r#"A="a b" "#), vec![kv("A", "a b")]);
        assert_eq!(ok(r#"A="x\ny\t\"q\"\\z""#), vec![kv("A", "x\ny\t\"q\"\\z")]);
        // Unknown escapes keep their backslash, so a Windows path survives.
        assert_eq!(ok(r#"A="C:\Users\me""#), vec![kv("A", r"C:\Users\me")]);
        assert_eq!(ok(r"A='no \n escapes'"), vec![kv("A", r"no \n escapes")]);
        assert_eq!(ok("A=\"it's\"\nB='say \"hi\"'"), vec![kv("A", "it's"), kv("B", "say \"hi\"")]);
        assert_eq!(ok("A=\"\"\nB=''"), vec![kv("A", ""), kv("B", "")]);
        // Only a comment may follow a closing quote.
        assert_eq!(ok("A=\"v\" # why\nB='w'\t#x"), vec![kv("A", "v"), kv("B", "w")]);
    }

    #[test]
    fn nothing_is_interpolated() {
        assert_eq!(
            ok("A=$HOME\nB=${HOME}/x\nC=$(id)\nD=\"$HOME `id`\"\nE='${X}'"),
            vec![kv("A", "$HOME"), kv("B", "${HOME}/x"), kv("C", "$(id)"), kv("D", "$HOME `id`"), kv("E", "${X}")]
        );
    }

    #[test]
    fn quoted_values_can_span_lines_and_line_numbers_follow() {
        let text = "A=\"-----BEGIN\nline two\n-----END\"\nB=1\n";
        let entries = parse(text, "f.env").unwrap();
        assert_eq!(entries[0].value, "-----BEGIN\nline two\n-----END");
        assert_eq!((entries[0].line, entries[1].line), (1, 4));
        // The error after a multi-line value names the right line.
        let err = parse("A='x\ny'\n\nbad line\n", "f.env").unwrap_err();
        assert!(err.message.contains("line 4"), "{}", err.message);
    }

    #[test]
    fn windows_line_endings_and_a_byte_order_mark() {
        assert_eq!(ok("\u{feff}A=1\r\nB=\"x\r\ny\"\r\nC=3\r\n"), vec![kv("A", "1"), kv("B", "x\ny"), kv("C", "3")]);
        assert_eq!(ok("A=1\rB=2"), vec![kv("A", "1\rB=2")], "a lone CR is not a line break");
    }

    #[test]
    fn a_bare_key_is_an_error_never_an_empty_value() {
        let err = parse("A=1\nDATABASE_URL\n", "prod.env").unwrap_err();
        assert!(err.message.contains("prod.env, line 2"), "{}", err.message);
        assert!(err.message.contains("KEY="), "{}", err.message);
        assert!(!err.message.contains("DATABASE_URL"), "the bare word may be a secret: {}", err.message);
        assert!(parse("export TOKEN\n", "f").is_err());
    }

    #[test]
    fn hostile_input_is_refused_without_echoing_it() {
        let secret = "hunter2-s3cret";
        let cases = [
            secret.to_string(),                 // a pasted token, no '='
            format!("A=\"{secret}"),            // unterminated double quote
            format!("A='{secret}\nB=1\n"),      // unterminated single quote swallows the file
            format!("A=\"{secret}\" trailing"), // text after the closing quote
            format!("A='{secret}'x"),
            format!("A={secret} # comment"), // ambiguous ' #'
            format!("A={secret}\t#c"),
            format!("\"A\"={secret}"), // quoted key
            format!("A B={secret}"),   // space in the key
            format!("=\"{secret}\""),  // empty key
            format!("A={secret}\0"),   // NUL
            format!("A=\"{secret}\\"), // dangling escape
        ];
        for text in cases {
            let e = parse(&text, "f.env").expect_err(&format!("{text:?} should be refused"));
            let shown = format!("{} {:?}", e.message, e.detail);
            assert!(!shown.contains(secret), "echoed the input for {text:?}: {shown}");
            assert!(shown.contains("f.env, line "), "{shown}");
        }
        // Short forms of the same.
        for text in [
            "A=\"x",
            "A='x\nB=1",
            "A=\"x\" y",
            "A=x # c",
            "A B=1",
            "\"A\"=1",
            "=x",
            "A=1\0",
            "A=\"x\\",
            "just a sentence",
            "A",
        ] {
            assert!(parse(text, "f.env").is_err(), "{text:?} should be refused");
        }
    }

    #[test]
    fn errors_name_the_line_of_the_problem() {
        let err = parse("A=1\n\n# c\nB=\"open\nC=2\n", "f.env").unwrap_err();
        assert!(err.message.starts_with("f.env, line 4:"), "{}", err.message);
        let err = parse("A=1\nB=2\0\n", "f.env").unwrap_err();
        assert!(err.message.contains("line 2"), "{}", err.message);
    }

    #[test]
    fn an_empty_file_parses_to_nothing() {
        assert!(parse("", "f").unwrap().is_empty());
        assert!(parse("\n\n# only comments\n", "f").unwrap().is_empty());
    }

    #[test]
    fn a_large_file_parses_quickly() {
        let text: String = (0..50_000).map(|i| format!("KEY_{i}=value-{i}\n")).collect();
        let started = std::time::Instant::now();
        assert_eq!(parse(&text, "big.env").unwrap().len(), 50_000);
        assert!(started.elapsed().as_secs() < 5);
    }

    fn files(plain: &[&str], secret: &[&str]) -> EnvFiles {
        EnvFiles {
            env_file: plain.iter().map(|s| s.to_string()).collect(),
            secret_env_file: secret.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn temp(name: &str, text: &[u8]) -> String {
        let dir = std::env::temp_dir().join(format!("sliplane-envfile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn files_layer_and_the_command_line_wins() {
        let a = temp("a.env", b"A=file-a\nB=file-a\nSHARED=plain\n");
        let b = temp("b.env", b"B=file-b\nSHARED=secret\nC=c\n");
        let flag = vec![obj! { "key" => "A", "value" => "flag", "secret" => false }];
        let out = combine(flag, &files(&[&a], &[&b])).unwrap();
        let shown: Vec<_> = out
            .iter()
            .map(|v| format!("{}={}:{}", v["key"].as_str().unwrap(), v["value"].as_str().unwrap(), v["secret"]))
            .collect();
        // File order, then the flag; A is the flag's, B and SHARED come from the later file.
        assert_eq!(shown, ["B=file-b:true", "SHARED=secret:true", "C=c:true", "A=flag:false"]);
    }

    #[test]
    fn no_files_leaves_the_flags_alone() {
        let flags = vec![obj! { "key" => "A", "value" => "1", "secret" => false }];
        assert_eq!(combine(flags.clone(), &files(&[], &[])).unwrap(), flags);
    }

    #[test]
    fn unreadable_empty_binary_and_oversized_files_are_refused() {
        let missing = std::env::temp_dir().join("sliplane-no-such-file.env");
        let err = load(missing.to_str().unwrap(), "--env-file").unwrap_err();
        assert!(err.message.contains("Could not read"), "{}", err.message);

        let empty = temp("empty.env", b"# nothing\n");
        assert!(load(&empty, "--env-file").unwrap_err().message.contains("defines no variables"));

        let binary = temp("binary.env", b"A=1\nB=\xff\xfe\n");
        let err = load(&binary, "--secret-env-file").unwrap_err();
        assert!(err.message.contains("line 2") && err.message.contains("UTF-8"), "{}", err.message);

        let huge = temp("huge.env", &vec![b'#'; (MAX_BYTES + 1) as usize]);
        assert!(load(&huge, "--env-file").unwrap_err().message.contains("larger than 1 MiB"));
    }
}
