//! Feature-gated codegen driver for smart-cli — the mechanical proof of
//! charter law #2 (you pay only for what you enable) and the only reason a
//! command's surface can be generated FROM the contract.
//!
//! It is a direct PORT of smart-lib-ng's `build.rs` approach (the
//! alloc-import post-processing and the transitive-import schema set), but
//! this crate stands entirely on its own: it reads the canonical schema dir
//! (read-only), compiles exactly the union of foundation + the enabled
//! `schema-*` features' schemas + their transitive imports, and generates its
//! OWN bindings into `OUT_DIR`. There is no dependency on smart-lib in any
//! form (the architecture law).
//!
//! What it emits (all into `OUT_DIR`, regenerated every build):
//!   * the capnp-generated `<stem>_capnp.rs` files (via the system `capnp`
//!     1.4.0 binary driving `capnpc` 0.27.0);
//!   * `schema_modules.rs`  — the `pub mod schema { … }` tree `lib.rs` includes;
//!   * `contract.rs`        — schema-id lookups + per-struct field lists, both
//!                            parsed from the canonical source, so the
//!                            selector's "valid fields" list and the wire's
//!                            interfaceId are GENERATED, never hand-written.
//!
//! Schema source (read-only input, never modified):
//!   1. `$SMART_SCHEMA_DIR` if set (hermetic / vendored builds);
//!   2. `<manifest dir>/../smart-protocol-official/schema`;
//!   3. the documented absolute path `/home/lufuno/smart/smart-protocol-official/schema`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Foundation schemas: never feature-gated, always compiled. Their own imports
/// (`types.capnp` only) are pulled in by `schema_set`. `cli.capnp` is the CLI's
/// registry contract (the availability honesty law reads `listUnavailable` from
/// it), so it too is always compiled — it depends only on types.capnp.
const FOUNDATION: [&str; 4] = ["abi.capnp", "capability.capnp", "cli.capnp", "types.capnp"];

/// The capnp triad, as declared in `Cargo.toml`.
const CAPNP_VERSION: &str = "0.27.2";
const CAPNPC_VERSION: &str = "0.27.0";

/// Module holding the capnp-generated code (see `crate::schema`).
const SCHEMA_MODULE: &str = "schema";

fn main() {
    let manifest_dir = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env("OUT_DIR"));
    let schema_dir = schema_dir(&manifest_dir);

    let enabled = enabled_schemas();
    let files = schema_set(&schema_dir, &enabled);

    for file in &files {
        println!(
            "cargo::rerun-if-changed={}",
            schema_dir.join(file).display()
        );
    }
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=SMART_SCHEMA_DIR");
    println!("cargo::rerun-if-env-changed=CAPNP");

    let binary = std::env::var("CAPNP").unwrap_or_else(|_| "capnp".to_string());
    let binary_version = capnp_version(&binary);
    compile(&binary, &schema_dir, &out_dir, &files, &binary_version);
    post_process_dir(&out_dir);
    write_schema_modules(&out_dir, &files, &binary_version);
    write_contract(&out_dir, &schema_dir, &files, &binary_version);

    println!(
        "cargo::warning=smart-cli: compiled {} schema file(s) for [{}]",
        files.len(),
        enabled.join(", ")
    );
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|e| panic!("build.rs: {key} not set: {e}"))
}

/// Resolve the schema directory (candidate order mirrors smart-lib's).
fn schema_dir(manifest_dir: &Path) -> PathBuf {
    let candidates = match std::env::var_os("SMART_SCHEMA_DIR") {
        Some(dir) => vec![PathBuf::from(dir)],
        None => vec![
            manifest_dir.join("../smart-protocol-official/schema"),
            PathBuf::from("/home/lufuno/smart/smart-protocol-official/schema"),
        ],
    };
    for candidate in candidates {
        if let Ok(dir) = candidate.canonicalize() {
            if dir.is_dir() {
                return dir;
            }
        }
    }
    panic!(
        "build.rs: cannot find the schema tree (tried: $SMART_SCHEMA_DIR, \
         <manifest>/../smart-protocol-official/schema, \
         /home/lufuno/smart/smart-protocol-official/schema)."
    );
}

/// The schema files of the enabled `schema-*` features, derived from
/// `CARGO_FEATURE_SCHEMA_*`. `schema-network` -> `network.capnp`; dashes become
/// underscores (unambiguous: no schema filename contains a dash).
fn enabled_schemas() -> Vec<String> {
    let mut stems: Vec<String> = Vec::new();
    for (key, value) in std::env::vars() {
        if value != "1" {
            continue;
        }
        let Some(flag) = key.strip_prefix("CARGO_FEATURE_SCHEMA_") else {
            continue;
        };
        let stem = flag.to_lowercase().replace('_', "-");
        if !stem.is_empty() {
            stems.push(format!("{}.capnp", stem.replace('-', "_")));
        }
    }
    stems.sort();
    stems.dedup();
    stems
}

