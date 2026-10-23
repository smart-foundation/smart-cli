//! `files` — the host's filesystem: the listing, the walk, the stat, the
//! bytes, the versions, the branches, the trash, the quota: the coupled
//! shell's presentation, re-keyed from the engine's mirror types
//! (`DirectoryEntry`, `FileInfo`, `DiskUsage`, `FileLock`, `FileVersion`,
//! `ContentRefResult`, `TrashEntry`) to the CONTRACT'S RESULT READERS
//! (`files.capnp`'s `listDirectory` / `searchFiles` / `walkTree` / `statFile`
//! / `getFileMetadata` / `readFile` / `getDiskUsage` / `getLocks` /
//! `getFileVersions` / `listBranches` / `listTrash` / `getRecentFiles` /
//! `getDefaultApp` / `searchFileContent` / `compareFiles` /
//! `getFilesystemHealth` / `getQuota`).
//!
//! ## What the re-keying changed
//!
//! * the engine's `DirectoryEntry` mirror became the contract's
//!   `DirectoryEntry` reader — the same fields, read from the wire;
//! * the shell's `Value` records became the contract's own JSON (the field
//!   names are the schema's, verbatim);
//! * the shell's **mounts** view (`du`'s mount row, the host's own
//!   /proc/mounts) has no contract method of its own: the contract's
//!   `getDiskUsage` carries the mount point and the mount id, so the du view
//!   keeps the mount row it can fill and nothing else;
//! * the shell's **watches** view (the session's own watch ledger) has no
//!   contract method: the contract's `watchDirectory` is the subscription
//!   itself, so the watch view is the event stream;
//! * the shell's `caps` view has no contract method (the capability tokens
//!   live in discovery's descriptor metadata), so it is not on this surface;
//! * the shell's `cat` was the engine's own handle read; the contract splits
//!   it into `openFile` → `readFile` → `closeFile`, and the registry
//!   publishes the composition ("files cat" composes openFile,closeFile), so
//!   the presenter makes those calls in that order.
//!
//! The registry publishes eighty-three paths.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::json;
use crate::present;
use smart_protocol::schema::files_capnp as wire;
use smart_protocol::schema::files_capnp::{
    FileEventType, FileLockType, FileType, OpenMode, RetentionAction, SortField, SortOrder,
    SpecialPathType,
};
use crate::transport as tp;

use super::domain::{self, Presenter};
use super::App;

/// The schema stem the family presents.
const STEM: &str = "files";
/// The contract interface the family presents.
const INTERFACE: &str = "Files";

/// The family's presenter lookup: keyed by **(schema id, method name)**.
pub fn presenter(schema_id: u64, method: &str) -> Option<Presenter> {
    if schema_id != smart_protocol::contract::schema_id(STEM)? {
        return None;
    }
    Some(match method {
        "listDirectory" => ls,
        "searchFiles" => find,
        "walkTree" => tree,
        "statFile" => stat,
        "getFileType" => stat,
        "getFileMetadata" => xattrs,
        "readFile" => cat,
        "getDiskUsage" => du,
        "getLocks" => locks,
        "getFileHash" => hash,
        "getFileChecksum" => checksum,
        "getFileVersions" => versions,
        "getFileVersion" => version,
        "listBranches" => branches,
        "storeContentAddressed" => store,
        "getByHash" => by_hash,
        "deduplicate" => dedupe,
        "getDeduplicationStats" => dedupe_stats,
        "watchDirectory" => watch,
        "getSpecialPath" => special,
        "listTrash" => trash,
        "getRecentFiles" => recents,
        "getDefaultApp" => app,
        "searchFileContent" => grep,
        "compareFiles" => compare,
        "compareDirectories" => compare,
        "compareVersions" => compare,
        "getFilesystemHealth" => health,
        "getQuota" => quota,
        "getDirectorySize" => size,
        "getDirectoryStats" => stats,
        "getFileActivity" => activity,
        "getLifecycle" => lifecycle,
        "getCompression" => compression,
        "getEncryptionInfo" => encryption_info,
        "analyzeFileContent" => analyze,
        "branchVersion" => branch,
        "createSnapshot" => snapshot,
        "getCurrentDirectory" => pwd,
        "fileExists" => exists,
        "resolveSymlink" => realpath,
        "openFile" => open,
        "createEphemeral" => open,
        "createEncrypted" => open,
        "acquireFile" => acquire,
        // The gated writes: the CLI issues the call; the SERVICE decides.
        "createDirectory" => write_create_directory,
        "touch" => write_touch,
        "removeFile" => write_remove_file,
        "removeDirectory" => write_remove_directory,
        "renameFile" => write_rename_file,
        "copyFile" => write_copy_file,
        "createSymlink" => write_create_symlink,
        "linkFile" => write_link_file,
        "lockFile" => write_lock_file,
        "unlockFile" => write_unlock_file,
        "closeFile" => write_close_file,
        "flushFile" => write_flush_file,
        "createFile" => write_create_file,
        "changeDirectory" => write_change_directory,
        "restoreVersion" => write_restore_version,
        "decryptFile" => write_decrypt_file,
        "setCompression" => write_set_compression,
        "mergeBranch" => write_merge_branch,
        "restoreTrash" => write_restore_trash,
        "emptyTrash" => write_empty_trash,
        "clearRecentFiles" => write_clear_recent_files,
        "setQuota" => write_set_quota,
        "setLifecycle" => write_set_lifecycle,
        "applyLifecycle" => write_apply_lifecycle,
        "writeFile" => write_write_file,
        "batchCopy" => write_batch_copy,
        "batchMove" => write_batch_move,
        "batchDelete" => write_batch_delete,
        _ => return None,
    })
}

/// Build one params message.
fn msg<F>(f: F) -> Vec<u8>
where
    F: FnOnce(&mut capnp::message::Builder<capnp::message::HeapAllocator>),
{
    let mut message = capnp::message::Builder::new_default();
    f(&mut message);
    tp::encode_message(&message)
}

/// The path the caller named (a positional, or `--path`).
fn path_of(args: &[String]) -> String {
    domain::flag(args, "--path")
        .or_else(|| domain::positionals(args).first().map(|p| (*p).to_string()))
        .unwrap_or_else(|| ".".to_string())
}

// ═════════════════════════════════════════════════════════════════════════
// `files ls` — the directory, read natively.
// ═════════════════════════════════════════════════════════════════════════

fn ls(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let pattern = domain::flag(args, "--pattern").unwrap_or_default();
    let all = domain::has(args, "--all");
    let recursive = domain::has(args, "--recursive");
    let sort = domain::flag(args, "--sort").unwrap_or_else(|| "name".to_string());
    let order = domain::flag(args, "--order").unwrap_or_else(|| "ascending".to_string());
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::list_directory_params::Builder<'_>>();
        params.set_path(&path);
        {
            let mut filter = params.reborrow().init_filter();
            if !pattern.is_empty() {
                let mut patterns = filter.reborrow().init_patterns(1);
                patterns.set(0, pattern.as_str());
            }
            filter.set_include_hidden(all);
            filter.set_recursive(recursive);
            filter.set_sort_by(sort_field_of(&sort));
            filter.set_sort_order(sort_order_of(&order));
            filter.reborrow().init_page().set_limit(u32::MAX);
        }
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "listDirectory", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::list_directory_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::directory_entry::Reader<'_>> = results
                .get_entries()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            let total = results.get_total();
            (entries_json("entries", &rows), ls_human(app, &rows, total))
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// The listing table: the glyph, the name, the kind, the size, the stamp.
fn ls_human(app: &App, rows: &[wire::directory_entry::Reader<'_>], total: u64) -> String {
    let mut out = String::new();
    if rows.is_empty() {
        out.push_str(&format!(
            "  {} {} {} — the directory is empty (the host was read and answered with no entries)\n",
            domain::cyan(app, "📁"),
            domain::bold(app, "Files"),
            domain::dim(app, "(0)")
        ));
        out.trim_end().to_string()
    } else {
        let sum: u64 = rows.iter().map(|row| row.get_size()).sum();
        out.push_str(&format!(
            "  {} {} {} {}\n",
            domain::cyan(app, "📁"),
            domain::bold(app, "Files"),
            domain::dim(app, &format!("({})", rows.len())),
            domain::dim(app, &format!("— getdents64, {} total", present::human_bytes(sum)))
        ));
        let _ = total;
        let mut table = present::Table::new(vec![
            String::new(),
            String::from("NAME"),
            String::from("KIND"),
            String::from("SIZE"),
            String::from("MODIFIED"),
        ]);
        for row in rows {
            let kind = file_type_token(row.get_type());
            table = table.row(vec![
                kind_glyph(kind),
                present::Cell::path(&text(row.get_name())),
                present::Cell::plain(kind),
                present::Cell::plain(present::human_bytes(row.get_size())),
                present::Cell::plain(stamp_label(&row.get_modified())),
            ]);
        }
        out.push_str(&table.render(app.style()));
        out.trim_end().to_string()
    }
}

// ═════════════════════════════════════════════════════════════════════════
// `files find` / `files tree` — the walk.
// ═════════════════════════════════════════════════════════════════════════

fn find(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let pattern = domain::flag(args, "--pattern").unwrap_or_else(|| "*".to_string());
    let recursive = domain::has(args, "--recursive");
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::search_files_params::Builder<'_>>();
        let mut query = params.reborrow().init_query();
        query.set_path(&path);
        query.set_pattern(&pattern);
        query.set_recursive(recursive);
        query.reborrow().init_page().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "searchFiles", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::search_files_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::file_search_result::Reader<'_>> = results
                .get_results()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            let _total = results.get_total();
            (
                search_json("results", &rows),
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} {}\n",
                        domain::cyan(app, "🔍"),
                        domain::bold(app, "Find"),
                        domain::dim(app, &format!("({})", rows.len())),
                        domain::dim(app, &format!(
                            "under {path} — the backend's own glob over getdents64"
                        ))
                    ));
                    if rows.is_empty() {
                        out.push_str(&format!(
                            "  └─ {} no entry under {path} matches (an honest empty — the walk \
                             ran)\n",
                            domain::dim(app, "○")
                        ));
                    } else {
                        let mut table = present::Table::new(vec![
                            String::new(),
                            String::from("PATH"),
                            String::from("KIND"),
                            String::from("SIZE"),
                        ]);
                        for row in &rows {
                            let kind = file_type_token(row.get_type());
                            table = table.row(vec![
                                kind_glyph(kind),
                                present::Cell::path(&text(row.get_path())),
                                present::Cell::plain(kind),
                                present::Cell::plain(present::human_bytes(row.get_size())),
                            ]);
                        }
                        out.push_str(&table.render(app.style()));
                    }
                    out.trim_end().to_string()
                },
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn tree(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let depth = domain::number::<u32>(args, "--depth", 3);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::walk_tree_params::Builder<'_>>();
        params.set_path(&path);
        params.set_max_depth(depth);
        params.reborrow().init_page().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "walkTree", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::walk_tree_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::directory_entry::Reader<'_>> = results
                .get_entries()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            let _total = results.get_total();
            (
                entries_json("entries", &rows),
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} {}\n",
                        domain::cyan(app, "🌳"),
                        domain::bold(app, "Tree"),
                        domain::dim(app, &format!("({})", rows.len())),
                        domain::dim(app, &format!("— {path} walked to depth {depth}"))
                    ));
                    if rows.is_empty() {
                        out.push_str(&format!(
                            "  └─ {} the walk found nothing under {path} (an honest empty)\n",
                            domain::dim(app, "○")
                        ));
                    } else {
                        let mut table = present::Table::new(vec![
                            String::new(),
                            String::from("NAME"),
                            String::from("KIND"),
                            String::from("SIZE"),
                        ]);
                        for row in &rows {
                            let kind = file_type_token(row.get_type());
                            let name = text(row.get_name());
                            table = table.row(vec![
                                kind_glyph(kind),
                                present::Cell::path(&name),
                                present::Cell::plain(kind),
                                present::Cell::plain(present::human_bytes(row.get_size())),
                            ]);
                        }
                        out.push_str(&table.render(app.style()));
                    }
                    out.trim_end().to_string()
                },
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// `files stat` — the statx record, one row per measured field.
// ═════════════════════════════════════════════════════════════════════════

