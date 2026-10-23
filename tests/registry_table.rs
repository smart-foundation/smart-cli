//! The registry table is GENERATED from the schema tree: the CLI names the
//! methods it calls, and the schema names their ordinals.
//!
//! Two proofs:
//!
//! 1. **No method ordinal is written by hand** anywhere in the CLI's source:
//!    the fallback table resolves each method through the GENERATED contract
//!    table (`crate::contract`), which build.rs derives from the schema tree.
//! 2. Every method the table names resolves to an ordinal the schema
//!    declares — for the schemas this build compiled. A method of a schema
//!    this build did NOT compile says so in its route line; it never shows an
//!    ordinal it does not know.

use std::path::{Path, PathBuf};

use smart_protocol::contract;
use smart_cli::registry;

/// The CLI's source tree.
fn source_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every `.rs` file under a directory.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(rust_files(&path));
        } else if path.extension().map(|ext| ext == "rs").unwrap_or(false) {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// True when the text at `index` is `@` followed by digits that are NOT part
/// of a host address (`@10.0.0.5`) — that is, a method ordinal.
fn is_hand_written_ordinal(text: &str, index: usize) -> bool {
    let bytes = text.as_bytes();
    let mut cursor = index + 1;
    while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
        cursor += 1;
    }
    if cursor == index + 1 {
        return false;
    }
    // A host address (`@10.0.0.5:7777`) is not an ordinal.
    !(cursor < bytes.len() && bytes[cursor] == b'.')
}

#[test]
fn no_method_ordinal_is_written_by_hand_in_the_clis_source() {
    let files = rust_files(&source_dir());
    assert!(!files.is_empty(), "the CLI's source tree is empty");
    let mut found: Vec<String> = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).expect("read a source file");
        for (index, ch) in text.char_indices() {
            if ch == '@' && is_hand_written_ordinal(&text, index) {
                let line = text[..index].lines().count() + 1;
                found.push(format!(
                    "{}:{}: {}",
                    file.strip_prefix(env!("CARGO_MANIFEST_DIR")).unwrap_or(&file).display(),
                    line,
                    text.lines().nth(line - 1).unwrap_or("").trim()
                ));
            }
        }
    }
    assert!(
        found.is_empty(),
        "a method ordinal is written by hand:\n{}",
        found.join("\n")
    );
}

#[test]
fn every_method_the_table_names_resolves_to_a_schema_ordinal() {
    for spec in registry::TASKS {
        for (interface, method) in spec.calls {
            let Some(info) = contract::interface_named(interface) else {
                // A schema this build did not compile: the route line says so
                // (it never shows an ordinal it does not know).
                continue;
            };
            assert!(
                contract::method_id(info.stem, info.name, method).is_some(),
                "the schema does not declare `{interface}.{method}`"
            );
        }
    }
}

#[test]
fn the_route_line_carries_the_generated_ordinals() {
    let wifi = registry::TASKS
        .iter()
        .find(|spec| spec.task.first() == Some(&"wifi"))
        .expect("the wifi task");
    let route = registry::route(wifi);
    // The ordinal comes from the generated table: the schema declares
    // `wifiScan` at 6, and the route line names it.
    assert!(route.contains("wifiScan @6"), "{route}");
    // A method of a schema this build did not compile says so, rather than
    // showing a guessed ordinal.
    let dns = registry::TASKS
        .iter()
        .find(|spec| spec.task.first() == Some(&"dns"))
        .expect("the dns task");
    let route = registry::route(dns);
    assert!(
        route.contains("DnsService.resolve (not compiled in this build)"),
        "{route}"
    );
}