/// The exact schema set to compile: foundation + enabled schemas + transitive
/// imports. A missing import is a hard error naming the requester.
fn schema_set(schema_dir: &Path, enabled: &[String]) -> Vec<String> {
    let mut queue: Vec<String> = FOUNDATION.iter().map(|f| f.to_string()).collect();
    queue.extend(enabled.iter().cloned());
    let mut found: BTreeSet<String> = BTreeSet::new();
    let mut missing: Vec<String> = Vec::new();

    while let Some(file) = queue.pop() {
        if !found.insert(file.clone()) {
            continue;
        }
        assert!(
            !file.contains('/') && !file.contains("..") && file.ends_with(".capnp"),
            "build.rs: refusing to compile schema path `{file}`"
        );
        let path = schema_dir.join(&file);
        if !path.is_file() {
            missing.push(file);
            continue;
        }
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("build.rs: cannot read {}: {e}", path.display()));
        for import in imported_schema_files(&source) {
            assert!(
                !import.contains('/') && !import.contains(".."),
                "build.rs: {} imports `{import}` which escapes the schema dir",
                path.display()
            );
            if !found.contains(&import) {
                queue.push(import);
            }
        }
    }
    if !missing.is_empty() {
        panic!("build.rs: schema file(s) not found in {}: {}", schema_dir.display(), missing.join(", "));
    }
    found.into_iter().collect()
}

/// Run the system `capnp` binary via `capnpc` over exactly `files`, generating
/// Rust into `out_dir`.
fn compile(binary: &str, schema_dir: &Path, out_dir: &Path, files: &[String], binary_version: &str) {
    let mut command = capnpc::CompilerCommand::new();
    command
        .capnp_executable(binary)
        .src_prefix(schema_dir)
        .import_path(schema_dir)
        .default_parent_module([SCHEMA_MODULE.to_string()].to_vec())
        .output_path(out_dir);
    for file in files {
        command.file(schema_dir.join(file));
    }
    command.run().unwrap_or_else(|e| {
        panic!(
            "build.rs: capnp compile failed (binary: {binary} {binary_version}; \
             codegen: capnpc {CAPNPC_VERSION}; runtime: capnp {CAPNP_VERSION}) \
             over {} file(s) in {}:\n{e:?}",
            files.len(),
            schema_dir.display()
        )
    });
}

/// Emit `OUT_DIR/schema_modules.rs`: the `pub mod schema { pub mod <stem>_capnp
/// { include!(...) } }` tree `lib.rs` includes. One module per compiled file,
/// and nothing else — so a family whose feature is off has NO module here.
fn write_schema_modules(out_dir: &Path, files: &[String], binary_version: &str) {
    let mut out = String::new();
    out.push_str(&format!(
        "// @generated by build.rs — DO NOT EDIT.\n// triad: {binary_version} | capnpc {CAPNPC_VERSION} | capnp {CAPNP_VERSION}\n\
         // compiled ({}): {}\n",
        files.len(),
        files.join(" ")
    ));
    out.push_str("pub mod schema {\n    #![allow(missing_docs)]\n    #![allow(clippy::all)]\n");
    for file in files {
        let module = schema_module_name(file);
        out.push_str(&format!(
            "    pub mod {module} {{ include!(concat!(env!(\"OUT_DIR\"), \"/{module}.rs\")); }}\n"
        ));
    }
    out.push_str("}\n");
    std::fs::write(out_dir.join("schema_modules.rs"), out)
        .unwrap_or_else(|e| panic!("build.rs: cannot write schema_modules.rs: {e}"));
}

