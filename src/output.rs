//! Renders command output as YAML by default, or raw JSON with --json.
//!
//! YAML reads well but is awkward to script against: pulling an id out of a listing means
//! line-pairing or a regex, and both break quietly when the shape changes. --json exists so
//! callers can pipe into jq instead.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Map, Value};

use crate::error::Error;

/// Seeded from the raw args before parsing, because the error envelope can be rendered before
/// any command has run.
static USE_JSON: AtomicBool = AtomicBool::new(false);

pub fn set_json(on: bool) {
    USE_JSON.store(on, Ordering::Relaxed);
}

pub fn json() -> bool {
    USE_JSON.load(Ordering::Relaxed)
}

pub fn render(value: &Value) -> String {
    if json() {
        // Non-ASCII stays readable; this goes to a terminal or jq, never into HTML.
        serde_json::to_string_pretty(value).unwrap_or_default()
    } else {
        yaml(value)
    }
}

pub fn yaml(value: &Value) -> String {
    let mut s = serde_norway::to_string(value).unwrap_or_default();
    while s.ends_with('\n') {
        s.pop();
    }
    s
}

pub fn write(value: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", render(value));
}

/// The error envelope on stderr. Returns the exit code, which is the same thing as `code:`.
pub fn write_error(e: &Error) -> i32 {
    let mut payload = Map::new();
    payload.insert("error".into(), Value::String(e.message.clone()));
    payload.insert("code".into(), Value::String(e.code.name().into()));
    if let Some(d) = e.detail.as_deref().filter(|d| !d.trim().is_empty()) {
        payload.insert("detail".into(), Value::String(d.into()));
    }
    if let Some(r) = e.remediation.as_deref().filter(|r| !r.trim().is_empty()) {
        payload.insert("remediation".into(), Value::String(r.into()));
    }
    let _ = writeln!(std::io::stderr().lock(), "{}", render(&Value::Object(payload)));
    e.code as i32
}

/// Builds a JSON object from `key => value` pairs, keeping their order.
#[macro_export]
macro_rules! obj {
    ($($k:expr => $v:expr),* $(,)?) => {{
        #[allow(unused_mut)]
        let mut m = ::serde_json::Map::new();
        $( m.insert(($k).into(), ::serde_json::json!($v)); )*
        ::serde_json::Value::Object(m)
    }};
}
