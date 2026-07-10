//! End-to-end CLI checks for the unresolved-reference warnings.

use std::io::Write;
use std::process::{Command, Stdio};

/// Run the compiled `newtype` binary with `args`, feeding `source` on stdin.
/// Returns (exit_ok, stderr).
fn run(source: &str, args: &[&str]) -> (bool, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_newtype"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn the newtype binary");

    child
        .stdin
        .as_mut()
        .expect("stdin is piped")
        .write_all(source.as_bytes())
        .expect("failed to write the program to stdin");

    let output = child
        .wait_with_output()
        .expect("failed to wait for newtype");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn unresolved_reference_warns_but_exits_zero() {
    let (ok, stderr) = run("type A do Foo end", &[]);
    assert!(ok, "warnings alone must not fail the run:\n{stderr}");
    assert!(stderr.contains("cannot resolve type `Foo`"), "{stderr}");
    assert!(
        stderr.contains("cannot be resolved to a definition"),
        "{stderr}"
    );
}

#[test]
fn deny_unresolved_exits_nonzero() {
    let (ok, stderr) = run("type A do Foo end", &["--deny-unresolved"]);
    assert!(!ok, "--deny-unresolved must fail the run:\n{stderr}");
    assert!(stderr.contains("cannot resolve type `Foo`"), "{stderr}");
}

#[test]
fn resolved_program_emits_no_warning() {
    let (ok, stderr) = run(
        "type Foo do 1 end\nunittest \"t\" do\n  assert Foo <: number\nend",
        &[],
    );
    assert!(ok, "{stderr}");
    assert!(!stderr.contains("cannot resolve type"), "{stderr}");
}

#[test]
fn deny_unresolved_with_clean_program_exits_zero() {
    let (ok, stderr) = run(
        "type Foo do 1 end\ntype A do Foo end",
        &["--deny-unresolved"],
    );
    assert!(ok, "{stderr}");
}

#[test]
fn exact_optional_property_types_flag_changes_optional_assignability() {
    let src = "unittest \"t\" do\n  assert { x: number | undefined } <: { x?: number }\nend";
    let (ok_default, _) = run(src, &[]);
    assert!(ok_default, "widening holds by default");
    let (ok_exact, stderr) = run(src, &["--exact-optional-property-types"]);
    assert!(!ok_exact, "the widened source must be rejected:\n{stderr}");
}