/// Emit `OUT_DIR/contract.rs`: two GENERATED lookups the CLI depends on.
///
/// 1. `schema_id(stem)` — the canonical id (`@0x…` line) per schema, used as
///    the frame's `interfaceId` and printed by `--verbose`.
/// 2. `fields_of(struct_name)` — the ordered field names of every struct the
///    enabled schemas declare, so the selector's "unknown field" usage error
///    names the VALID fields from the schema rather than a hand-list (law).
fn write_contract(out_dir: &Path, schema_dir: &Path, files: &[String], binary_version: &str) {
    let mut ids = String::new();
    let mut field_arms = String::new();

    for file in files {
        let stem = schema_file_to_stem(file);
        let path = schema_dir.join(file);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("build.rs: cannot read {}: {e}", path.display()));
        if let Some(id) = schema_id_of(&source) {
            ids.push_str(&format!("    \"{stem}\" => Some({id}),\n"));
        }
        for (name, names) in struct_fields(&source) {
            // A struct name can be declared by more than one schema (the
            // contract's `ProxyConfig` lives in both network.capnp and
            // web.capnp, by design). The first declaration wins so the match
            // stays exhaustive WITHOUT an unreachable arm; a selector over a
            // colliding name names the first schema's fields, and the
            // command supplies its own accessor for whatever it lists.
            if !field_arms.contains(&format!("\"{name}\" => Some(")) {
                let list = names
                    .iter()
                    .map(|n| format!("\"{n}\""))
                    .collect::<Vec<_>>()
                    .join(", ");
                field_arms.push_str(&format!("    \"{name}\" => Some(&[{list}]),\n"));
            }
        }
    }

    let out = format!(
        "// @generated by build.rs — DO NOT EDIT.\n\
         // triad: {binary_version} | capnpc {CAPNPC_VERSION} | capnp {CAPNP_VERSION}\n\n\
         pub mod contract {{\n\
         /// The canonical schema id (`@0x…`) per schema stem.\n\
         pub fn schema_id(stem: &str) -> Option<u64> {{\n    match stem {{\n{ids}        _ => None,\n    }}\n}}\n\n\
         /// The ordered field names of a contract struct, by its schema name.\n\
         /// GENERATED from the canonical source: the selector's \"valid fields\"\n\
         /// usage error reads this, so it can never drift from the contract.\n\
         pub fn fields_of(struct_name: &str) -> Option<&'static [&'static str]> {{\n    match struct_name {{\n{field_arms}        _ => None,\n    }}\n}}\n}}\n"
    );
    std::fs::write(out_dir.join("contract.rs"), out)
        .unwrap_or_else(|e| panic!("build.rs: cannot write contract.rs: {e}"));
}

// ═════════════════════════════════════════════════════════════════════════
// Post-processing — PORTED from smart-lib-ng's src/codegen/mod.rs. capnp 0.27
// emits bare `Box<..>`, `Vec` and `.to_string()` in interface code; in a
// no_std crate those names are not in scope, so we inject the alloc imports at
// every module boundary of a file that contains interface code. `String` is
// deliberately NOT imported: capnp can generate enum variants named `String`.
// ═════════════════════════════════════════════════════════════════════════

const ALLOC_IMPORTS: &str = "#[allow(unused_imports)]\nuse alloc::boxed::Box;\n\
                             #[allow(unused_imports)]\nuse alloc::string::ToString;\n\
                             #[allow(unused_imports)]\nuse alloc::vec::Vec;\n";

fn post_process_dir(dir: &Path) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    for entry in entries {
        let Ok(entry) = entry else { continue };
        if entry.path().extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        post_process_file(&entry.path());
    }
}

fn post_process_file(path: &Path) {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    if let Some(patched) = patch_generated(&content) {
        std::fs::write(path, patched).unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
    }
}

fn patch_generated(content: &str) -> Option<String> {
    if content.contains("alloc::boxed::Box") {
        return None;
    }
    if !content.contains("FromClientHook") && !content.contains("DynClientHook") {
        return None;
    }
    Some(inject_alloc_imports(content))
}

fn inject_alloc_imports(source: &str) -> String {
    let mut result = String::with_capacity(source.len() + ALLOC_IMPORTS.len() * 8);
    let mut in_module = false;
    let mut brace_depth = 0usize;
    let mut waiting_for_inner_attrs = false;

    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("pub mod ") && trimmed.ends_with('{') {
            result.push_str(line);
            result.push('\n');
            in_module = true;
            brace_depth = 1;
            waiting_for_inner_attrs = true;
            continue;
        }
        if in_module {
            if waiting_for_inner_attrs && trimmed.starts_with("#![") {
                result.push_str(line);
                result.push('\n');
                continue;
            }
            if waiting_for_inner_attrs {
                waiting_for_inner_attrs = false;
                result.push_str(ALLOC_IMPORTS);
            }
            brace_depth = brace_depth.saturating_add(trimmed.matches('{').count());
            brace_depth = brace_depth.saturating_sub(trimmed.matches('}').count());
            if brace_depth == 0 {
                in_module = false;
            }
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}

// ═════════════════════════════════════════════════════════════════════════
// Minimal capnp source analysis: imports, schema id, struct field names.
// Comments are stripped first (the contract's prose names *deleted* schemas,
// and a naive match would resurrect them). These mirror smart-lib's codegen
// helpers so both consumers read the canonical source the same way.
// ═════════════════════════════════════════════════════════════════════════

fn strip_capnp_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    for line in source.lines() {
        let code = match line.find('#') {
            Some(idx) => &line[..idx],
            None => line,
        };
        out.push_str(code);
        out.push('\n');
    }
    out
}

