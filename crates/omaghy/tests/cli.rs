//! What the binary does before it ever draws anything.
//!
//! These run the real executable with its stdout piped, which means
//! `is_terminal()` is false — the same situation as a container, a pipe, or a
//! script. That case used to surface as `No such device or address`.

use std::process::Command;

fn omaghy() -> Command {
    Command::new(env!("CARGO_BIN_EXE_omaghy"))
}

#[test]
fn without_a_terminal_it_says_so_plainly() {
    let out = omaghy().output().expect("binary runs");
    let err = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success(), "must not pretend to succeed");
    assert!(
        err.contains("interactive terminal"),
        "should name the actual problem, got: {err}"
    );
    assert!(
        !err.contains("os error 6") && !err.contains("No such device"),
        "must not leak the raw errno, got: {err}"
    );
}

#[test]
fn an_unknown_route_is_rejected_before_the_terminal_is_touched() {
    let out = omaghy().arg("nonsense").output().expect("binary runs");
    let err = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success());
    assert!(err.contains("not a route"), "got: {err}");
    // Argument errors must not be reported as terminal errors.
    assert!(!err.contains("interactive terminal"), "got: {err}");
}

#[test]
fn version_and_help_work_without_a_terminal() {
    // This is what the clean-room workflow runs, so it must never need a TTY.
    let out = omaghy().arg("--version").output().expect("binary runs");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("omaghy"));

    let out = omaghy().arg("--help").output().expect("binary runs");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("ROUTE"));
}
