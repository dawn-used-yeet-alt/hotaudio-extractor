//! The command-line contract, exercised through the built binary.
//!
//! Argument parsing is otherwise only covered by the daily `live/` suite, which
//! needs the network; these checks are the offline half: help, version, the exit
//! codes for bad input, and the completions/man-page generators. Spawning a
//! process cannot exist on `wasm32`, so the whole file is native-only.

#![cfg(not(target_arch = "wasm32"))]

use std::process::Command;

/// Run the binary and return its exit code, stdout and stderr.
fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_hotaudio-download"))
        .args(args)
        .output()
        .expect("run hotaudio-download");
    (
        out.status.code().expect("the process was signalled"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn help_and_version_succeed() {
    let (code, stdout, _) = run(&["--help"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("Usage:"),
        "help has no usage line:\n{stdout}"
    );
    assert!(stdout.contains("--stream-to"), "help omits --stream-to");
    assert!(
        stdout.contains("Exit status:"),
        "help omits the exit-status note"
    );

    let (code, stdout, stderr) = run(&["--version"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("hotaudio-download"), "got {stdout:?}");
}

#[test]
fn bad_input_exits_two_before_any_network_call() {
    // A lone URL would be enough to reach the pipeline, so every case here has
    // to fail at (or immediately after) parsing, before any request is sent.
    let url = "https://hotaudio.net/u/user/track-slug";
    let cases: &[&[&str]] = &[
        &[],
        &["--definitely-not-a-flag", url],
        &["--quiet", "--verbose", url],
        &["--keys-only", url],
        &["--verify", "--stream-to", "-", url],
    ];
    for args in cases {
        let (code, _, stderr) = run(args);
        assert_eq!(code, 2, "args {args:?} exited {code}: {stderr}");
    }
}

#[test]
fn completions_and_man_page_are_printed() {
    for shell in ["bash", "zsh", "fish", "powershell"] {
        let (code, stdout, stderr) = run(&["--completions", shell]);
        assert_eq!(code, 0, "--completions {shell}: {stderr}");
        assert!(!stdout.is_empty(), "--completions {shell} printed nothing");
    }

    let (code, stdout, stderr) = run(&["--man-page"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains(".TH"), "man page is not roff:\n{stdout}");
}