fn imported_schema_files(source: &str) -> Vec<String> {
    let code = strip_capnp_comments(source);
    let mut names = Vec::new();
    for line in code.lines() {
        let mut rest = line;
        while let Some(idx) = rest.find("import") {
            let before = rest[..idx].chars().next_back();
            rest = &rest[idx + "import".len()..];
            if before.is_some_and(char::is_alphanumeric) {
                continue;
            }
            let after = rest.trim_start();
            let Some(quoted) = after.strip_prefix('"') else { continue };
            match quoted.find('"') {
                Some(end) => names.push(quoted[..end].to_string()),
                None => continue,
            }
        }
    }
    names
}

/// The canonical `@0x…;` id of a schema source, as a u64.
fn schema_id_of(source: &str) -> Option<u64> {
    for line in source.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("@0x") {
            let hex = rest.trim_end_matches(';').trim();
            if let Ok(id) = u64::from_str_radix(hex, 16) {
                return Some(id);
            }
        }
    }
    None
}

/// Every `struct Name { … }` in a schema source, with the field names declared
/// at its own (first) nesting level. Nested struct/enum/interface bodies are
/// skipped by depth tracking, so their fields never leak into the parent.
/// Union arms and plain fields are both listed — they are all addressable
/// filterable fields of the type.
fn struct_fields(source: &str) -> Vec<(String, Vec<String>)> {
    let code = strip_capnp_comments(source);
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    // Stack of (name, fields) for open struct bodies; depth 0 is file scope.
    let mut stack: Vec<(String, Vec<String>)> = Vec::new();

    for line in code.lines() {
        let t = line.trim();
        // A struct declaration opens a body.
        if let Some(name) = opening_type(t, "struct") {
            stack.push((name, Vec::new()));
        } else if opening_type(t, "enum").is_some()
            || opening_type(t, "interface").is_some()
            || t.starts_with("union")
        {
            // Enums/interfaces/unions open bodies whose members are NOT struct
            // fields; push a sentinel that collects nothing.
            stack.push((String::new(), Vec::new()));
        }

        // Field declaration at the current innermost level: an identifier
        // immediately followed by ` @` — `fieldName @0 :Type;` or `arm @0;`.
        if let Some(top) = stack.last_mut() {
            if !top.0.is_empty() {
                if let Some(field) = field_name(t) {
                    if top.1.iter().all(|existing| existing != &field) {
                        top.1.push(field);
                    }
                }
            }
        }

        // Close braces: a line ending in `}` pops the innermost body.
        let closes = t.matches('}').count() as i32 - t.matches('{').count() as i32;
        // Account for opens on this same line too (rare in this contract).
        let opens = t.matches('{').count();
        if opens == 0 {
            for _ in 0..closes.max(0) {
                if let Some((name, fields)) = stack.pop() {
                    if !name.is_empty() {
                        out.push((name, fields));
                    }
                }
            }
        }
    }
    out
}

/// `struct Name {` / `enum Name {` / `interface Name {` — the opening keyword
/// and a following type name.
fn opening_type(t: &str, keyword: &str) -> Option<String> {
    let rest = t.strip_prefix(keyword).and_then(|r| r.strip_prefix(' ')).or_else(|| {
        t.strip_prefix(&format!("{keyword} "))
    })?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// The identifier immediately preceding a field's ` @` ordinal marker.
fn field_name(t: &str) -> Option<String> {
    let at = t.find(" @")?;
    let before = t[..at].trim_end();
    let ident: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if ident.is_empty() {
        None
    } else {
        Some(ident)
    }
}

fn schema_file_to_stem(file: &str) -> &str {
    file.strip_suffix(".capnp").unwrap_or(file)
}

/// Module name capnp codegen uses for `<stem>.capnp`: `<stem>_capnp`.
fn schema_module_name(file: &str) -> String {
    format!("{}_capnp", schema_file_to_stem(file).replace('-', "_"))
}

fn capnp_version(binary: &str) -> String {
    let output = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .unwrap_or_else(|e| panic!("build.rs: failed to run `{binary} --version`: {e}"));
    if !output.status.success() {
        panic!("build.rs: `{binary} --version` exited {:?}", output.status);
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
