//! `services logs --follow`. The API has no streaming endpoint, so this polls, and the work is
//! in not printing a line twice while not swallowing a line that really did repeat.
//!
//! `from` has one-second resolution, so each poll starts one second before the newest line seen
//! and the overlap is filtered out here: a line is new only if it appears more often in this
//! response than it did the last time it was counted.

use std::collections::HashMap;
use std::io::Write;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::client::{Client, query};
use crate::error::{Error, Result};
use crate::{output, timeparse};

/// The API returns at most this many lines per request.
pub const LINE_LIMIT: usize = 500;

pub struct Entry {
    at: (i64, u32),
    created_at: String,
    message: String,
}

impl Entry {
    fn from(v: &Value) -> Option<Entry> {
        let created_at = v.get("createdAt")?.as_str()?.to_string();
        let message = v.get("message").and_then(Value::as_str).unwrap_or("").to_string();
        Some(Entry { at: timeparse::rfc3339(&created_at)?, created_at, message })
    }

    fn json(&self) -> Value {
        json!({ "createdAt": self.created_at, "message": self.message })
    }
}

#[derive(Default)]
pub struct Follower {
    /// How often each (time, message) has been printed so far.
    seen: HashMap<(i64, u32, String), usize>,
    latest: Option<i64>,
}

impl Follower {
    /// The lines of `batch` not printed before, oldest first.
    pub fn fresh(&mut self, mut batch: Vec<Entry>) -> Vec<Entry> {
        batch.sort_by_key(|e| e.at);
        let mut counted: HashMap<(i64, u32, String), usize> = HashMap::new();
        let mut out = Vec::new();
        for e in batch {
            let key = (e.at.0, e.at.1, e.message.clone());
            let n = counted.entry(key.clone()).or_insert(0);
            *n += 1;
            if *n > self.seen.get(&key).copied().unwrap_or(0) {
                out.push(e);
            }
        }
        for (key, n) in counted {
            self.latest = Some(self.latest.map_or(key.0, |l| l.max(key.0)));
            let seen = self.seen.entry(key).or_insert(0);
            *seen = (*seen).max(n);
        }
        // Lines older than the next request's start can never come back.
        if let Some(from) = self.next_from() {
            self.seen.retain(|k, _| k.0 >= from);
        }
        out
    }

    /// Where the next poll starts, in Unix seconds.
    pub fn next_from(&self) -> Option<i64> {
        self.latest.map(|l| l - 1)
    }
}

fn fetch(client: &Client, path: &str, from: Option<i64>) -> Result<Vec<Entry>> {
    let q = query(&[("from", from.map(|f| f.to_string()))]);
    let body = client.get(&format!("{path}{q}"))?;
    let lines = body.as_array().ok_or_else(|| Error::other("The logs endpoint did not return a list."))?;
    if lines.len() >= LINE_LIMIT {
        eprintln!(
            "warning: the API returned its limit of {LINE_LIMIT} lines; older lines in this window were not shown."
        );
    }
    Ok(lines.iter().filter_map(Entry::from).collect())
}

/// Container output is untrusted text: an escape sequence in it could rewrite the terminal.
fn clean(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

fn emit(e: &Entry) {
    let mut out = std::io::stdout().lock();
    let _ = if output::json() {
        writeln!(out, "{}", e.json())
    } else {
        writeln!(out, "{}  {}", e.created_at, clean(&e.message))
    };
    let _ = out.flush();
}

/// Prints new log lines until `limit` passes (or forever when it is `None`, until interrupted).
pub fn follow(
    client: &Client,
    path: &str,
    mut from: Option<i64>,
    interval: Duration,
    limit: Option<Duration>,
) -> Result<()> {
    let started = Instant::now();
    let mut follower = Follower::default();
    loop {
        for e in follower.fresh(fetch(client, path, from)?) {
            emit(&e);
        }
        from = follower.next_from().or(from);

        let pause = match limit {
            Some(l) => {
                let remaining = l.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return Ok(());
                }
                interval.min(remaining)
            }
            None => interval,
        };
        std::thread::sleep(pause);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(at: &str, message: &str) -> Entry {
        Entry::from(&json!({ "createdAt": at, "message": message })).unwrap()
    }

    fn messages(v: &[Entry]) -> Vec<&str> {
        v.iter().map(|e| e.message.as_str()).collect()
    }

    #[test]
    fn lines_come_out_oldest_first() {
        let mut f = Follower::default();
        let out = f.fresh(vec![
            entry("2026-10-31T12:00:03Z", "c"),
            entry("2026-10-31T12:00:01Z", "a"),
            entry("2026-10-31T12:00:02Z", "b"),
        ]);
        assert_eq!(messages(&out), ["a", "b", "c"]);
    }

    #[test]
    fn the_overlap_between_polls_is_not_printed_twice() {
        let mut f = Follower::default();
        f.fresh(vec![entry("2026-10-31T12:00:01Z", "a"), entry("2026-10-31T12:00:02Z", "b")]);
        // The next poll starts one second back, so it returns "b" again along with the new line.
        let out = f.fresh(vec![entry("2026-10-31T12:00:02Z", "b"), entry("2026-10-31T12:00:03Z", "c")]);
        assert_eq!(messages(&out), ["c"]);
    }

    #[test]
    fn a_line_that_really_repeated_is_still_printed() {
        let mut f = Follower::default();
        let first = f.fresh(vec![entry("2026-10-31T12:00:01Z", "ping")]);
        assert_eq!(first.len(), 1);
        // Same second, same text, but now twice: one of them is new.
        let second = f.fresh(vec![entry("2026-10-31T12:00:01Z", "ping"), entry("2026-10-31T12:00:01Z", "ping")]);
        assert_eq!(second.len(), 1);
        let third = f.fresh(vec![entry("2026-10-31T12:00:01Z", "ping"), entry("2026-10-31T12:00:01Z", "ping")]);
        assert!(third.is_empty());
    }

    #[test]
    fn identical_text_at_different_times_is_not_a_repeat_of_the_same_line() {
        let mut f = Follower::default();
        f.fresh(vec![entry("2026-10-31T12:00:01Z", "tick")]);
        let out = f.fresh(vec![entry("2026-10-31T12:00:01Z", "tick"), entry("2026-10-31T12:00:02Z", "tick")]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].created_at, "2026-10-31T12:00:02Z");
    }

    #[test]
    fn the_cursor_starts_one_second_before_the_newest_line() {
        let mut f = Follower::default();
        assert_eq!(f.next_from(), None);
        f.fresh(vec![entry("2000-03-01T00:00:05Z", "x")]);
        assert_eq!(f.next_from(), Some(951_868_800 + 4));
        // An empty poll leaves it where it was.
        f.fresh(vec![]);
        assert_eq!(f.next_from(), Some(951_868_800 + 4));
    }

    #[test]
    fn memory_of_old_lines_is_dropped() {
        let mut f = Follower::default();
        f.fresh(vec![entry("2026-10-31T12:00:01Z", "old")]);
        f.fresh(vec![entry("2026-10-31T12:00:09Z", "new")]);
        assert_eq!(f.seen.len(), 1, "only lines at or after the next start are worth remembering");
    }

    #[test]
    fn control_characters_cannot_reach_the_terminal() {
        assert_eq!(clean("ok \u{1b}[2J\u{7}gone\r\n"), "ok  [2J gone  ");
    }

    #[test]
    fn malformed_entries_are_skipped_not_fatal() {
        assert!(Entry::from(&json!({ "createdAt": "yesterday", "message": "x" })).is_none());
        assert!(Entry::from(&json!({ "message": "x" })).is_none());
    }
}
