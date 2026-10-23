//! Build script for smart-cli test support.
//!
//! The CLI library does NOT compile schemas — smart-protocol-rs owns all
//! contract codegen. But the FIXTURE schema (tests/fixtures/fixture.capnp) is
//! test-only: it is deliberately outside the canonical contract tree and is
//! compiled only for the test support mock, which serves it the way a real
//! service compiles its own contract.

use std::path::{Path, PathBuf};

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|e| panic!("build.rs: {key} not set: {e}"))
}

/// Compile the FIXTURE schema (the one the CLI library never compiles) for the
/// test support, and write its node tree.
///
/// * `OUT_DIR/fixture_capnp.rs` — the generated bindings, addressed as
///   `crate::support::fixture::…` (the test support's own module path), so the
///   CLI LIBRARY has no binding for this schema at all.
/// * `OUT_DIR/fixture_schema.bin` — the schema's node tree, the bytes
///   `capnp compile -o-` produces: the contract's own `CodeGeneratorRequest`.
fn compile_fixture(binary: &str, manifest_dir: &Path, out_dir: &Path) {
    let fixture_dir = manifest_dir.join("tests/fixtures");
    let fixture = fixture_dir.join("fixture.capnp");
    if !fixture.is_file() {
        return;
    }
    println!("cargo::rerun-if-changed={}", fixture.display());

    let mut command = capnpc::CompilerCommand::new();
    command
        .capnp_executable(binary)
        .src_prefix(&fixture_dir)
        .import_path(&fixture_dir)
        .default_parent_module(vec!["support".to_string(), "fixture".to_string()])
        .output_path(out_dir);
    command.file(&fixture);
    command.run().unwrap_or_else(|e| {
        panic!("build.rs: capnp compile of the fixture schema failed:\n{e:?}")
    });
    // The generated file is named after the schema: `fixture_capnp.rs`.
    let generated = out_dir.join("fixture_capnp.rs");
    if !generated.is_file() {
        panic!("build.rs: the fixture schema produced no bindings");
    }

    // The node tree: `capnp compile -o-` writes the code generator request.
    let output = std::process::Command::new(binary)
        .arg("compile")
        .arg("-o-")
        .arg("--src-prefix")
        .arg(&fixture_dir)
        .arg(&fixture)
        .output()
        .unwrap_or_else(|e| panic!("build.rs: cannot run `{binary} compile -o-`: {e}"));
    if !output.status.success() {
        panic!(
            "build.rs: `{binary} compile -o-` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::write(out_dir.join("fixture_schema.bin"), &output.stdout)
        .unwrap_or_else(|e| panic!("build.rs: cannot write the fixture node tree: {e}"));
}

fn main() {
    let manifest_dir = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env("OUT_DIR"));
    let binary = std::env::var("CAPNP").unwrap_or_else(|_| "capnp".to_string());
    compile_fixture(&binary, &manifest_dir, &out_dir);
}