fn stat(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::stat_file_params::Builder<'_>>();
        params.set_path(&path);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "statFile", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::stat_file_results::Owned, _, _>(&bytes, |results| {
            let result = results.get_result().ok();
            (
                {
                    let mut out = String::from("{\"result\":");
                    match &result {
                        Some(result) => match result.which() {
                            Ok(wire::file_info_result::Which::Info(Ok(info))) => {
                                info_json(&mut out, &info)
                            }
                            Ok(wire::file_info_result::Which::Error(Ok(error))) => {
                                let mut o = json::Obj::new(&mut out);
                                o.key("error").str(&text(error.get_message()));
                                o.end();
                            }
                            _ => {
                                let mut o = json::Obj::new(&mut out);
                                o.key("unknown").str("unknown");
                                o.end();
                            }
                        },
                        None => {
                            let mut o = json::Obj::new(&mut out);
                            o.key("unknown").str("the service answered no result");
                            o.end();
                        }
                    }
                    out.push('}');
                    out
                },
                match &result {
                    Some(result) => match result.which() {
                        Ok(wire::file_info_result::Which::Info(Ok(info))) => {
                            stat_human(app, &info, &path)
                        }
                        Ok(wire::file_info_result::Which::Error(Ok(error))) => format!(
                            "  {} stat — {}",
                            domain::red(app, "✗"),
                            text(error.get_message())
                        ),
                        _ => format!("  {} stat — no answer for {path}", domain::red(app, "✗")),
                    },
                    None => format!("  {} stat — no answer for {path}", domain::red(app, "✗")),
                },
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// The stat tree: the shell's own rows, the contract's own fields.
fn stat_human(app: &App, info: &wire::file_info::Reader<'_>, path: &str) -> String {
    let kind = file_type_token(info.get_type());
    let mut out = String::new();
    out.push_str(&format!(
        "  {} {} {}\n",
        domain::cyan(app, "🔎"),
        domain::bold(app, "Stat"),
        domain::bold(app, path)
    ));
    out.push_str(&format!(
        "  ├─ Type       {} {kind}\n",
        domain::glyph(app, &kind_glyph(kind))
    ));
    out.push_str(&format!(
        "  ├─ Size       {} ({})\n",
        info.get_size(),
        present::human_bytes(info.get_size())
    ));
    out.push_str(&format!(
        "  ├─ Created    {}\n",
        stamp_label(&info.get_created())
    ));
    out.push_str(&format!(
        "  ├─ Modified   {}\n",
        stamp_label(&info.get_modified())
    ));
    out.push_str(&format!(
        "  ├─ Accessed   {}\n",
        stamp_label(&info.get_accessed())
    ));
    let mime = mime_of(&info.get_mime_type());
    out.push_str(&format!(
        "  ├─ Mime       {}\n",
        if mime.is_empty() {
            "— the host's mime.types table does not name it".to_string()
        } else {
            mime
        }
    ));
    let hash = hash_of(&info.get_content_hash());
    if !hash.is_empty() {
        out.push_str(&format!("  └─ Hash       {}", domain::dim(app, &hash)));
    } else {
        out.push_str("  └─ Source     statx(2) — the kernel's own answer");
    }
    out
}

// ═════════════════════════════════════════════════════════════════════════
// `files xattrs` — the extended metadata.
// ═════════════════════════════════════════════════════════════════════════

fn xattrs(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_file_metadata_params::Builder<'_>>();
        params.set_path(&path);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getFileMetadata", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::get_file_metadata_results::Owned, _, _>(&bytes, |results| {
            let metadata = results.get_metadata().ok();
            (
                {
                    let mut out = String::from("{\"metadata\":");
                    let mut o = json::Obj::new(&mut out);
                    match &metadata {
                        Some(metadata) => {
                            o.key("path").str(&text(metadata.get_path()));
                            o.key("type").str(file_type_token(metadata.get_type()));
                            o.key("size").int(metadata.get_size());
                            o.key("created")
                                .sub(|out| timestamp(out, &metadata.get_created()));
                            o.key("modified")
                                .sub(|out| timestamp(out, &metadata.get_modified()));
                            o.key("accessed")
                                .sub(|out| timestamp(out, &metadata.get_accessed()));
                            o.key("mimeType")
                                .str(&mime_of(&metadata.get_mime_type()));
                            o.key("contentHash").str(&hash_of(&metadata.get_content_hash()));
                        }
                        None => {
                            o.key("path").str(&path);
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                match &metadata {
                    Some(metadata) => {
                        let mut out = String::new();
                        out.push_str(&format!(
                            "  {} {} {}\n",
                            domain::cyan(app, "🏷"),
                            domain::bold(app, "Extended metadata"),
                            domain::bold(app, &path)
                        ));
                        out.push_str(&format!(
                            "  ├─ Type       {}\n",
                            file_type_token(metadata.get_type())
                        ));
                        out.push_str(&format!(
                            "  ├─ Size       {}\n",
                            present::human_bytes(metadata.get_size())
                        ));
                        out.push_str(&format!(
                            "  ├─ Created    {} · modified {}\n",
                            stamp_label(&metadata.get_created()),
                            stamp_label(&metadata.get_modified())
                        ));
                        out.push_str("  └─ Source     read natively from the file's own metadata");
                        out
                    }
                    None => format!(
                        "  {} {} {} — the host did not answer",
                        domain::cyan(app, "🏷"),
                        domain::bold(app, "Extended metadata"),
                        domain::bold(app, &path)
                    ),
                },
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// `files cat` — the composition the registry publishes: openFile, readFile,
// closeFile.
// ═════════════════════════════════════════════════════════════════════════

fn cat(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    // The open.
    let open_params = msg(|m| {
        let mut params = m.init_root::<wire::files::open_file_params::Builder<'_>>();
        params.set_path(&path);
        params.reborrow().init_request().set_mode(OpenMode::Read);
    });
    let open_bytes = match super::call_in(app, STEM, INTERFACE, "openFile", &open_params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let handle = tp::with_root::<wire::files::open_file_results::Owned, _, _>(&open_bytes, |r| {
        r.get_result()
            .ok()
            .and_then(|result| match result.which() {
                Ok(wire::open_result::Which::Handle(handle)) => handle
                    .ok()
                    .and_then(|handle| handle.get_handle().ok().map(|id| id.get_id())),
                _ => None,
            })
    })
    .flatten();
    let Some(handle) = handle else {
        super::usage_error_app(
            app,
            &format!("the service answered no file handle for {path}"),
        );
        return crate::registry::EXIT_USAGE;
    };
    // The read.
    let read_params = msg(|m| {
        let mut params = m.init_root::<wire::files::read_file_params::Builder<'_>>();
        params.reborrow().init_handle().init_handle().set_id(handle);
        params.set_offset(0);
        params.set_size(u32::MAX);
    });
    let read_bytes = match super::call_in(app, STEM, INTERFACE, "readFile", &read_params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let payload = tp::with_root::<wire::files::read_file_results::Owned, _, _>(&read_bytes, |r| {
        r.get_data()
            .ok()
            .and_then(|handle| match handle.which() {
                Ok(smart_protocol::schema::types_capnp::data_handle::Which::Inline(data)) => {
                    data.map(|bytes| bytes.to_vec()).ok()
                }
                _ => None,
            })
    })
    .flatten()
    .unwrap_or_default();
    // The close.
    let close_params = msg(|m| {
        let mut params = m.init_root::<wire::files::close_file_params::Builder<'_>>();
        params.reborrow().init_handle().init_handle().set_id(handle);
    });
    let _ = super::call_in(app, STEM, INTERFACE, "closeFile", &close_params);
    let text_payload = String::from_utf8_lossy(&payload).to_string();
    let human = format!(
        "  {} {} {}\n{}\n",
        domain::cyan(app, "📄"),
        domain::bold(app, "Cat"),
        domain::bold(app, &path),
        text_payload.trim_end()
    );
    let json_text = {
        let mut out = String::from("{\"data\":");
        let mut o = json::Obj::new(&mut out);
        o.key("bytes").int(payload.len() as u64);
        o.key("text").str(&text_payload);
        o.end();
        out.push('}');
        out
    };
    print_answer(app, json_text, human)
}

// ═════════════════════════════════════════════════════════════════════════
// `files du` — the host's own statvfs answer.
// ═════════════════════════════════════════════════════════════════════════

fn du(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_disk_usage_params::Builder<'_>>();
        params.set_path(&path);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getDiskUsage", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::get_disk_usage_results::Owned, _, _>(&bytes, |results| {
            let usage = results.get_usage().ok();
            (
                {
                    let mut out = String::from("{\"usage\":");
                    let mut o = json::Obj::new(&mut out);
                    match &usage {
                        Some(usage) => {
                            o.key("total").int(usage.get_total());
                            o.key("used").int(usage.get_used());
                            o.key("free").int(usage.get_free());
                            o.key("percentUsed").float(usage.get_percent_used() as f64);
                            o.key("mountPoint").str(&text(usage.get_mount_point()));
                        }
                        None => {
                            o.key("total").int(0);
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                match &usage {
                    Some(usage) => {
                        let mut out = String::new();
                        out.push_str(&format!(
                            "  {} {} {}\n",
                            domain::cyan(app, "💽"),
                            domain::bold(app, "Disk usage"),
                            domain::bold(app, &text(usage.get_mount_point()))
                        ));
                        out.push_str(&format!(
                            "  ├─ Total      {:<12} {}\n",
                            present::human_bytes(usage.get_total()),
                            domain::dim(app, &format!("({} bytes)", usage.get_total()))
                        ));
                        out.push_str(&format!(
                            "  ├─ Used       {:<12} {:.1}%\n",
                            present::human_bytes(usage.get_used()),
                            usage.get_percent_used()
                        ));
                        out.push_str(&format!(
                            "  ├─ Free       {:<12}\n",
                            present::human_bytes(usage.get_free())
                        ));
                        let mount = text(usage.get_mount_point());
                        if !mount.is_empty() {
                            out.push_str(&format!(
                                "  ├─ Mount      {} (mount id {})\n",
                                mount,
                                usage.get_mount_id().map(|id| id.get_id()).unwrap_or(0)
                            ));
                        }
                        out.push_str("  └─ Source     statvfs plus the host's own mount row");
                        out
                    }
                    None => format!(
                        "  {} {} — the host published no usage",
                        domain::cyan(app, "💽"),
                        domain::bold(app, "Disk usage")
                    ),
                },
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// `files locks` — the host's own lock rows.
// ═════════════════════════════════════════════════════════════════════════

fn locks(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_locks_params::Builder<'_>>();
        params.set_path(&path);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getLocks", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::get_locks_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::file_lock::Reader<'_>> = results
                .get_locks()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            (
                {
                    let mut out = String::from("{\"locks\":[");
                    for (index, row) in rows.iter().enumerate() {
                        json::sep(&mut out, index == 0);
                        let mut l = json::Obj::new(&mut out);
                        l.key("lockId").int(row.get_lock_id());
                        l.key("lockType").str(lock_type_token(row.get_lock_type()));
                        l.key("owner").str(&text(row.get_owner()));
                        l.key("acquiredAt")
                            .sub(|out| timestamp(out, &row.get_acquired_at()));
                        l.end();
                    }
                    out.push_str("]}");
                    out
                },
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} — the host's own lock rows\n",
                        domain::cyan(app, "🔒"),
                        domain::bold(app, "Locks"),
                        domain::dim(app, &format!("({})", rows.len()))
                    ));
                    if rows.is_empty() {
                        out.push_str(&format!(
                            "  └─ {} the host holds no lock on {path}\n",
                            domain::dim(app, "○")
                        ));
                    } else {
                        let mut table = present::Table::new(vec![
                            String::from("ID"),
                            String::from("TYPE"),
                            String::from("OWNER"),
                            String::from("ACQUIRED"),
                        ]);
                        for row in &rows {
                            table = table.row(vec![
                                present::Cell::plain(row.get_lock_id().to_string()),
                                present::Cell::plain(lock_type_token(row.get_lock_type())),
                                present::Cell::plain(text(row.get_owner())),
                                present::Cell::plain(stamp_label(&row.get_acquired_at())),
                            ]);
                        }
                        out.push_str(&table.render(app.style()));
                    }
                    out.trim_end().to_string()
                },
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// `files hash` / `files checksum` — the content's own digest.
// ═════════════════════════════════════════════════════════════════════════

fn hash(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let algorithm = domain::flag(args, "--algorithm").unwrap_or_else(|| "sha256".to_string());
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_file_hash_params::Builder<'_>>();
        params.set_path(&path);
        params.set_algorithm(&algorithm);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getFileHash", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::get_file_hash_results::Owned, _, _>(&bytes, |results| {
            let result = results.get_result().ok();
            let digest = result
                .as_ref()
                .and_then(|r| match r.which() {
                    Ok(wire::file_hash_result::Which::Hash(Ok(hash))) => Some(hash_reader(&hash)),
                    _ => None,
                })
                .unwrap_or_default();
            (
                {
                    let mut out = String::from("{\"result\":");
                    let mut o = json::Obj::new(&mut out);
                    o.key("hash").str(&digest);
                    o.end();
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} {} {}",
                    domain::cyan(app, "#️⃣"),
                    domain::bold(app, "Hash"),
                    domain::bold(app, &path),
                    domain::dim(app, &digest)
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn checksum(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_file_checksum_params::Builder<'_>>();
        params.set_path(&path);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getFileChecksum", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::files::get_file_checksum_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let result = results.get_result().ok();
            let digest = result
                .as_ref()
                .map(|r| hash_of(&r.get_stored()))
                .unwrap_or_default();
            let valid = result.as_ref().map(|r| r.get_valid()).unwrap_or(false);
            (
                {
                    let mut out = String::from("{\"result\":");
                    let mut o = json::Obj::new(&mut out);
                    o.key("valid").boolean(valid);
                    o.key("stored").str(&digest);
                    o.end();
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} {} {} {}",
                    domain::cyan(app, "#️⃣"),
                    domain::bold(app, "Checksum"),
                    domain::bold(app, &path),
                    domain::dim(app, &digest),
                    domain::glyph(app, &domain::yes_no(valid))
                ),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// `files versions` / `files version` — the store's own version trail.
// ═════════════════════════════════════════════════════════════════════════

fn versions(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_file_versions_params::Builder<'_>>();
        params.set_path(&path);
        params.reborrow().init_page().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getFileVersions", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::get_file_versions_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::file_version::Reader<'_>> = results
                .get_versions()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            let total = results.get_total();
            (
                {
                    let mut out = versions_json("versions", &rows);
                    out.pop();
                    out.push_str(",\"total\":");
                    out.push_str(&total.to_string());
                    out.push('}');
                    out
                },
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} {}\n",
                        domain::cyan(app, "🕘"),
                        domain::bold(app, "Versions"),
                        domain::bold(app, &path),
                        domain::dim(app, &format!("({})", rows.len()))
                    ));
                    if rows.is_empty() {
                        out.push_str(&format!(
                            "  └─ {} the store records no version for {path} (an honest empty)\n",
                            domain::dim(app, "○")
                        ));
                    } else {
                        let mut table = present::Table::new(vec![
                            String::from("ID"),
                            String::from("SIZE"),
                            String::from("WHEN"),
                            String::from("MESSAGE"),
                        ]);
                        for row in &rows {
                            table = table.row(vec![
                                present::Cell::plain(row.get_version_id().to_string()),
                                present::Cell::plain(present::human_bytes(row.get_size())),
                                present::Cell::plain(stamp_label(&row.get_created_at())),
                                present::truncate_to(&text(row.get_message()), 40).into(),
                            ]);
                        }
                        out.push_str(&table.render(app.style()));
                    }
                    out.trim_end().to_string()
                },
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn version(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let id = domain::number::<u64>(args, "--version", 0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_file_version_params::Builder<'_>>();
        params.set_path(&path);
        params.set_version_id(id);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getFileVersion", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::get_file_version_results::Owned, _, _>(&bytes, |results| {
            let result = results.get_result().ok();
            (
                {
                    let mut out = String::from("{\"result\":");
                    let mut o = json::Obj::new(&mut out);
                    match &result {
                        Some(result) => match result.which() {
                            Ok(wire::version_result::Which::Version(Ok(version))) => {
                                o.key("versionId").int(version.get_version_id());
                                o.key("size").int(version.get_size());
                                o.key("message").str(&text(version.get_message()));
                                o.key("createdAt")
                                    .sub(|out| timestamp(out, &version.get_created_at()));
                            }
                            _ => {
                                o.key("unknown").str("unknown");
                            }
                        },
                        None => {
                            o.key("unknown").str("the service answered no result");
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                match &result {
                    Some(result) => match result.which() {
                        Ok(wire::version_result::Which::Version(Ok(version))) => format!(
                            "  {} {} #{} {} {}",
                            domain::cyan(app, "🕘"),
                            domain::bold(app, "Version"),
                            domain::bold(app, &version.get_version_id().to_string()),
                            domain::bold(app, &text(version.get_message())),
                            domain::dim(app, &format!("{}", present::human_bytes(version.get_size())))
                        ),
                        _ => format!(
                            "  {} {} — no version {id} for {path}",
                            domain::cyan(app, "🕘"),
                            domain::bold(app, "Version")
                        ),
                    },
                    None => format!(
                        "  {} {} — no version {id} for {path}",
                        domain::cyan(app, "🕘"),
                        domain::bold(app, "Version")
                    ),
                },
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// `files branches` / `files branch` — the version branches.
// ═════════════════════════════════════════════════════════════════════════

fn branches(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::list_branches_params::Builder<'_>>();
        params.set_path(&path);
        params.reborrow().init_page().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "listBranches", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::list_branches_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::file_branch::Reader<'_>> = results
                .get_branches()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            let total = results.get_total();
            (
                {
                    let mut out = branches_json("branches", &rows);
                    out.pop();
                    out.push_str(",\"total\":");
                    out.push_str(&total.to_string());
                    out.push('}');
                    out
                },
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} {}\n",
                        domain::cyan(app, "🌿"),
                        domain::bold(app, "Branches"),
                        domain::bold(app, &path),
                        domain::dim(app, &format!("({})", rows.len()))
                    ));
                    if rows.is_empty() {
                        out.push_str(&format!(
                            "  └─ {} the store holds no branch for {path} (an honest empty)\n",
                            domain::dim(app, "○")
                        ));
                    } else {
                        let mut table = present::Table::new(vec![
                            String::from("BRANCH"),
                            String::from("BASE"),
                            String::from("WHEN"),
                            String::from("AUTHOR"),
                        ]);
                        for row in &rows {
                            table = table.row(vec![
                                present::Cell::plain(text(row.get_branch_name())),
                                present::Cell::plain(row.get_base_version().to_string()),
                                present::Cell::plain(stamp_label(&row.get_created_at())),
                                present::Cell::plain(text(row.get_author())),
                            ]);
                        }
                        out.push_str(&table.render(app.style()));
                    }
                    out.trim_end().to_string()
                },
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn branch(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let id = domain::number::<u64>(args, "--version", 0);
    let name = domain::flag(args, "--label").unwrap_or_default();
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::branch_version_params::Builder<'_>>();
        params.set_path(&path);
        params.set_version_id(id);
        params.set_branch_name(&name);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "branchVersion", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::branch_version_results::Owned, _, _>(&bytes, |results| {
            let result = results.get_result().ok();
            (
                {
                    let mut out = String::from("{\"result\":");
                    let mut o = json::Obj::new(&mut out);
                    match &result {
                        Some(result) => match result.which() {
                            Ok(wire::branch_result::Which::Branch(Ok(branch))) => {
                                o.key("branchId").str(&text(branch.get_branch_id()));
                                o.key("branchName").str(&text(branch.get_branch_name()));
                                o.key("path").str(&text(branch.get_path()));
                            }
                            _ => {
                                o.key("unknown").str("unknown");
                            }
                        },
                        None => {
                            o.key("unknown").str("the service answered no result");
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} {} {}",
                    domain::green(app, "✎"),
                    domain::bold(app, "Branch"),
                    domain::bold(app, &name),
                    domain::dim(app, "— the engine's own host-mutating path, accepted")
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// `files store` / `files by-hash` / `files dedupe` — the content-addressed
// store.
// ═════════════════════════════════════════════════════════════════════════

fn store(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::store_content_addressed_params::Builder<'_>>();
        params.set_path(&path);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "storeContentAddressed", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::files::store_content_addressed_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let result = results.get_result().ok();
            (
                content_ref_json(&mut {
                    let mut out = String::from("{\"result\":");
                    out
                }, &result),
                content_ref_human(app, &result, "Store", "🧬"),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn by_hash(app: &mut App, args: &[String], _path: &str) -> i32 {
    let hash = domain::flag(args, "--hash")
        .or_else(|| domain::positionals(args).first().map(|h| (*h).to_string()))
        .unwrap_or_default();
    let (algorithm, digest) = match hash.split_once(':') {
        Some((algorithm, digest)) => (algorithm.to_string(), digest.to_string()),
        None => (String::new(), hash.clone()),
    };
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_by_hash_params::Builder<'_>>();
        {
            let mut request = params.reborrow().init_request();
            request.set_algorithm(&algorithm);
            request.reborrow().init_hash().set_bytes(&decode_hex(&digest));
        }
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getByHash", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::files::get_by_hash_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let result = results.get_result().ok();
            (
                content_ref_json(&mut {
                    let mut out = String::from("{\"result\":");
                    out
                }, &result),
                content_ref_human(app, &result, "By hash", "🧬"),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn dedupe(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::deduplicate_params::Builder<'_>>();
        params.set_path(&path);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "deduplicate", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::deduplicate_results::Owned, _, _>(&bytes, |results| {
            let saved = results.get_saved_bytes();
            let references = results.get_reference_count();
            (
                format!("{{\"savedBytes\":{saved},\"referenceCount\":{references}}}"),
                format!(
                    "  {} {} {} — {} saved, {} reference(s)",
                    domain::green(app, "✎"),
                    domain::bold(app, "Deduplicate"),
                    domain::bold(app, &path),
                    present::human_bytes(saved),
                    references
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn dedupe_stats(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_deduplication_stats_params::Builder<'_>>();
        params.set_path(&path);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getDeduplicationStats", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::files::get_deduplication_stats_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let stats = results.get_stats().ok();
            (
                {
                    let mut out = String::from("{\"stats\":");
                    let mut o = json::Obj::new(&mut out);
                    match &stats {
                        Some(stats) => {
                            o.key("contentHash").str(&hash_of(&stats.get_content_hash()));
                            o.key("referenceCount").int(stats.get_reference_count());
                            o.key("savedBytes").int(stats.get_saved_bytes());
                        }
                        None => {
                            o.key("referenceCount").int(0);
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                match &stats {
                    Some(stats) => format!(
                        "  {} {} {} — {} reference(s) · {} saved",
                        domain::cyan(app, "🧬"),
                        domain::bold(app, "Deduplication"),
                        domain::bold(app, &path),
                        stats.get_reference_count(),
                        present::human_bytes(stats.get_saved_bytes())
                    ),
                    None => format!(
                        "  {} {} — the host published no statistics",
                        domain::cyan(app, "🧬"),
                        domain::bold(app, "Deduplication")
                    ),
                },
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// `files watch` — the contract's own subscription.
// ═════════════════════════════════════════════════════════════════════════

fn watch(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let count = domain::number::<u32>(args, "--count", 1);
    let interval = domain::number::<u64>(args, "--interval", 400);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::watch_directory_params::Builder<'_>>();
        params.set_path(&path);
    });
    let (interface_id, method_id) = match super::ids_in(STEM, INTERFACE, "watchDirectory") {
        Some(ids) => ids,
        None => {
            super::usage_error_app(app, "the contract has no `watchDirectory` method in this build");
            return crate::registry::EXIT_USAGE;
        }
    };
    let reader = match tp::decode_message::<wire::files::watch_directory_params::Owned>(&params) {
        Ok(reader) => reader,
        Err(error) => {
            super::usage_error_app(app, &format!("{error}"));
            return crate::registry::EXIT_USAGE;
        }
    };
    let call_params = match reader.get() {
        Ok(params) => params,
        Err(error) => {
            super::usage_error_app(app, &format!("{error}"));
            return crate::registry::EXIT_USAGE;
        }
    };
    let response = match app.conn().call_typed::<
        wire::files::watch_directory_params::Owned,
        wire::files::watch_directory_results::Owned,
    >(interface_id, method_id, call_params) {
        Ok(response) => response,
        Err(error) => return super::emit_error(app, &error),
    };
    let results = match response.get() {
        Ok(results) => results,
        Err(error) => {
            return super::emit_error(app, &crate::transport::CallError::Transport(format!("{error}")))
        }
    };
    let capability = match results.get_subscription() {
        Ok(capability) => capability,
        Err(error) => {
            return super::emit_error(app, &crate::transport::CallError::Transport(format!("{error}")))
        }
    };
    let subscription = crate::transport::Subscription::new(
        capability.client,
        "files",
        "FileEventSubscription",
    );
    let json_mode = app.json();
    let quiet = app.quiet();
    let mut delivered = 0u32;
    let mut idle = 0u32;
    while delivered < count.max(1) && idle < 3 {
        let payload = match subscription.pull(app.conn()) {
            Ok(payload) => payload,
            Err(error) => {
                let _ = subscription.cancel(app.conn());
                return super::emit_error(app, &error);
            }
        };
        let batch = tp::with_root::<wire::file_event_subscription::pull_results::Owned, _, _>(
            &payload,
            |r| {
                let mut count = 0u32;
                if let Ok(list) = r.get_events() {
                    for event in list.iter() {
                        let kind = file_event_token(event.get_type());
                        if json_mode {
                            let mut out = String::new();
                            let mut o = json::Obj::new(&mut out);
                            o.key("type").str(kind);
                            o.key("path").str(&text(event.get_path()));
                            o.key("oldPath").str(&text(event.get_old_path()));
                            o.key("timestamp")
                                .int(event.get_timestamp().map(|t| t.get_nanoseconds()).unwrap_or(0));
                            o.end();
                            println!("{out}");
                        } else if !quiet {
                            println!(
                                "  {} {:<12} {}",
                                domain::green(app, "◉"),
                                kind,
                                text(event.get_path())
                            );
                        }
                        count += 1;
                    }
                }
                count
            },
        )
        .unwrap_or(0);
        if batch == 0 {
            idle += 1;
            crate::watch::sleep_ms(interval);
            continue;
        }
        delivered += batch;
        crate::watch::sleep_ms(interval);
    }
    let _ = subscription.cancel(app.conn());
    if json_mode {
        let mut out = String::from("{\"summary\":");
        let mut o = json::Obj::new(&mut out);
        o.key("delivered").int(delivered as u64);
        o.key("idle").int(idle as u64);
        o.end();
        println!("{out}");
    } else if !quiet {
        eprintln!("watch: {} event(s) delivered, {} idle poll(s)", delivered, idle);
    }
    crate::registry::EXIT_OK
}

// ═════════════════════════════════════════════════════════════════════════
// `files special` / `files trash` / `files recents` / `files app`.
// ═════════════════════════════════════════════════════════════════════════

fn special(app: &mut App, args: &[String], _path: &str) -> i32 {
    let token = domain::flag(args, "--type")
        .or_else(|| domain::positionals(args).first().map(|t| (*t).to_string()))
        .unwrap_or_else(|| "home".to_string());
    let kind = special_path_of(&token);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_special_path_params::Builder<'_>>();
        params.set_type(kind);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getSpecialPath", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::get_special_path_results::Owned, _, _>(&bytes, |results| {
            let result = results.get_result().ok();
            let path = result
                .as_ref()
                .and_then(|r| match r.which() {
                    Ok(wire::text_result::Which::Value(Ok(value))) => Some(text(Ok(value))),
                    _ => None,
                })
                .unwrap_or_default();
            (
                {
                    let mut out = String::from("{\"result\":");
                    let mut o = json::Obj::new(&mut out);
                    o.key("value").str(&path);
                    o.end();
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} {} {}",
                    domain::cyan(app, "📌"),
                    domain::bold(app, "Special path"),
                    domain::bold(app, &token),
                    domain::bold(app, &path)
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn trash(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::list_trash_params::Builder<'_>>();
        params.reborrow().init_page().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "listTrash", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::list_trash_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::trash_entry::Reader<'_>> = results
                .get_entries()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            let total = results.get_total();
            (
                {
                    let mut out = String::from("{\"entries\":[");
                    for (index, row) in rows.iter().enumerate() {
                        json::sep(&mut out, index == 0);
                        let mut e = json::Obj::new(&mut out);
                        e.key("entryId").str(&text(row.get_entry_id()));
                        e.key("originalPath").str(&text(row.get_original_path()));
                        e.key("size").int(row.get_size());
                        e.key("deletedAt")
                            .sub(|out| timestamp(out, &row.get_deleted_at()));
                        e.end();
                    }
                    out.push_str("],\"total\":");
                    out.push_str(&total.to_string());
                    out.push('}');
                    out
                },
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} — the host's own trash\n",
                        domain::cyan(app, "🗑"),
                        domain::bold(app, "Trash"),
                        domain::dim(app, &format!("({})", rows.len()))
                    ));
                    if rows.is_empty() {
                        out.push_str(&format!(
                            "  └─ {} the host's trash is empty (an honest empty — it was read)\n",
                            domain::dim(app, "○")
                        ));
                    } else {
                        let mut table = present::Table::new(vec![
                            String::from("ID"),
                            String::from("ORIGINAL PATH"),
                            String::from("SIZE"),
                            String::from("DELETED"),
                        ]);
                        for row in &rows {
                            table = table.row(vec![
                                present::truncate_to(&text(row.get_entry_id()), 16).into(),
                                present::Cell::path(&text(row.get_original_path())),
                                present::Cell::plain(present::human_bytes(row.get_size())),
                                present::Cell::plain(stamp_label(&row.get_deleted_at())),
                            ]);
                        }
                        out.push_str(&table.render(app.style()));
                    }
                    out.trim_end().to_string()
                },
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn recents(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_recent_files_params::Builder<'_>>();
        params.reborrow().init_page().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getRecentFiles", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::get_recent_files_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::recent_file::Reader<'_>> = results
                .get_files()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            let total = results.get_total();
            (
                {
                    let mut out = String::from("{\"files\":[");
                    for (index, row) in rows.iter().enumerate() {
                        json::sep(&mut out, index == 0);
                        let mut f = json::Obj::new(&mut out);
                        f.key("path").str(&text(row.get_path()));
                        f.key("mimeType").str(&mime_of(&row.get_mime_type()));
                        f.key("timestamp")
                            .sub(|out| timestamp(out, &row.get_timestamp()));
                        f.end();
                    }
                    out.push_str("],\"total\":");
                    out.push_str(&total.to_string());
                    out.push('}');
                    out
                },
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} — the host's own recent files\n",
                        domain::cyan(app, "🕒"),
                        domain::bold(app, "Recent"),
                        domain::dim(app, &format!("({})", rows.len()))
                    ));
                    if rows.is_empty() {
                        out.push_str(&format!(
                            "  └─ {} the host records no recent file (an honest empty — it was read)\n",
                            domain::dim(app, "○")
                        ));
                        out.trim_end().to_string()
                    } else {
                        let mut table = present::Table::new(vec![
                            String::from("PATH"),
                            String::from("TYPE"),
                            String::from("WHEN"),
                        ]);
                        for row in &rows {
                            table = table.row(vec![
                                present::Cell::path(&text(row.get_path())),
                                present::Cell::plain(mime_of(&row.get_mime_type())),
                                present::Cell::plain(stamp_label(&row.get_timestamp())),
                            ]);
                        }
                        out.push_str(&table.render(app.style()));
                        out.trim_end().to_string()
                    }
                },
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn app(app: &mut App, args: &[String], _path: &str) -> i32 {
    let mime = domain::flag(args, "--mime")
        .or_else(|| domain::positionals(args).first().map(|m| (*m).to_string()))
        .unwrap_or_else(|| "text/plain".to_string());
    let (kind, subtype) = match mime.split_once('/') {
        Some((kind, subtype)) => (kind.to_string(), subtype.to_string()),
        None => (mime.clone(), "plain".to_string()),
    };
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_default_app_params::Builder<'_>>();
        {
            let mut mime_type = params.reborrow().init_mime_type();
            mime_type.set_type(&kind);
            mime_type.set_subtype(&subtype);
        }
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getDefaultApp", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::get_default_app_results::Owned, _, _>(&bytes, |results| {
            let result = results.get_result().ok();
            let name = result
                .as_ref()
                .and_then(|r| match r.which() {
                    Ok(wire::default_app_result::Which::App(Ok(app))) => Some(text(app.get_name())),
                    _ => None,
                })
                .unwrap_or_default();
            (
                {
                    let mut out = String::from("{\"result\":");
                    let mut o = json::Obj::new(&mut out);
                    o.key("app").str(&name);
                    o.end();
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} {} {}",
                    domain::cyan(app, "📦"),
                    domain::bold(app, "Default app"),
                    domain::bold(app, &format!("{kind}/{subtype}")),
                    domain::bold(app, &name)
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// `files grep` — the content search.
// ═════════════════════════════════════════════════════════════════════════

fn grep(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let query = domain::flag(args, "--pattern")
        .or_else(|| domain::positionals(args).get(1).map(|q| (*q).to_string()))
        .unwrap_or_default();
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::search_file_content_params::Builder<'_>>();
        let mut search = params.reborrow().init_query();
        search.set_path(&path);
        search.set_query(&query);
        search.set_recursive(true);
        search.reborrow().init_page().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "searchFileContent", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::files::search_file_content_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let rows: Vec<wire::content_search_result::Reader<'_>> = results
                .get_results()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            let total = results.get_total();
            (
                {
                    let mut out = String::from("{\"results\":[");
                    for (index, row) in rows.iter().enumerate() {
                        json::sep(&mut out, index == 0);
                        let mut r = json::Obj::new(&mut out);
                        r.key("path").str(&text(row.get_path()));
                        r.key("lineNumber").int(row.get_line_number());
                        r.key("snippet").str(&text(row.get_snippet()));
                        r.key("score").float(row.get_score() as f64);
                        r.end();
                    }
                    out.push_str("],\"total\":");
                    out.push_str(&total.to_string());
                    out.push('}');
                    out
                },
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} {}\n",
                        domain::cyan(app, "🔎"),
                        domain::bold(app, "Grep"),
                        domain::bold(app, &query),
                        domain::dim(app, &format!("under {path} — the contract's own content search"))
                    ));
                    if rows.is_empty() {
                        out.push_str(&format!(
                            "  └─ {} no line under {path} matches (an honest empty — the search \
                             ran)\n",
                            domain::dim(app, "○")
                        ));
                        out.trim_end().to_string()
                    } else {
                        let mut table = present::Table::new(vec![
                            String::new(),
                            String::from("PATH"),
                            String::from("LINE"),
                            String::from("TEXT"),
                        ]);
                        for row in &rows {
                            table = table.row(vec![
                                present::Cell::styled("◉", present::Sgr::Green),
                                present::Cell::path(&text(row.get_path())),
                                present::Cell::plain(row.get_line_number().to_string()),
                                present::truncate_to(&text(row.get_snippet()), 48).into(),
                            ]);
                        }
                        out.push_str(&table.render(app.style()));
                        out.trim_end().to_string()
                    }
                },
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// `files compare` — the two paths, compared.
// ═════════════════════════════════════════════════════════════════════════

fn compare(app: &mut App, args: &[String], _path: &str) -> i32 {
    let positionals = domain::positionals(args);
    let left = positionals
        .first()
        .map(|p| (*p).to_string())
        .or_else(|| domain::flag(args, "--path"))
        .unwrap_or_default();
    let right = positionals
        .get(1)
        .map(|p| (*p).to_string())
        .or_else(|| domain::flag(args, "--to"))
        .unwrap_or_default();
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::compare_files_params::Builder<'_>>();
        params.set_path1(&left);
        params.set_path2(&right);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "compareFiles", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::compare_files_results::Owned, _, _>(&bytes, |results| {
            let result = results.get_result().ok();
            let identical = result
                .as_ref()
                .map(|r| r.get_identical())
                .unwrap_or(false);
            let reason = result
                .as_ref()
                .and_then(|r| r.get_error().ok())
                .map(|e| text(e.get_message()))
                .unwrap_or_default();
            (
                {
                    let mut out = String::from("{\"result\":");
                    let mut o = json::Obj::new(&mut out);
                    o.key("identical").boolean(identical);
                    if !reason.is_empty() {
                        o.key("error").str(&reason);
                    }
                    o.end();
                    out.push('}');
                    out
                },
                if identical {
                    format!(
                        "  {} {} {} ◉ identical — byte for byte",
                        domain::cyan(app, "🆚"),
                        domain::bold(app, "Compare"),
                        domain::bold(app, &format!("{left} ↔ {right}"))
                    )
                } else {
                    format!(
                        "  {} {} {} ✗ different",
                        domain::cyan(app, "🆚"),
                        domain::bold(app, "Compare"),
                        domain::bold(app, &format!("{left} ↔ {right}"))
                    )
                },
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// `files health` / `files quota` — the filesystem's own verdicts.
// ═════════════════════════════════════════════════════════════════════════

fn health(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_filesystem_health_params::Builder<'_>>();
        params.set_path(&path);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getFilesystemHealth", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::files::get_filesystem_health_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let result = results.get_result().ok();
            let health = result.as_ref().and_then(|r| match r.which() {
                Ok(wire::filesystem_health_result::Which::Health(Ok(health))) => Some(health),
                _ => None,
            });
            (
                {
                    let mut out = String::from("{\"result\":");
                    let mut o = json::Obj::new(&mut out);
                    match &health {
                        Some(health) => {
                            o.key("status").str(&text(health.get_status()));
                            o.key("errors").int(health.get_errors());
                            o.key("warnings").int(health.get_warnings());
                            o.key("details").str(&text(health.get_details()));
                        }
                        None => {
                            o.key("status").str("");
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                match &health {
                    Some(health) => {
                        let mut out = String::new();
                        out.push_str(&format!(
                            "  {} {} {}\n",
                            domain::cyan(app, "🩺"),
                            domain::bold(app, "Filesystem health"),
                            domain::bold(app, &path)
                        ));
                        out.push_str(&format!(
                            "  ├─ Status     {}\n",
                            domain::bold(app, &text(health.get_status()))
                        ));
                        out.push_str(&format!(
                            "  ├─ Errors     {} · warnings {}\n",
                            health.get_errors(),
                            health.get_warnings()
                        ));
                        out.push_str(&format!(
                            "  ├─ Details    {}\n",
                            present::truncate_to(&text(health.get_details()), 48)
                        ));
                        out.push_str("  └─ Source     the filesystem's own answer, measured");
                        out
                    }
                    None => format!(
                        "  {} {} {} — the host published no verdict",
                        domain::cyan(app, "🩺"),
                        domain::bold(app, "Filesystem health"),
                        domain::bold(app, &path)
                    ),
                },
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn quota(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_quota_params::Builder<'_>>();
        params.set_path(&path);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getQuota", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::get_quota_results::Owned, _, _>(&bytes, |results| {
            let quota = results.get_quota().ok();
            (
                {
                    let mut out = String::from("{\"quota\":");
                    let mut o = json::Obj::new(&mut out);
                    match &quota {
                        Some(quota) => {
                            o.key("maxSizeBytes").int(quota.get_max_size_bytes());
                            o.key("maxFiles").int(quota.get_max_files());
                            o.key("currentSizeBytes").int(quota.get_current_size_bytes());
                            o.key("currentFiles").int(quota.get_current_files());
                            o.key("softLimitPercent").int(quota.get_soft_limit_percent());
                        }
                        None => {
                            o.key("maxSizeBytes").int(0);
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                match &quota {
                    Some(quota) => {
                        let mut out = String::new();
                        out.push_str(&format!(
                            "  {} {} {}\n",
                            domain::cyan(app, "📊"),
                            domain::bold(app, "Quota"),
                            domain::bold(app, &path)
                        ));
                        out.push_str(&format!(
                            "  ├─ Size       {} of {}\n",
                            present::human_bytes(quota.get_current_size_bytes()),
                            present::human_bytes(quota.get_max_size_bytes())
                        ));
                        out.push_str(&format!(
                            "  ├─ Files      {} of {}\n",
                            quota.get_current_files(),
                            quota.get_max_files()
                        ));
                        out.push_str(&format!(
                            "  └─ Soft limit {}%",
                            quota.get_soft_limit_percent()
                        ));
                        out
                    }
                    None => format!(
                        "  {} {} {} — the host published no quota",
                        domain::cyan(app, "📊"),
                        domain::bold(app, "Quota"),
                        domain::bold(app, &path)
                    ),
                },
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// The smaller readings: size, stats, activity, lifecycle, compression,
// encryption, analyze, snapshot, pwd, exists, realpath, open, acquire.
// ═════════════════════════════════════════════════════════════════════════

fn size(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_directory_size_params::Builder<'_>>();
        params.set_path(&path);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getDirectorySize", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::files::get_directory_size_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let size = results.get_size();
            (
                format!("{{\"size\":{size}}}"),
                format!(
                    "  {} {} {} {}",
                    domain::cyan(app, "📏"),
                    domain::bold(app, "Size"),
                    domain::bold(app, &path),
                    domain::bold(app, &present::human_bytes(size))
                ),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn stats(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_directory_stats_params::Builder<'_>>();
        params.set_path(&path);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getDirectoryStats", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::get_directory_stats_results::Owned, _, _>(&bytes, |results| {
            let stats = results.get_stats().ok();
            (
                {
                    let mut out = String::from("{\"stats\":");
                    let mut o = json::Obj::new(&mut out);
                    match &stats {
                        Some(stats) => {
                            o.key("totalFiles").int(stats.get_total_files());
                            o.key("totalDirectories").int(stats.get_total_directories());
                            o.key("totalSize").int(stats.get_total_size());
                            o.key("largestFile").str(&text(stats.get_largest_file()));
                            o.key("oldestFile").str(&text(stats.get_oldest_file()));
                            o.key("newestFile").str(&text(stats.get_newest_file()));
                        }
                        None => {
                            o.key("totalFiles").int(0);
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                match &stats {
                    Some(stats) => format!(
                        "  {} {} {} — {} file(s) · {} dir(s) · {}",
                        domain::cyan(app, "📊"),
                        domain::bold(app, "Directory stats"),
                        domain::bold(app, &path),
                        stats.get_total_files(),
                        stats.get_total_directories(),
                        present::human_bytes(stats.get_total_size())
                    ),
                    None => format!(
                        "  {} {} {} — the host published no statistics",
                        domain::cyan(app, "📊"),
                        domain::bold(app, "Directory stats"),
                        domain::bold(app, &path)
                    ),
                },
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn activity(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_file_activity_params::Builder<'_>>();
        params.set_path(&path);
        params.reborrow().init_page().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getFileActivity", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::get_file_activity_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::file_activity::Reader<'_>> = results
                .get_activities()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            let total = results.get_total();
            (
                {
                    let mut out = String::from("{\"activities\":[");
                    for (index, row) in rows.iter().enumerate() {
                        json::sep(&mut out, index == 0);
                        let mut a = json::Obj::new(&mut out);
                        a.key("path").str(&text(row.get_path()));
                        a.key("action").str(&text(row.get_action()));
                        a.key("actor").str(&text(row.get_actor()));
                        a.key("timestamp")
                            .sub(|out| timestamp(out, &row.get_timestamp()));
                        a.end();
                    }
                    out.push_str("],\"total\":");
                    out.push_str(&total.to_string());
                    out.push('}');
                    out
                },
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} {}\n",
                        domain::cyan(app, "🕘"),
                        domain::bold(app, "Activity"),
                        domain::bold(app, &path),
                        domain::dim(app, &format!("({})", rows.len()))
                    ));
                    if rows.is_empty() {
                        out.push_str(&format!(
                            "  └─ {} the host records no activity for {path} (an honest empty)\n",
                            domain::dim(app, "○")
                        ));
                        out.trim_end().to_string()
                    } else {
                        let mut table = present::Table::new(vec![
                            String::from("PATH"),
                            String::from("OPERATION"),
                            String::from("WHEN"),
                        ]);
                        for row in &rows {
                            table = table.row(vec![
                                present::Cell::path(&text(row.get_path())),
                                present::Cell::plain(text(row.get_action())),
                                present::Cell::plain(stamp_label(&row.get_timestamp())),
                            ]);
                        }
                        out.push_str(&table.render(app.style()));
                        out.trim_end().to_string()
                    }
                },
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn lifecycle(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_lifecycle_params::Builder<'_>>();
        params.set_path(&path);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getLifecycle", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::get_lifecycle_results::Owned, _, _>(&bytes, |results| {
            let result = results.get_result().ok();
            (
                {
                    let mut out = String::from("{\"result\":");
                    let mut o = json::Obj::new(&mut out);
                    match &result {
                        Some(result) => match result.which() {
                            Ok(wire::lifecycle_policy_result::Which::Policy(Ok(policy))) => {
                                o.key("path").str(&text(policy.get_path()));
                                o.key("action").str(retention_token(policy.get_action()));
                                o.key("enabled").boolean(policy.get_enabled());
                                o.key("description").str(&text(policy.get_description()));
                            }
                            _ => {
                                o.key("unknown").str("unknown");
                            }
                        },
                        None => {
                            o.key("unknown").str("the service answered no result");
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} {}",
                    domain::cyan(app, "⏳"),
                    domain::bold(app, "Lifecycle"),
                    domain::bold(app, &path)
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn compression(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_compression_params::Builder<'_>>();
        params.set_path(&path);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getCompression", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::get_compression_results::Owned, _, _>(&bytes, |results| {
            let config = results.get_config().ok();
            (
                {
                    let mut out = String::from("{\"config\":");
                    let mut o = json::Obj::new(&mut out);
                    match &config {
                        Some(config) => {
                            o.key("algorithm").str(&text(config.get_algorithm()));
                            o.key("level").int(config.get_level());
                        }
                        None => {
                            o.key("algorithm").str("");
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                match &config {
                    Some(config) => format!(
                        "  {} {} {} — {} level {}",
                        domain::cyan(app, "🗜"),
                        domain::bold(app, "Compression"),
                        domain::bold(app, &path),
                        text(config.get_algorithm()),
                        config.get_level()
                    ),
                    None => format!(
                        "  {} {} {} — the host published no compression",
                        domain::cyan(app, "🗜"),
                        domain::bold(app, "Compression"),
                        domain::bold(app, &path)
                    ),
                },
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn encryption_info(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::get_encryption_info_params::Builder<'_>>();
        params.set_path(&path);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getEncryptionInfo", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::files::get_encryption_info_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let result = results.get_result().ok();
            (
                {
                    let mut out = String::from("{\"result\":");
                    let mut o = json::Obj::new(&mut out);
                    match &result {
                        Some(result) => match result.which() {
                            Ok(wire::encryption_info_result::Which::Info(Ok(info))) => {
                                o.key("keyId")
                                    .int(info.get_key_id().map(|id| id.get_id()).unwrap_or(0));
                                o.key("algorithm").str(&text(info.get_algorithm()));
                                o.key("encryptedSize").int(info.get_encrypted_size());
                            }
                            _ => {
                                o.key("encryptedSize").int(0);
                            }
                        },
                        None => {
                            o.key("encrypted").boolean(false);
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} {}",
                    domain::cyan(app, "🔐"),
                    domain::bold(app, "Encryption"),
                    domain::bold(app, &path)
                ),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn analyze(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::analyze_file_content_params::Builder<'_>>();
        params.reborrow().init_request().set_path(&path);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "analyzeFileContent", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::files::analyze_file_content_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let result = results.get_result().ok();
            (
                {
                    let mut out = String::from("{\"result\":");
                    let mut o = json::Obj::new(&mut out);
                    match &result {
                        Some(result) => {
                            o.key("path").str(&text(result.get_path()));
                            o.key("analysisType").str(&text(result.get_analysis_type()));
                            o.key("confidence").float(result.get_confidence() as f64);
                            o.key("timestamp")
                                .sub(|out| timestamp(out, &result.get_timestamp()));
                        }
                        None => {
                            o.key("unknown").str("the service answered no result");
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} {}",
                    domain::cyan(app, "🔬"),
                    domain::bold(app, "Analyze"),
                    domain::bold(app, &path)
                ),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn snapshot(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let name = domain::flag(args, "--label").unwrap_or_default();
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::create_snapshot_params::Builder<'_>>();
        params.set_path(&path);
        params.set_name(&name);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "createSnapshot", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::files::create_snapshot_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let result = results.get_result().ok();
            let reference = result
                .as_ref()
                .and_then(|r| match r.which() {
                    Ok(wire::snapshot_ref_result::Which::SnapshotId(Ok(id))) => {
                        Some(id.get_id().to_string())
                    }
                    _ => None,
                })
                .unwrap_or_default();
            (
                {
                    let mut out = String::from("{\"result\":");
                    let mut o = json::Obj::new(&mut out);
                    o.key("snapshotId").str(&reference);
                    o.end();
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} {} {}",
                    domain::green(app, "✎"),
                    domain::bold(app, "Snapshot"),
                    domain::bold(app, &name),
                    domain::dim(app, &reference)
                ),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn pwd(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| {
        let _ = m.init_root::<wire::files::get_current_directory_params::Builder<'_>>();
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getCurrentDirectory", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::files::get_current_directory_results::Owned,
        _,
        _,
    >(&bytes, |results| {
        let path = text(results.get_path());
        (
            format!("{{\"path\":\"{path}\"}}"),
            format!(
                "  {} {} {}",
                domain::cyan(app, "📁"),
                domain::bold(app, "Working directory"),
                domain::bold(app, &path)
            ),
        )
    }) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn exists(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::file_exists_params::Builder<'_>>();
        params.set_path(&path);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "fileExists", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::file_exists_results::Owned, _, _>(&bytes, |results| {
            let exists = results.get_exists();
            (
                format!("{{\"exists\":{exists}}}"),
                format!(
                    "  {} {} {} {}",
                    domain::cyan(app, "❓"),
                    domain::bold(app, "Exists"),
                    domain::bold(app, &path),
                    domain::glyph(app, &domain::yes_no(exists))
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn realpath(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::resolve_symlink_params::Builder<'_>>();
        params.set_path(&path);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "resolveSymlink", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::resolve_symlink_results::Owned, _, _>(&bytes, |results| {
            let result = results.get_result().ok();
            let resolved = result
                .as_ref()
                .and_then(|r| match r.which() {
                    Ok(wire::text_result::Which::Value(Ok(value))) => Some(text(Ok(value))),
                    _ => None,
                })
                .unwrap_or_default();
            (
                {
                    let mut out = String::from("{\"result\":");
                    let mut o = json::Obj::new(&mut out);
                    o.key("value").str(&resolved);
                    o.end();
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} {} {}",
                    domain::cyan(app, "🔗"),
                    domain::bold(app, "Real path"),
                    domain::bold(app, &path),
                    domain::bold(app, &resolved)
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn open(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::open_file_params::Builder<'_>>();
        params.set_path(&path);
        params.reborrow().init_request().set_mode(OpenMode::Read);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "openFile", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::open_file_results::Owned, _, _>(&bytes, |results| {
            let result = results.get_result().ok();
            let handle = result.as_ref().and_then(|r| match r.which() {
                Ok(wire::open_result::Which::Handle(handle)) => handle
                    .ok()
                    .and_then(|handle| handle.get_handle().ok().map(|id| id.get_id())),
                _ => None,
            });
            (
                {
                    let mut out = String::from("{\"result\":");
                    let mut o = json::Obj::new(&mut out);
                    o.key("handle")
                        .int(handle.unwrap_or(0));
                    o.end();
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} {} {}",
                    domain::cyan(app, "📂"),
                    domain::bold(app, "Open"),
                    domain::bold(app, &path),
                    domain::dim(app, &format!("handle {}", handle.unwrap_or(0)))
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

fn acquire(app: &mut App, args: &[String], _path: &str) -> i32 {
    let path = path_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::files::acquire_file_params::Builder<'_>>();
        params.set_path(&path);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "acquireFile", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::acquire_file_results::Owned, _, _>(&bytes, |results| {
            let acquired = results.get_service().is_ok();
            (
                format!("{{\"acquired\":{acquired}}}"),
                format!(
                    "  {} {} {} {}",
                    domain::cyan(app, "🔑"),
                    domain::bold(app, "Acquire"),
                    domain::bold(app, &path),
                    if acquired {
                        domain::green(app, "◉ the file service is acquired — holding it IS the authority")
                    } else {
                        domain::red(app, "✗ the service answered no capability")
                    }
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// The gated writes: the CLI issues the call; the SERVICE decides.
// ═════════════════════════════════════════════════════════════════════════

/// A gated write: the CLI builds the CONTRACT'S params and issues the call;
/// the SERVICE decides (its capability class refuses a read-only connection).
///
/// Each write carries its own method, so the params are the ones the contract
/// declares for that call — never a stand-in built for another one.
macro_rules! gated_void {
    ($fn_name:ident, $method:literal, $params_ty:ident, |$p:ident, $t:ident| $body:block) => {
        fn $fn_name(app: &mut App, args: &[String], path: &str) -> i32 {
            let $t = path_of(args);
            let params = msg(|m| {
                let mut $p = m.init_root::<wire::files::$params_ty::Builder<'_>>();
                $body
            });
            let bytes = match super::call_in(app, STEM, INTERFACE, $method, &params) {
                Ok(bytes) => bytes,
                Err(error) => return super::emit_error(app, &error),
            };
            let verb = path.rsplit(' ').next().unwrap_or(path);
            void_verb(app, verb, &$t, &bytes)
        }
    };
}

gated_void!(write_create_directory, "createDirectory", create_directory_params, |p, t| {
    p.set_path(&t);
});
gated_void!(write_touch, "touch", touch_params, |p, t| {
    p.set_path(&t);
});
gated_void!(write_remove_file, "removeFile", remove_file_params, |p, t| {
    p.set_path(&t);
});
gated_void!(write_remove_directory, "removeDirectory", remove_directory_params, |p, t| {
    p.set_path(&t);
});
gated_void!(write_rename_file, "renameFile", rename_file_params, |p, t| {
    p.set_old_path(&t);
    p.set_new_path(&t);
});
gated_void!(write_copy_file, "copyFile", copy_file_params, |p, t| {
    p.set_src(&t);
    p.set_dst(&t);
});
gated_void!(write_create_symlink, "createSymlink", create_symlink_params, |p, t| {
    p.set_target(&t);
    p.set_link(&t);
});
gated_void!(write_link_file, "linkFile", link_file_params, |p, t| {
    p.set_target(&t);
    p.set_link(&t);
});
gated_void!(write_unlock_file, "unlockFile", unlock_file_params, |p, _t| {
    p.set_lock_id(0);
});
gated_void!(write_change_directory, "changeDirectory", change_directory_params, |p, t| {
    p.set_path(&t);
});
gated_void!(write_restore_version, "restoreVersion", restore_version_params, |p, t| {
    p.set_path(&t);
});
gated_void!(write_decrypt_file, "decryptFile", decrypt_file_params, |p, t| {
    p.set_path(&t);
});
gated_void!(write_set_compression, "setCompression", set_compression_params, |p, t| {
    p.set_path(&t);
});
gated_void!(write_merge_branch, "mergeBranch", merge_branch_params, |p, t| {
    p.set_path(&t);
});
gated_void!(write_restore_trash, "restoreTrash", restore_trash_params, |p, t| {
    p.set_entry_id(&t);
});
gated_void!(write_empty_trash, "emptyTrash", empty_trash_params, |_p, _t| {});
gated_void!(write_clear_recent_files, "clearRecentFiles", clear_recent_files_params, |_p, _t| {});
gated_void!(write_set_quota, "setQuota", set_quota_params, |p, t| {
    p.set_path(&t);
});
gated_void!(write_set_lifecycle, "setLifecycle", set_lifecycle_params, |p, t| {
    p.set_path(&t);
});
gated_void!(write_create_file, "createFile", create_file_params, |p, t| {
    p.set_path(&t);
});
gated_void!(write_close_file, "closeFile", close_file_params, |_p, _t| {});
gated_void!(write_flush_file, "flushFile", flush_file_params, |_p, _t| {});

/// The writes whose answer is not a void: the contract's own count.
macro_rules! gated_count {
    ($fn_name:ident, $method:literal, $params_ty:ident, $results_ty:ident, $field:ident, |$p:ident, $t:ident| $body:block) => {
        fn $fn_name(app: &mut App, args: &[String], path: &str) -> i32 {
            let $t = path_of(args);
            let params = msg(|m| {
                let mut $p = m.init_root::<wire::files::$params_ty::Builder<'_>>();
                $body
            });
            let bytes = match super::call_in(app, STEM, INTERFACE, $method, &params) {
                Ok(bytes) => bytes,
                Err(error) => return super::emit_error(app, &error),
            };
            let verb = path.rsplit(' ').next().unwrap_or(path);
            let Some((json_text, human_text)) = tp::with_root::<wire::files::$results_ty::Owned, _, _>(
                &bytes,
                |r| {
                    let count = r.$field();
                    (
                        format!("{{\"verb\":\"{verb}\",\"written\":{count}}}"),
                        format!(
                            "  {} {} {} {}",
                            domain::green(app, "✎"),
                            domain::bold(app, verb),
                            &$t,
                            domain::dim(app, &format!("— {count}"))
                        ),
                    )
                },
            ) else {
                super::usage_error_app(app, "the service answered a result this build cannot read");
                return crate::registry::EXIT_USAGE;
            };
            print_answer(app, json_text, human_text)
        }
    };
}

gated_count!(write_write_file, "writeFile", write_file_params, write_file_results, get_written, |p, _t| {
    p.reborrow().init_handle().init_handle().set_id(0);
});
gated_count!(
    write_apply_lifecycle,
    "applyLifecycle",
    apply_lifecycle_params,
    apply_lifecycle_results,
    get_processed,
    |p, t| {
        p.set_path(&t);
    }
);

/// The batch writes: the contract's own request struct.
macro_rules! gated_batch {
    ($fn_name:ident, $method:literal, $params_ty:ident, $results_ty:ident, |$r:ident, $t:ident| $body:block) => {
        fn $fn_name(app: &mut App, args: &[String], path: &str) -> i32 {
            let $t = path_of(args);
            let params = msg(|m| {
                let mut p = m.init_root::<wire::files::$params_ty::Builder<'_>>();
                let mut $r = p.reborrow().init_request();
                $body
            });
            let bytes = match super::call_in(app, STEM, INTERFACE, $method, &params) {
                Ok(bytes) => bytes,
                Err(error) => return super::emit_error(app, &error),
            };
            let verb = path.rsplit(' ').next().unwrap_or(path);
            let Some((json_text, human_text)) = tp::with_root::<wire::files::$results_ty::Owned, _, _>(
                &bytes,
                |r| {
                    let result = r.get_result();
                    let completed = result
                        .as_ref()
                        .map(|result| result.get_completed())
                        .unwrap_or(0);
                    let failed = result
                        .as_ref()
                        .map(|result| result.get_failed())
                        .unwrap_or(0);
                    (
                        format!(
                            "{{\"verb\":\"{verb}\",\"completed\":{completed},\"failed\":{failed}}}"
                        ),
                        format!(
                            "  {} {} {} {}",
                            domain::green(app, "✎"),
                            domain::bold(app, verb),
                            $t,
                            domain::dim(app, &format!("— {completed} completed, {failed} failed"))
                        ),
                    )
                },
            ) else {
                super::usage_error_app(app, "the service answered a result this build cannot read");
                return crate::registry::EXIT_USAGE;
            };
            print_answer(app, json_text, human_text)
        }
    };
}

gated_batch!(write_batch_copy, "batchCopy", batch_copy_params, batch_copy_results, |r, t| {
    r.set_destination(&t);
    r.reborrow().init_sources(1);
});
gated_batch!(write_batch_move, "batchMove", batch_move_params, batch_move_results, |r, t| {
    r.set_destination(&t);
    r.reborrow().init_sources(1);
});
gated_batch!(write_batch_delete, "batchDelete", batch_delete_params, batch_delete_results, |r, _t| {
    r.reborrow().init_paths(1);
    r.set_recursive(false);
});

/// The lock write: the contract's own lock row.
fn write_lock_file(app: &mut App, args: &[String], path: &str) -> i32 {
    let target = path_of(args);
    let params = msg(|m| {
        let mut p = m.init_root::<wire::files::lock_file_params::Builder<'_>>();
        let mut request = p.reborrow().init_request();
        request.set_path(&target);
        request.set_lock_type(FileLockType::Exclusive);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "lockFile", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let verb = path.rsplit(' ').next().unwrap_or(path);
    let Some((json_text, human_text)) =
        tp::with_root::<wire::files::lock_file_results::Owned, _, _>(&bytes, |r| {
            let lock = r.get_lock();
            let id = lock
                .as_ref()
                .map(|lock| lock.get_lock_id())
                .unwrap_or(0);
            (
                format!("{{\"verb\":\"{verb}\",\"lockId\":{id}}}"),
                format!(
                    "  {} {} {} {}",
                    domain::green(app, "✎"),
                    domain::bold(app, verb),
                    target,
                    domain::dim(app, &format!("— lock {id}"))
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// A `T.VoidResult` answer, in the shell's own "✎ verb target" line.
fn void_verb(app: &mut App, verb: &str, target: &str, bytes: &[u8]) -> i32 {
    let Some((json_text, human_text)) =
        tp::with_root::<smart_protocol::schema::types_capnp::void_result::Owned, _, _>(bytes, |r| {
            match r.which() {
                Ok(smart_protocol::schema::types_capnp::void_result::Which::Success(_)) => (
                    format!("{{\"verb\":\"{verb}\",\"target\":\"{target}\",\"changed\":true}}"),
                    format!(
                        "  {} {} {} {}",
                        domain::green(app, "✎"),
                        domain::bold(app, verb),
                        target,
                        domain::dim(app, "— the engine's own host-mutating path, accepted")
                    ),
                ),
                Ok(smart_protocol::schema::types_capnp::void_result::Which::Error(Ok(error))) => {
                    let reason = text(error.get_message());
                    (
                        format!("{{\"verb\":\"{verb}\",\"error\":\"{reason}\"}}"),
                        format!(
                            "  {} {} {} {}",
                            domain::green(app, "✎"),
                            domain::bold(app, verb),
                            target,
                            domain::red(app, &format!("— refused: {reason}"))
                        ),
                    )
                }
                _ => (
                    format!("{{\"verb\":\"{verb}\",\"target\":\"{target}\"}}"),
                    format!(
                        "  {} {} {} {}",
                        domain::green(app, "✎"),
                        domain::bold(app, verb),
                        target,
                        domain::dim(app, "— the service answered no verdict")
                    ),
                ),
            }
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

// ═════════════════════════════════════════════════════════════════════════
// The shared helpers.
// ═════════════════════════════════════════════════════════════════════════

/// The kind glyph, in the shell's own vocabulary.
fn kind_glyph(kind: &str) -> present::Cell {
    match kind {
        "regular" | "file" => present::Cell::styled("▣", present::Sgr::Plain),
        "directory" | "dir" => present::Cell::styled("▤", present::Sgr::Cyan),
        "symlink" => present::Cell::styled("↔", present::Sgr::Cyan),
        _ => present::Cell::plain("▢"),
    }
}

/// A file type token, from the contract's own enum.
fn file_type_token(kind: core::result::Result<FileType, capnp::NotInSchema>) -> &'static str {
    let kind = kind.unwrap_or(FileType::Unknown);
    match kind {
        FileType::Regular => "regular",
        FileType::Directory => "directory",
        FileType::Symlink => "symlink",
        FileType::Other => "other",
        FileType::Custom => "custom",
        _ => "unknown",
    }
}

/// A retention action token, from the contract's own enum.
fn retention_token(kind: core::result::Result<RetentionAction, capnp::NotInSchema>) -> &'static str {
    match kind.unwrap_or(RetentionAction::Unknown) {
        RetentionAction::Retain => "retain",
        RetentionAction::Archive => "archive",
        RetentionAction::Delete => "delete",
        RetentionAction::Compress => "compress",
        RetentionAction::Notify => "notify",
        RetentionAction::Custom => "custom",
        _ => "unknown",
    }
}

/// A lock type token, from the contract's own enum.
fn lock_type_token(kind: core::result::Result<FileLockType, capnp::NotInSchema>) -> &'static str {
    match kind.unwrap_or(FileLockType::Shared) {
        FileLockType::Exclusive => "exclusive",
        FileLockType::Shared => "shared",
        _ => "custom",
    }
}

/// A file event token, from the contract's own enum.
fn file_event_token(kind: core::result::Result<FileEventType, capnp::NotInSchema>) -> &'static str {
    match kind.unwrap_or(FileEventType::Custom) {
        FileEventType::Create => "created",
        FileEventType::Modify => "modified",
        FileEventType::Delete => "deleted",
        FileEventType::Rename => "renamed",
        FileEventType::Move => "moved",
        FileEventType::Unknown => "unknown",
        _ => "changed",
    }
}

/// A mime type, as the contract spells it.
fn mime_of(mime: &capnp::Result<smart_protocol::schema::types_capnp::mime_type::Reader<'_>>) -> String {
    match mime {
        Ok(mime) => {
            let kind = text(mime.get_type());
            let subtype = text(mime.get_subtype());
            if kind.is_empty() && subtype.is_empty() {
                String::new()
            } else {
                format!("{kind}/{subtype}")
            }
        }
        Err(_) => String::new(),
    }
}

/// A hash output, as the contract spells it.
fn hash_of(hash: &capnp::Result<smart_protocol::schema::types_capnp::hash_output::Reader<'_>>) -> String {
    match hash {
        Ok(hash) => hash_reader(hash),
        Err(_) => String::new(),
    }
}

/// A hash output, as the contract spells it.
fn hash_reader(hash: &smart_protocol::schema::types_capnp::hash_output::Reader<'_>) -> String {
    // The contract's `HashOutput.algorithm` is still an ordinal (the tree's
    // other algorithm vocabularies moved to open tokens): it is printed as
    // the number the contract carries, never guessed into a name.
    let algorithm = hash.get_algorithm();
    let digest = hash
        .get_bytes()
        .map(|d| d.to_vec())
        .unwrap_or_default();
    let hex: String = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{algorithm}:{hex}")
}

/// A hex string, as the bytes it spells.
fn decode_hex(text: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index + 1 < chars.len() {
        let pair: String = chars[index..index + 2].iter().collect();
        if let Ok(byte) = u8::from_str_radix(&pair, 16) {
            bytes.push(byte);
        }
        index += 2;
    }
    bytes
}

/// A timestamp, as the contract-shaped JSON.
fn timestamp(out: &mut String, stamp: &capnp::Result<smart_protocol::schema::types_capnp::timestamp::Reader<'_>>) {
    let mut o = json::Obj::new(out);
    o.key("nanoseconds")
        .int(stamp.as_ref().map(|s| s.get_nanoseconds()).unwrap_or(0));
    o.end();
}

/// A timestamp, in the shell's civil spelling.
fn stamp_label(stamp: &capnp::Result<smart_protocol::schema::types_capnp::timestamp::Reader<'_>>) -> String {
    let nanos = stamp.as_ref().map(|s| s.get_nanoseconds()).unwrap_or(0);
    if nanos == 0 {
        return "—".to_string();
    }
    let secs = (nanos / 1_000_000_000) as i64;
    let millis = (nanos % 1_000_000_000) / 1_000_000;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{millis:03}",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// Days since the epoch → the civil date (the contract's own civil kernel).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d as u32)
}

/// The contract-shaped JSON of a directory listing.
fn entries_json(key: &str, rows: &[wire::directory_entry::Reader<'_>]) -> String {
    let mut out = String::new();
    let mut o = json::Obj::new(&mut out);
    o.key(key).sub(|out| {
        out.push('[');
        for (index, row) in rows.iter().enumerate() {
            json::sep(out, index == 0);
            let mut e = json::Obj::new(out);
            e.key("name").str(&text(row.get_name()));
            e.key("path").str(&text(row.get_path()));
            e.key("type").str(file_type_token(row.get_type()));
            e.key("size").int(row.get_size());
            e.key("modified").sub(|out| timestamp(out, &row.get_modified()));
            e.end();
        }
        out.push(']');
    });
    o.end();
    out
}

/// The contract-shaped JSON of a search result list.
fn search_json(key: &str, rows: &[wire::file_search_result::Reader<'_>]) -> String {
    let mut out = String::new();
    let mut o = json::Obj::new(&mut out);
    o.key(key).sub(|out| {
        out.push('[');
        for (index, row) in rows.iter().enumerate() {
            json::sep(out, index == 0);
            let mut r = json::Obj::new(out);
            r.key("path").str(&text(row.get_path()));
            r.key("name").str(&text(row.get_name()));
            r.key("type").str(file_type_token(row.get_type()));
            r.key("size").int(row.get_size());
            r.key("modified").sub(|out| timestamp(out, &row.get_modified()));
            r.key("score").float(row.get_score() as f64);
            r.end();
        }
        out.push(']');
    });
    o.end();
    out
}

/// The contract-shaped JSON of a version list.
fn versions_json(key: &str, rows: &[wire::file_version::Reader<'_>]) -> String {
    let mut out = String::new();
    let mut o = json::Obj::new(&mut out);
    o.key(key).sub(|out| {
        out.push('[');
        for (index, row) in rows.iter().enumerate() {
            json::sep(out, index == 0);
            let mut v = json::Obj::new(out);
            v.key("versionId").int(row.get_version_id());
            v.key("size").int(row.get_size());
            v.key("message").str(&text(row.get_message()));
            v.key("createdAt").sub(|out| timestamp(out, &row.get_created_at()));
            v.end();
        }
        out.push(']');
    });
    o.end();
    out
}

/// The contract-shaped JSON of a branch list.
fn branches_json(key: &str, rows: &[wire::file_branch::Reader<'_>]) -> String {
    let mut out = String::new();
    let mut o = json::Obj::new(&mut out);
    o.key(key).sub(|out| {
        out.push('[');
        for (index, row) in rows.iter().enumerate() {
            json::sep(out, index == 0);
            let mut b = json::Obj::new(out);
            b.key("branchId").str(&text(row.get_branch_id()));
            b.key("branchName").str(&text(row.get_branch_name()));
            b.key("path").str(&text(row.get_path()));
            b.key("baseVersion").int(row.get_base_version());
            b.key("createdAt").sub(|out| timestamp(out, &row.get_created_at()));
            b.key("author").str(&text(row.get_author()));
            b.end();
        }
        out.push(']');
    });
    o.end();
    out
}

/// The contract-shaped JSON of a file info.
fn info_json(out: &mut String, info: &wire::file_info::Reader<'_>) {
    let mut o = json::Obj::new(out);
    o.key("path").str(&text(info.get_path()));
    o.key("type").str(file_type_token(info.get_type()));
    o.key("size").int(info.get_size());
    o.key("created").sub(|out| timestamp(out, &info.get_created()));
    o.key("modified").sub(|out| timestamp(out, &info.get_modified()));
    o.key("accessed").sub(|out| timestamp(out, &info.get_accessed()));
    o.key("mimeType").str(&mime_of(&info.get_mime_type()));
    o.key("contentHash").str(&hash_of(&info.get_content_hash()));
    o.end();
}

/// The contract-shaped JSON of a content reference.
fn content_ref_json(out: &mut String, result: &Option<wire::content_ref_result::Reader<'_>>) -> String {
    let mut o = json::Obj::new(out);
    match result {
        Some(result) => match result.which() {
            Ok(wire::content_ref_result::Which::Ref(Ok(reference))) => {
                o.key("hash").str(&hash_of(&reference.get_hash()));
                o.key("algorithm").str(&text(reference.get_algorithm()));
                o.key("sizeBytes").int(reference.get_size_bytes());
            }
            _ => {
                o.key("unknown").str("unknown");
            }
        },
        None => {
            o.key("unknown").str("the service answered no result");
        }
    }
    o.end();
    out.to_string()
}

/// The human view of a content reference.
fn content_ref_human(
    app: &App,
    result: &Option<wire::content_ref_result::Reader<'_>>,
    title: &str,
    glyph: &str,
) -> String {
    let (hash, path, size) = match result {
        Some(result) => match result.which() {
            Ok(wire::content_ref_result::Which::Ref(Ok(reference))) => (
                hash_of(&reference.get_hash()),
                text(reference.get_algorithm()),
                reference.get_size_bytes(),
            ),
            _ => (String::new(), String::new(), 0),
        },
        None => (String::new(), String::new(), 0),
    };
    format!(
        "  {} {} {} {} {}",
        domain::cyan(app, glyph),
        domain::bold(app, title),
        domain::bold(app, &path),
        domain::dim(app, &hash),
        domain::dim(app, &present::human_bytes(size))
    )
}

/// A sort field, from the contract's own enum.
fn sort_field_of(token: &str) -> SortField {
    match token {
        "size" => SortField::Size,
        "modified" | "mtime" => SortField::Modified,
        "created" | "ctime" => SortField::Created,
        "accessed" | "atime" => SortField::Accessed,
        _ => SortField::Name,
    }
}

/// A sort order, from the contract's own enum.
fn sort_order_of(token: &str) -> SortOrder {
    match token {
        "descending" | "desc" => SortOrder::Descending,
        _ => SortOrder::Ascending,
    }
}

/// A special path kind, from the contract's own enum.
fn special_path_of(token: &str) -> SpecialPathType {
    match token {
        "desktop" => SpecialPathType::Desktop,
        "documents" => SpecialPathType::Documents,
        "downloads" => SpecialPathType::Downloads,
        "pictures" => SpecialPathType::Pictures,
        "music" => SpecialPathType::Music,
        "videos" => SpecialPathType::Videos,
        "config" => SpecialPathType::Config,
        "cache" => SpecialPathType::Cache,
        "data" => SpecialPathType::Data,
        "runtime" => SpecialPathType::Runtime,
        "templates" => SpecialPathType::Templates,
        "public" => SpecialPathType::Public,
        "trash" => SpecialPathType::Trash,
        _ => SpecialPathType::Home,
    }
}

/// A text reader, as a string (never a guess).
fn text(reader: capnp::Result<capnp::text::Reader<'_>>) -> String {
    reader
        .map(|t| t.to_str().unwrap_or("").to_string())
        .unwrap_or_default()
}

/// Print the answer: the JSON object, or the human view.
fn print_answer(app: &App, json_text: String, human_text: String) -> i32 {
    if app.json() {
        println!("{json_text}");
    } else if !app.quiet() {
        // The human view ends with a newline (a shell prompt deserves one).
        if human_text.ends_with('\n') {
            print!("{human_text}");
        } else {
            println!("{human_text}");
        }
    }
    crate::registry::EXIT_OK
}
