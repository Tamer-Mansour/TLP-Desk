//! Where command output goes: human lines, JSON objects, and errors.
//!
//! * Human mode (default): progress and results on stdout, errors on stderr.
//! * `--json`: stdout carries only JSON, one object per line (JSON Lines), each with a
//!   `"type"` field; human progress lines move to stderr so a person still sees them.
//!   A failure is the object `{"type":"error","code":N,"kind":"...","message":"..."}`.
//! * `--quiet`: no progress lines; results and errors only.
//!
//! Text is UTF-8. On a Windows console Rust's standard library writes through
//! `WriteConsoleW`, so Arabic reaches the console intact whatever the code page; when the
//! output is piped the bytes are UTF-8. Windows Terminal shapes and orders Arabic correctly;
//! the legacy console host shows the letters unjoined.

use std::io::Write;

use serde_json::{json, Value};

use crate::exit::Exit;

/// How the user asked us to talk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Output {
    /// Emit JSON objects on stdout instead of human text.
    pub json: bool,
    /// Suppress progress lines.
    pub quiet: bool,
}

impl Output {
    /// A progress line for humans (dropped with `--quiet`; on stderr with `--json`).
    pub fn progress(&self, line: impl std::fmt::Display) {
        if self.quiet {
            return;
        }
        if self.json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }

    /// A result: `human` in text mode, `value` (one JSON line) in JSON mode. Never suppressed.
    pub fn result(&self, human: impl std::fmt::Display, value: &Value) {
        if self.json {
            print_json(value);
        } else {
            println!("{human}");
        }
    }

    /// A streamed event: a progress line in text mode, a JSON line in JSON mode.
    pub fn event(&self, human: impl std::fmt::Display, value: &Value) {
        if self.json {
            print_json(value);
        } else {
            self.progress(human);
        }
    }

    /// Reports a failure (`message` is already localized) and returns its exit code.
    pub fn error(&self, err: &anyhow::Error, message: &str) -> Exit {
        let exit = Exit::of(err);
        if self.json {
            print_json(&error_json(exit, &format!("{err:#}")));
        }
        if !self.json || !self.quiet {
            eprintln!("{message}");
        }
        exit
    }
}

/// The JSON object describing a failure.
pub fn error_json(exit: Exit, message: &str) -> Value {
    json!({
        "type": "error",
        "code": exit.code(),
        "kind": exit.name(),
        "message": message,
    })
}

fn print_json(value: &Value) {
    let mut out = std::io::stdout().lock();
    // A closed pipe (e.g. `| head`) is not worth a panic; the exit code still tells the story.
    let _ = serde_json::to_writer(&mut out, value);
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exit::CliError;

    #[test]
    fn error_object_has_stable_fields() {
        let v = error_json(Exit::Timeout, "لا رد no answer");
        assert_eq!(v["type"], "error");
        assert_eq!(v["code"], 8);
        assert_eq!(v["kind"], "timeout");
        assert_eq!(v["message"], "لا رد no answer");
        // One line, valid UTF-8 JSON, Arabic not escaped away.
        let s = serde_json::to_string(&v).unwrap();
        assert!(!s.contains('\n'));
        assert!(s.contains("لا رد"));
    }

    #[test]
    fn error_returns_the_mapped_code() {
        let out = Output {
            json: false,
            quiet: true,
        };
        let err = anyhow::Error::from(CliError::Interrupted);
        assert_eq!(out.error(&err, "x"), Exit::Interrupted);
    }
}
