//! `TLP-Desk.exe --accept-prompt` as a process: refuses bad input without opening a window.
//! (Showing the real window is a manual test: it takes the foreground on the desktop.)

use std::io::Write as _;
use std::process::{Command, Stdio};

fn run_prompt(stdin: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_TLP-Desk"))
        .arg("--accept-prompt")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start TLP-Desk --accept-prompt");
    if let Some(mut s) = child.stdin.take() {
        // The popup may exit (refusing the input) before reading it all.
        let _ = s.write_all(stdin);
    }
    child.wait_with_output().expect("wait")
}

#[test]
fn garbage_request_exits_2_without_an_answer() {
    let out = run_prompt(b"{not json\n");
    assert_eq!(out.status.code(), Some(2));
    assert!(
        out.stdout.is_empty(),
        "no decision for a request never shown"
    );
}

#[test]
fn empty_stdin_exits_2() {
    let out = run_prompt(b"");
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}

#[test]
fn oversized_request_is_refused_before_parsing() {
    let mut big = vec![b'x'; 64 * 1024];
    big.push(b'\n');
    let out = run_prompt(&big);
    assert_eq!(out.status.code(), Some(2));
}
