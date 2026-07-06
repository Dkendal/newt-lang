//! End-to-end checks for the module loader: `import` statements load the
//! exported definitions of the referenced `.nt` file into the type
//! environment, and unresolvable modules/exports warn (evaluating as
//! indeterminate rather than producing bogus constraint errors).

use std::path::PathBuf;
use std::process::Command;

/// Run the compiled `newtype` binary with `--input tests/fixtures/imports/<name>`.
/// Returns (exit_ok, stderr).
fn run_fixture(name: &str) -> (bool, String) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/imports")
        .join(name);
    let output = Command::new(env!("CARGO_BIN_EXE_newtype"))
        .arg("--input")
        .arg(&path)
        .output()
        .expect("failed to run the newtype binary");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn imported_type_resolves_in_asserts_and_where_constraints() {
    let (ok, stderr) = run_fixture("main.nt");
    assert!(ok, "all assertions must pass:\n{stderr}");
    assert!(
        stderr.contains("2 assertion(s): 2 passed, 0 failed"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("does not satisfy its constraint"),
        "the resolved `Key` must satisfy the where-clause:\n{stderr}"
    );
}

#[test]
fn aliased_import_binds_the_alias() {
    let (ok, stderr) = run_fixture("aliased.nt");
    assert!(ok, "{stderr}");
    assert!(
        stderr.contains("1 assertion(s): 1 passed, 0 failed"),
        "{stderr}"
    );
}

#[test]
fn transitive_imports_resolve() {
    let (ok, stderr) = run_fixture("transitive.nt");
    assert!(ok, "{stderr}");
    assert!(
        stderr.contains("1 assertion(s): 1 passed, 0 failed"),
        "{stderr}"
    );
}

#[test]
fn missing_module_warns_and_evaluates_indeterminate() {
    let (ok, stderr) = run_fixture("missing_module.nt");
    assert!(!ok, "the indeterminate assertion must fail:\n{stderr}");
    assert!(
        stderr.contains("cannot resolve module `./nonexistent`"),
        "{stderr}"
    );
    // The unbound `Nope` must be indeterminate, not a definite constraint
    // violation: the generic application itself still evaluates …
    assert!(
        !stderr.contains("does not satisfy its constraint"),
        "{stderr}"
    );
    assert!(stderr.contains("ok      Get(\"id\") == \"id\""), "{stderr}");
    // … while a claim over the unbound name itself fails.
    assert!(stderr.contains("FAILED  \"id\" <: Nope"), "{stderr}");
}

#[test]
fn unexported_type_warns() {
    let (ok, stderr) = run_fixture("unexported.nt");
    assert!(
        !ok,
        "the unresolved import must fail the assertion:\n{stderr}"
    );
    assert!(
        stderr.contains("does not export a type named `Hidden`"),
        "{stderr}"
    );
}

#[test]
fn import_colliding_with_local_definition_errors() {
    let (ok, stderr) = run_fixture("collide_local.nt");
    assert!(!ok, "a name collision must fail the run:\n{stderr}");
    assert!(
        stderr.contains("imported name `Key` collides with a local definition of `Key`"),
        "{stderr}"
    );
}

#[test]
fn two_imports_binding_the_same_name_error() {
    let (ok, stderr) = run_fixture("collide_imports.nt");
    assert!(!ok, "a name collision must fail the run:\n{stderr}");
    assert!(
        stderr.contains("imported name `Key` is already bound by another import"),
        "{stderr}"
    );
}

#[test]
fn aliased_import_colliding_with_local_definition_errors() {
    let (ok, stderr) = run_fixture("collide_alias.nt");
    assert!(!ok, "a name collision must fail the run:\n{stderr}");
    assert!(
        stderr.contains("imported name `Local` collides with a local definition of `Local`"),
        "{stderr}"
    );
}
