//! `process` — this host's processes: the list, the detail, the tree, the
//! threads, the memory map, the accounting: the coupled shell's presentation,
//! re-keyed from the engine's mirror types (`ProcessInfo`, `ThreadInfo`,
//! `MemoryMapEntry`, `ProcessGroup`, `ProcessStats`) to the CONTRACT'S RESULT
//! READERS (`process.capnp`'s `listProcesses` / `getProcess` / `getProcessTree`
//! / `getThreads` / `getEnvironment` / `getMemoryMap` / `getResourceLimits` /
//! `getAccounting` / `listProcessGroups` / `getStats`).
//!
//! ## What the re-keying changed
//!
//! * the engine's `ProcessInfo` mirror became the contract's `ProcessInfo`
//!   reader — the same fields, read from the wire;
//! * the shell's `Value` records became the contract's own JSON (the field
//!   names are the schema's, verbatim);
//! * the shell's CPU column was the engine's own nanosecond counter; the
//!   contract carries a FRACTION (`cpuUsage`, 0.0–1.0 of one core), so the
//!   column reads the fraction as a percentage of one core;
//! * the shell's **composite inspect** (one read of stat+status+cmdline+statm
//!   +io, with the peak memory, the I/O, the signal probe and the limits
//!   folded in) is not one contract method: the contract splits it into
//!   `getProcess` (the identity and the state), `getAccounting` (the CPU, the
//!   memory, the I/O) and `getResourceLimits` (the declared quota). Each is
//!   presented, and each published path keeps its own view;
//! * the shell's `caps` view has no contract method (the capability tokens
//!   live in discovery's descriptor metadata), so it is not on this surface.
//!
//! The registry publishes forty-five paths, including the debugger's
//! breakpoints and watchpoints, the supervision surface, and the
//! subscription's own `pull` / `cancel`.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::json;
use crate::present;
use crate::schema::process_capnp as wire;
use crate::schema::types_capnp::{ExecutionState, MemoryPerms};
use crate::transport as tp;

use super::domain::{self, Presenter};
use super::App;

/// The schema stem the family presents.
const STEM: &str = "process";
/// The contract interface the family presents.
const INTERFACE: &str = "Process";

/// The family's presenter lookup: keyed by **(schema id, method name)**.
pub fn presenter(schema_id: u64, method: &str) -> Option<Presenter> {
    if schema_id != crate::contract::schema_id(STEM)? {
        return None;
    }
    Some(match method {
        "listProcesses" => list,
        "getProcess" => inspect,
        "getProcessStatus" => status,
        "getProcessTree" => tree,
        "getThreads" => threads,
        "getEnvironment" => environment,
        "getMemoryMap" => maps,
        "getResourceLimits" => limits,
        "getAccounting" => accounting,
        "getResourceAccounting" => resource_accounting,
        "listProcessGroups" => groups,
        "getProcessGroup" => group,
        "getStats" => stats,
        "getExecutionContext" => context,
        "setExecutionContext" => write_set_execution_context,
        "setResourceLimit" => write_set_resource_limit,
        "spawn" => spawn,
        "wait" => wait,
        "setEnvironment" => write_set_environment,
        "attach" => write_attach,
        "detach" => write_detach,
        "readMemory" => read_memory,
        "writeMemory" => write_write_memory,
        "subscribeProcess" => watch,
        "subscribeDeath" => watch_exit,
        "registerSupervisor" => write_register_supervisor,
        "reportProcessFailure" => write_report_failure,
        "respawnProcess" => respawn,
        "getProcessRespawnStatus" => respawn_status,
        "exitProcess" => write_exit_process,
        "stepProcess" => write_step_process,
        "continueProcess" => write_continue_process,
        "setBreakpoint" => write_set_breakpoint,
        "removeBreakpoint" => write_remove_breakpoint,
        "setWatchpoint" => write_set_watchpoint,
        "removeWatchpoint" => write_remove_watchpoint,
        "pull" => subscription_pull,
        "cancel" => subscription_cancel,
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

/// The pid the caller named (a positional, or `--pid`).
fn pid_of(args: &[String]) -> u64 {
    domain::flag(args, "--pid")
        .and_then(|value| value.parse::<u64>().ok())
        .or_else(|| {
            domain::positionals(args)
                .first()
                .and_then(|value| value.parse::<u64>().ok())
        })
        .unwrap_or(0)
}

// ═════════════════════════════════════════════════════════════════════════
// `process list` — the kernel's own enumeration, every value measured.
// ═════════════════════════════════════════════════════════════════════════

fn list(app: &mut App, args: &[String], _path: &str) -> i32 {
    let name = domain::flag(args, "--name").unwrap_or_default();
    let user = domain::flag(args, "--user").unwrap_or_default();
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::list_processes_params::Builder<'_>>();
        {
            let mut filter = params.reborrow().init_filter();
            filter.set_name(&name);
            if let Ok(uid) = user.parse::<u64>() {
                filter.reborrow().init_user().set_id(uid);
            }
        }
        params.reborrow().init_page().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "listProcesses", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::process::list_processes_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::process_info::Reader<'_>> = results
                .get_processes()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            let total = results.get_total();
            (
                {
                    let mut out = String::from("{\"processes\":[");
                    for (index, row) in rows.iter().enumerate() {
                        json::sep(&mut out, index == 0);
                        let mut p = json::Obj::new(&mut out);
                        p.key("pid").int(row.get_pid().map(|id| id.get_id()).unwrap_or(0));
                        p.key("ppid").int(row.get_ppid().map(|id| id.get_id()).unwrap_or(0));
                        p.key("name").str(&text(row.get_name()));
                        p.key("command").str(&text(row.get_command()));
                        p.key("state").str(state_token(row.get_state()));
                        p.key("cpuUsage").float(row.get_cpu_usage() as f64);
                        p.key("memoryUsage").int(row.get_memory_usage());
                        p.key("threads").int(row.get_threads());
                        p.key("startTime")
                            .sub(|out| timestamp(out, &row.get_start_time()));
                        p.key("user").int(row.get_user().map(|u| u.get_id()).unwrap_or(0));
                        p.end();
                    }
                    out.push_str("],\"total\":");
                    out.push_str(&total.to_string());
                    out.push('}');
                    out
                },
                list_human(app, &rows, total),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// The process table: PID, NAME, STATE, UID, MEM, CPU, THR.
fn list_human(app: &App, rows: &[wire::process_info::Reader<'_>], total: u64) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "  {} {} {} {} {}\n",
        domain::cyan(app, "🧮"),
        domain::bold(app, "Processes"),
        domain::dim(app, &format!("({})", rows.len())),
        domain::dim(app, "— /proc/<pid> read natively, every value measured"),
        domain::dim(app, "— every row the read found: `| take N` narrows it")
    ));
    if rows.is_empty() {
        out.push_str(&format!(
            "  └─ {} the host runs no process matching that filter (an honest empty — /proc was \
             read)\n",
            domain::dim(app, "○")
        ));
        return out.trim_end().to_string();
    }
    let mut table = present::Table::new(vec![
        String::from("PID"),
        String::from("NAME"),
        String::from("STATE"),
        String::from("UID"),
        String::from("MEM"),
        String::from("CPU"),
        String::from("THR"),
    ]);
    for row in rows {
        let state = state_token(row.get_state());
        table = table.row(vec![
            present::Cell::plain(row.get_pid().map(|id| id.get_id()).unwrap_or(0).to_string()),
            present::Cell::plain(text(row.get_name())),
            state_cell(state),
            present::Cell::plain(row.get_user().map(|u| u.get_id()).unwrap_or(0).to_string()),
            present::Cell::plain(present::human_bytes(row.get_memory_usage())),
            present::Cell::plain(cpu_label(row.get_cpu_usage() as f64)),
            present::Cell::plain(row.get_threads().to_string()),
        ]);
    }
    out.push_str(&table.cap(1, 32).render(app.style()));
    out.push_str(&format!(
        "\n  {} memory is the kernel's own resident pages; CPU is the contract's own fraction of \
         one core — never a guess",
        domain::cyan(app, "●")
    ));
    let _ = total;
    out.trim_end().to_string()
}

// ═════════════════════════════════════════════════════════════════════════
// `process inspect` — one process's own reading.
// ═════════════════════════════════════════════════════════════════════════

fn inspect(app: &mut App, args: &[String], _path: &str) -> i32 {
    let pid = pid_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::get_process_params::Builder<'_>>();
        params.reborrow().init_pid().set_id(pid);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getProcess", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::process::get_process_results::Owned, _, _>(&bytes, |results| {
            let info = results.get_info().ok();
            (
                {
                    let mut out = String::from("{\"info\":");
                    let mut o = json::Obj::new(&mut out);
                    match &info {
                        Some(info) => {
                            o.key("pid").int(info.get_pid().map(|id| id.get_id()).unwrap_or(0));
                            o.key("ppid").int(info.get_ppid().map(|id| id.get_id()).unwrap_or(0));
                            o.key("name").str(&text(info.get_name()));
                            o.key("command").str(&text(info.get_command()));
                            o.key("state").str(state_token(info.get_state()));
                            o.key("cpuUsage").float(info.get_cpu_usage() as f64);
                            o.key("memoryUsage").int(info.get_memory_usage());
                            o.key("threads").int(info.get_threads());
                            o.key("startTime")
                                .sub(|out| timestamp(out, &info.get_start_time()));
                            o.key("user").int(info.get_user().map(|u| u.get_id()).unwrap_or(0));
                        }
                        None => {
                            o.key("pid").int(pid);
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                match &info {
                    Some(info) => {
                        let state = state_token(info.get_state());
                        let started = info
                            .get_start_time()
                            .map(|t| t.get_nanoseconds())
                            .unwrap_or(0);
                        let mut out = String::new();
                        out.push_str(&format!(
                            "  {} {} {} {}\n",
                            domain::cyan(app, "🔬"),
                            domain::bold(app, "Process"),
                            domain::bold(app, &pid.to_string()),
                            domain::dim(app, &format!("({})", text(info.get_name())))
                        ));
                        out.push_str(&format!(
                            "  ├─ State       {} {}\n",
                            domain::glyph(app, &state_glyph(state)),
                            state
                        ));
                        out.push_str(&format!(
                            "  ├─ Parent      {} · uid {} · {} thread(s)\n",
                            info.get_ppid().map(|id| id.get_id()).unwrap_or(0),
                            info.get_user().map(|u| u.get_id()).unwrap_or(0),
                            info.get_threads()
                        ));
                        let command = text(info.get_command());
                        out.push_str(&format!(
                            "  ├─ Command     {}\n",
                            if command.is_empty() { "—" } else { &command }
                        ));
                        out.push_str(&format!(
                            "  ├─ Memory      {} current\n",
                            present::human_bytes(info.get_memory_usage())
                        ));
                        out.push_str(&format!(
                            "  ├─ CPU         {} of one core (the contract's own fraction)\n",
                            cpu_label(info.get_cpu_usage() as f64)
                        ));
                        out.push_str(&format!(
                            "  ├─ Uptime      started {}\n",
                            stamp_label(started)
                        ));
                        let groups = info
                            .get_groups()
                            .map(|list| list.iter().count())
                            .unwrap_or(0);
                        out.push_str(&format!("  ├─ Groups      {groups} group(s)\n"));
                        out.push_str("  └─ Source     the contract's own /proc/<pid> read");
                        out
                    }
                    None => format!(
                        "  {} {} {} — the host did not answer",
                        domain::cyan(app, "🔬"),
                        domain::bold(app, "Process"),
                        domain::bold(app, &pid.to_string())
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
// `process status` — the state, the exit status, the uptime.
// ═════════════════════════════════════════════════════════════════════════

fn status(app: &mut App, args: &[String], _path: &str) -> i32 {
    let pid = pid_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::get_process_status_params::Builder<'_>>();
        params.reborrow().init_pid().set_id(pid);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getProcessStatus", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::process::get_process_status_results::Owned, _, _>(&bytes, |results| {
            let status = results.get_status().ok();
            let state = status
                .as_ref()
                .map(|s| state_token(s.get_state()))
                .unwrap_or("unknown");
            (
                {
                    let mut out = String::from("{\"status\":");
                    let mut o = json::Obj::new(&mut out);
                    match &status {
                        Some(status) => {
                            o.key("pid").int(status.get_pid().map(|id| id.get_id()).unwrap_or(0));
                            o.key("name").str(&text(status.get_name()));
                            o.key("state").str(state);
                            o.key("uptime")
                                .sub(|out| duration(out, &status.get_uptime()));
                            o.key("memoryBytes").int(status.get_memory_bytes());
                            o.key("cpuTime").sub(|out| duration(out, &status.get_cpu_time()));
                        }
                        None => {
                            o.key("pid").int(pid);
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                match &status {
                    Some(status) => {
                        let uptime = status.get_uptime().map(|d| d.get_nanoseconds()).unwrap_or(0);
                        let cpu = status.get_cpu_time().map(|d| d.get_nanoseconds()).unwrap_or(0);
                        let mut out = String::new();
                        out.push_str(&format!(
                            "  {} {} {} {}\n",
                            domain::cyan(app, "📶"),
                            domain::bold(app, "Status"),
                            domain::bold(app, &pid.to_string()),
                            domain::dim(app, &format!("({})", text(status.get_name())))
                        ));
                        out.push_str(&format!(
                            "  ├─ State       {} {}\n",
                            domain::glyph(app, &state_glyph(state)),
                            state
                        ));
                        out.push_str(&format!(
                            "  ├─ Uptime      {}\n",
                            duration_label(uptime)
                        ));
                        out.push_str(&format!(
                            "  ├─ CPU time    {}\n",
                            duration_label(cpu)
                        ));
                        out.push_str(&format!(
                            "  ├─ Memory      {}\n",
                            present::human_bytes(status.get_memory_bytes())
                        ));
                        out.push_str("  └─ Source     the contract's own status read");
                        out
                    }
                    None => format!(
                        "  {} {} {} — the host did not answer",
                        domain::cyan(app, "📶"),
                        domain::bold(app, "Status"),
                        domain::bold(app, &pid.to_string())
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
// `process tree` — the engine's own walk, each node a real pid.
// ═════════════════════════════════════════════════════════════════════════

fn tree(app: &mut App, args: &[String], _path: &str) -> i32 {
    let pid = pid_of(args);
    let depth = domain::number::<u32>(args, "--depth", 16);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::get_process_tree_params::Builder<'_>>();
        params.reborrow().init_pid().set_id(pid);
        params.set_max_depth(depth);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getProcessTree", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::process::get_process_tree_results::Owned, _, _>(&bytes, |results| {
            let tree = results.get_tree().ok();
            (
                {
                    let mut out = String::from("{\"tree\":");
                    write_tree(&mut out, &tree);
                    out.push('}');
                    out
                },
                match &tree {
                    Some(tree) => {
                        let mut out = String::new();
                        out.push_str(&format!(
                            "  {} {} {}\n",
                            domain::cyan(app, "🌲"),
                            domain::bold(app, "Process tree"),
                            domain::dim(
                                app,
                                &format!(
                                    "(root pid {})",
                                    tree.get_pid().map(|id| id.get_id()).unwrap_or(0)
                                )
                            )
                        ));
                        render_tree(&mut out, app, tree, 0);
                        out.trim_end().to_string()
                    }
                    None => format!(
                        "  {} tree — the host did not answer",
                        domain::cyan(app, "✗")
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

/// One tree node, in the shell's own branch shape.
fn render_tree(
    out: &mut String,
    app: &App,
    node: &wire::process_tree::Reader<'_>,
    depth: usize,
) {
    if depth > 16 {
        return;
    }
    out.push_str(&format!(
        "  {}{} {} {}\n",
        "  ".repeat(depth),
        if depth == 0 { "└─" } else { "├─" },
        node.get_pid().map(|id| id.get_id()).unwrap_or(0),
        text(node.get_name())
    ));
    if let Ok(children) = node.get_children() {
        for child in children.iter() {
            render_tree(out, app, &child, depth + 1);
        }
    }
}

/// The contract-shaped JSON of a tree.
fn write_tree(out: &mut String, node: &Option<wire::process_tree::Reader<'_>>) {
    let mut o = json::Obj::new(out);
    match node {
        Some(node) => {
            o.key("pid").int(node.get_pid().map(|id| id.get_id()).unwrap_or(0));
            o.key("name").str(&text(node.get_name()));
            o.key("state").str(state_token(node.get_state()));
            o.key("children").sub(|out| {
                out.push('[');
                if let Ok(children) = node.get_children() {
                    for (index, child) in children.iter().enumerate() {
                        json::sep(out, index == 0);
                        write_tree(out, &Some(child));
                    }
                }
                out.push(']');
            });
        }
        None => {
            o.key("pid").int(0);
        }
    }
    o.end();
}

// ═════════════════════════════════════════════════════════════════════════
// `process threads` / `process env` / `process maps` — the kernel's own rows.
// ═════════════════════════════════════════════════════════════════════════

fn threads(app: &mut App, args: &[String], _path: &str) -> i32 {
    let pid = pid_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::get_threads_params::Builder<'_>>();
        params.reborrow().init_pid().set_id(pid);
        params.reborrow().init_page().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getThreads", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::process::get_threads_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::thread_info::Reader<'_>> = results
                .get_threads()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            let total = results.get_total();
            (
                {
                    let mut out = String::from("{\"threads\":[");
                    for (index, row) in rows.iter().enumerate() {
                        json::sep(&mut out, index == 0);
                        let mut t = json::Obj::new(&mut out);
                        t.key("id").int(row.get_id().map(|id| id.get_id()).unwrap_or(0));
                        t.key("name").str(&text(row.get_name()));
                        t.key("state").str(state_token(row.get_state()));
                        t.key("cpuUsage").float(row.get_cpu_usage() as f64);
                        t.end();
                    }
                    out.push_str("],\"total\":");
                    out.push_str(&total.to_string());
                    out.push('}');
                    out
                },
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} — the kernel's own /proc/<pid>/task rows\n",
                        domain::cyan(app, "🧵"),
                        domain::bold(app, "Threads"),
                        domain::dim(app, &format!("({})", rows.len()))
                    ));
                    if rows.is_empty() {
                        out.push_str(&format!(
                            "  └─ {} the host names no thread here\n",
                            domain::dim(app, "○")
                        ));
                    } else {
                        let mut table = present::Table::new(vec![
                            String::from("TID"),
                            String::from("NAME"),
                            String::from("STATE"),
                            String::from("CPU"),
                        ]);
                        for row in &rows {
                            let state = state_token(row.get_state());
                            table = table.row(vec![
                                present::Cell::plain(
                                    row.get_id().map(|id| id.get_id()).unwrap_or(0).to_string(),
                                ),
                                present::Cell::plain(text(row.get_name())),
                                state_cell(state),
                                present::Cell::plain(cpu_label(row.get_cpu_usage() as f64)),
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

fn environment(app: &mut App, args: &[String], _path: &str) -> i32 {
    let pid = pid_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::get_environment_params::Builder<'_>>();
        params.reborrow().init_pid().set_id(pid);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getEnvironment", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::process::get_environment_results::Owned, _, _>(&bytes, |results| {
            let pairs: Vec<(String, String)> = results
                .get_env()
                .ok()
                .and_then(|map| map.get_entries().ok())
                .map(|list| {
                    list.iter()
                        .map(|entry| {
                            (
                                text(entry.get_key()),
                                text(entry.get_value()),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            (
                {
                    let mut out = String::from("{\"env\":{");
                    for (index, (key, value)) in pairs.iter().enumerate() {
                        json::sep(&mut out, index == 0);
                        let mut o = json::Obj::new(&mut out);
                        o.key(key).str(value);
                        o.end();
                    }
                    out.push_str("}}");
                    out
                },
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} — the host's own NUL-separated pairs\n",
                        domain::cyan(app, "🌍"),
                        domain::bold(app, "Environment"),
                        domain::dim(app, &format!("(pid {pid}, {})", pairs.len()))
                    ));
                    if pairs.is_empty() {
                        out.push_str(&format!(
                            "  └─ {} the host's own /proc/{pid}/environ reads empty\n",
                            domain::dim(app, "○")
                        ));
                    } else {
                        let mut table = present::Table::new(vec![
                            String::from("VARIABLE"),
                            String::from("VALUE"),
                        ]);
                        for (key, value) in &pairs {
                            table = table.row(vec![
                                present::Cell::plain(key.clone()),
                                present::truncate_to(value, 56).into(),
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

fn maps(app: &mut App, args: &[String], _path: &str) -> i32 {
    let pid = pid_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::get_memory_map_params::Builder<'_>>();
        params.reborrow().init_pid().set_id(pid);
        params.reborrow().init_page().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getMemoryMap", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::process::get_memory_map_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::memory_map_entry::Reader<'_>> = results
                .get_regions()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            let total = results.get_total();
            (
                {
                    let mut out = String::from("{\"regions\":[");
                    for (index, row) in rows.iter().enumerate() {
                        json::sep(&mut out, index == 0);
                        let mut r = json::Obj::new(&mut out);
                        r.key("start").int(row.get_start());
                        r.key("end").int(row.get_end());
                        r.key("perms").str(&perms_token(row.get_perms().unwrap_or(MemoryPerms::Read)));
                        r.key("name").str(&text(row.get_name()));
                        r.key("backing").str(&text(row.get_backing()));
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
                        "  {} {} {} — the kernel's own /proc/<pid>/maps rows\n",
                        domain::cyan(app, "🗺"),
                        domain::bold(app, "Memory map"),
                        domain::dim(app, &format!("({})", rows.len()))
                    ));
                    if rows.is_empty() {
                        out.push_str(&format!(
                            "  └─ {} the host names no region here\n",
                            domain::dim(app, "○")
                        ));
                    } else {
                        let mut table = present::Table::new(vec![
                            String::from("START"),
                            String::from("END"),
                            String::from("SIZE"),
                            String::from("PERMS"),
                            String::from("NAME"),
                        ]);
                        for row in &rows {
                            let size = row.get_end().saturating_sub(row.get_start());
                            table = table.row(vec![
                                present::Cell::plain(format!("{:#x}", row.get_start())),
                                present::Cell::plain(format!("{:#x}", row.get_end())),
                                present::Cell::plain(present::human_bytes(size)),
                                present::Cell::plain(perms_token(row.get_perms().unwrap_or(MemoryPerms::Read))),
                                present::truncate_to(&text(row.get_name()), 40).into(),
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
// `process limits` / `process accounting` / `process resource-accounting`.
// ═════════════════════════════════════════════════════════════════════════

fn limits(app: &mut App, args: &[String], _path: &str) -> i32 {
    let pid = pid_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::get_resource_limits_params::Builder<'_>>();
        params.reborrow().init_pid().set_id(pid);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getResourceLimits", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::process::get_resource_limits_results::Owned, _, _>(&bytes, |results| {
            let quota = results.get_limits().ok();
            (
                {
                    let mut out = String::from("{\"limits\":");
                    let mut o = json::Obj::new(&mut out);
                    match &quota {
                        Some(quota) => {
                            o.key("memoryBytes").int(quota.get_memory_bytes());
                            o.key("cpuPercent").float(quota.get_cpu_percent() as f64);
                            o.key("ioReadBytesPerSec").int(quota.get_io_read_bytes_per_sec());
                            o.key("ioWriteBytesPerSec").int(quota.get_io_write_bytes_per_sec());
                            o.key("pids").int(quota.get_pids());
                        }
                        None => {
                            o.key("memoryBytes").int(0);
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                match &quota {
                    Some(quota) => format!(
                        "  {} {} {} memory · {}% cpu · {} io r/s · {} io w/s · {} pid(s)",
                        domain::cyan(app, "📐"),
                        domain::bold(app, "Limits"),
                        present::human_bytes(quota.get_memory_bytes()),
                        quota.get_cpu_percent(),
                        present::human_bytes(quota.get_io_read_bytes_per_sec()),
                        present::human_bytes(quota.get_io_write_bytes_per_sec()),
                        quota.get_pids()
                    ),
                    None => format!(
                        "  {} {} — the host published no quota",
                        domain::cyan(app, "📐"),
                        domain::bold(app, "Limits")
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

fn accounting(app: &mut App, args: &[String], _path: &str) -> i32 {
    let pid = pid_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::get_accounting_params::Builder<'_>>();
        params.reborrow().init_pid().set_id(pid);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getAccounting", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::process::get_accounting_results::Owned, _, _>(&bytes, |results| {
            let usage = results.get_usage().ok();
            (
                {
                    let mut out = String::from("{\"usage\":");
                    let mut o = json::Obj::new(&mut out);
                    match &usage {
                        Some(usage) => {
                            o.key("cpuTime").sub(|out| duration(out, &usage.get_cpu_time()));
                            o.key("memoryBytes").int(usage.get_memory_bytes());
                            o.key("peakMemoryBytes").int(usage.get_peak_memory_bytes());
                            o.key("ioReadBytes").int(usage.get_io_read_bytes());
                            o.key("ioWriteBytes").int(usage.get_io_write_bytes());
                            o.key("networkRxBytes").int(usage.get_network_rx_bytes());
                            o.key("networkTxBytes").int(usage.get_network_tx_bytes());
                        }
                        None => {
                            o.key("memoryBytes").int(0);
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                match &usage {
                    Some(usage) => {
                        let cpu = usage.get_cpu_time().map(|d| d.get_nanoseconds()).unwrap_or(0);
                        let mut out = String::new();
                        out.push_str(&format!(
                            "  {} {}\n",
                            domain::cyan(app, "📈"),
                            domain::bold(app, "Accounting")
                        ));
                        out.push_str(&format!(
                            "  ├─ CPU time   {} ({cpu} ns)\n",
                            duration_label(cpu)
                        ));
                        out.push_str(&format!(
                            "  ├─ Memory     {} current · {} peak\n",
                            present::human_bytes(usage.get_memory_bytes()),
                            present::human_bytes(usage.get_peak_memory_bytes())
                        ));
                        out.push_str(&format!(
                            "  ├─ I/O        {} read · {} written\n",
                            present::human_bytes(usage.get_io_read_bytes()),
                            present::human_bytes(usage.get_io_write_bytes())
                        ));
                        out.push_str(&format!(
                            "  └─ Network    {} in · {} out",
                            present::human_bytes(usage.get_network_rx_bytes()),
                            present::human_bytes(usage.get_network_tx_bytes())
                        ));
                        out
                    }
                    None => format!(
                        "  {} {} — the host published no accounting",
                        domain::cyan(app, "📈"),
                        domain::bold(app, "Accounting")
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

fn resource_accounting(app: &mut App, args: &[String], _path: &str) -> i32 {
    let pid = pid_of(args);
    let seconds = domain::number::<u64>(args, "--seconds", 0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::get_resource_accounting_params::Builder<'_>>();
        params.reborrow().init_pid().set_id(pid);
        params
            .reborrow()
            .init_duration()
            .set_nanoseconds(seconds * 1_000_000_000);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getResourceAccounting", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::process::get_resource_accounting_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let window = results.get_window().ok();
            let entries = window
                .as_ref()
                .and_then(|w| w.get_entries().ok())
                .map(|list| list.iter().collect::<Vec<_>>())
                .unwrap_or_default();
            (
                {
                    let mut out = String::from("{\"window\":");
                    let mut o = json::Obj::new(&mut out);
                    o.key("pid")
                        .int(window.as_ref().map(|w| w.get_pid().map(|id| id.get_id()).unwrap_or(0)).unwrap_or(0));
                    o.key("truncated")
                        .boolean(window.as_ref().map(|w| w.get_truncated()).unwrap_or(false));
                    o.key("entries")
                        .int(entries.len() as u64);
                    o.end();
                    out.push('}');
                    out
                },
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} — the kernel's own counters over the window\n",
                        domain::cyan(app, "📈"),
                        domain::bold(app, "Resource accounting"),
                        domain::dim(app, &format!("({})", entries.len()))
                    ));
                    if entries.is_empty() {
                        out.push_str(&format!(
                            "  └─ {} the host recorded no sample in that window\n",
                            domain::dim(app, "○")
                        ));
                    } else {
                        let mut table = present::Table::new(vec![
                            String::from("WHEN"),
                            String::from("CPU"),
                            String::from("MEMORY"),
                        ]);
                        for entry in &entries {
                            table = table.row(vec![
                                present::Cell::plain(
                                    entry
                                        .get_timestamp()
                                        .map(|t| t.get_nanoseconds())
                                        .unwrap_or(0)
                                        .to_string(),
                                ),
                                present::Cell::plain(cpu_label(entry.get_cpu_usage() as f64)),
                                present::Cell::plain(present::human_bytes(entry.get_memory_usage())),
                            ]);
                        }
                        out.push_str(&table.render(app.style()));
                    }
                    out.trim_end().to_string()
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
// `process groups` / `process group` — the kernel's own pgrp/session rows.
// ═════════════════════════════════════════════════════════════════════════

fn groups(app: &mut App, _args: &[String], _path: &str) -> i32 {
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::list_process_groups_params::Builder<'_>>();
        params.reborrow().init_page().set_limit(u32::MAX);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "listProcessGroups", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::process::list_process_groups_results::Owned, _, _>(&bytes, |results| {
            let rows: Vec<wire::process_group::Reader<'_>> = results
                .get_groups()
                .map(|list| list.iter().collect())
                .unwrap_or_default();
            let total = results.get_total();
            (
                {
                    let mut out = String::from("{\"groups\":[");
                    for (index, row) in rows.iter().enumerate() {
                        json::sep(&mut out, index == 0);
                        let mut g = json::Obj::new(&mut out);
                        g.key("groupId")
                            .int(row.get_group_id().map(|id| id.get_id()).unwrap_or(0));
                        g.key("name").str(&text(row.get_name()));
                        g.key("sessionId")
                            .int(row.get_session_id().map(|id| id.get_id()).unwrap_or(0));
                        g.key("foreground").boolean(row.get_foreground());
                        g.end();
                    }
                    out.push_str("],\"total\":");
                    out.push_str(&total.to_string());
                    out.push('}');
                    out
                },
                {
                    let mut out = String::new();
                    out.push_str(&format!(
                        "  {} {} {} — the kernel's own pgrp/session rows\n",
                        domain::cyan(app, "👥"),
                        domain::bold(app, "Process groups"),
                        domain::dim(app, &format!("({})", rows.len()))
                    ));
                    if rows.is_empty() {
                        out.push_str(&format!(
                            "  └─ {} the host runs no process group\n",
                            domain::dim(app, "○")
                        ));
                    } else {
                        let mut table = present::Table::new(vec![
                            String::from("PGID"),
                            String::from("NAME"),
                            String::from("SESSION"),
                            String::from("MEMBERS"),
                        ]);
                        for row in &rows {
                            let members = row
                                .get_processes()
                                .map(|list| list.iter().count())
                                .unwrap_or(0);
                            table = table.row(vec![
                                present::Cell::plain(
                                    row.get_group_id()
                                        .map(|id| id.get_id())
                                        .unwrap_or(0)
                                        .to_string(),
                                ),
                                present::Cell::plain(text(row.get_name())),
                                present::Cell::plain(
                                    row.get_session_id()
                                        .map(|id| id.get_id())
                                        .unwrap_or(0)
                                        .to_string(),
                                ),
                                present::Cell::plain(members.to_string()),
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

fn group(app: &mut App, args: &[String], _path: &str) -> i32 {
    let id = domain::number::<u64>(args, "--id", 0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::get_process_group_params::Builder<'_>>();
        params.reborrow().init_group_id().set_id(id);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getProcessGroup", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::process::get_process_group_results::Owned, _, _>(&bytes, |results| {
            let group = results.get_group().ok();
            let members: Vec<u64> = group
                .as_ref()
                .and_then(|g| g.get_processes().ok())
                .map(|list| list.iter().map(|p| p.get_id()).collect())
                .unwrap_or_default();
            let session = group
                .as_ref()
                .map(|g| g.get_session_id().map(|id| id.get_id()).unwrap_or(0))
                .unwrap_or(0);
            let names: Vec<String> = members.iter().map(|pid| pid.to_string()).collect();
            (
                {
                    let mut out = String::from("{\"group\":");
                    let mut o = json::Obj::new(&mut out);
                    o.key("groupId").int(id);
                    o.key("name")
                        .str(&group.as_ref().map(|g| text(g.get_name())).unwrap_or_default());
                    o.key("sessionId").int(session);
                    o.key("members")
                        .int(members.len() as u64);
                    o.end();
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} {} member(s) in session {} — {}",
                    domain::cyan(app, "👥"),
                    domain::bold(app, &format!("Group {id}")),
                    members.len(),
                    session,
                    names.join(", ")
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
// `process stats` — the aggregate over the kernel's own enumeration.
// ═════════════════════════════════════════════════════════════════════════

fn stats(app: &mut App, args: &[String], _path: &str) -> i32 {
    let user = domain::flag(args, "--user").unwrap_or_default();
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::get_stats_params::Builder<'_>>();
        if let Ok(uid) = user.parse::<u64>() {
            params.reborrow().init_user().set_id(uid);
        }
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getStats", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::process::get_stats_results::Owned, _, _>(&bytes, |results| {
            let stats = results.get_stats().ok();
            (
                {
                    let mut out = String::from("{\"stats\":");
                    let mut o = json::Obj::new(&mut out);
                    match &stats {
                        Some(stats) => {
                            o.key("totalProcesses").int(stats.get_total_processes());
                            o.key("active").int(stats.get_active());
                            o.key("blocked").int(stats.get_blocked());
                            o.key("suspended").int(stats.get_suspended());
                            o.key("terminated").int(stats.get_terminated());
                            o.key("totalCpuUsage").float(stats.get_total_cpu_usage() as f64);
                            o.key("totalMemoryUsage").int(stats.get_total_memory_usage());
                        }
                        None => {
                            o.key("totalProcesses").int(0);
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                match &stats {
                    Some(stats) => {
                        let mut out = String::new();
                        out.push_str(&format!(
                            "  {} {} {} — the kernel's own enumeration, aggregated\n",
                            domain::cyan(app, "📊"),
                            domain::bold(app, "Process stats"),
                            domain::dim(app, &format!("({})", user))
                        ));
                        out.push_str(&format!(
                            "  ├─ Processes  {} total · {} active · {} blocked · {} suspended · {} \
                             terminated\n",
                            stats.get_total_processes(),
                            stats.get_active(),
                            stats.get_blocked(),
                            stats.get_suspended(),
                            stats.get_terminated()
                        ));
                        out.push_str(&format!(
                            "  ├─ CPU        the contract's own aggregate over its measured window: \
                             {}\n",
                            stats.get_total_cpu_usage()
                        ));
                        out.push_str(&format!(
                            "  └─ Memory     {} in use (the kernel's own /proc/meminfo)",
                            present::human_bytes(stats.get_total_memory_usage())
                        ));
                        out
                    }
                    None => format!(
                        "  {} {} — the host published no aggregate",
                        domain::cyan(app, "📊"),
                        domain::bold(app, "Process stats")
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
// `process context` — the isolation boundary and the held capabilities.
// ═════════════════════════════════════════════════════════════════════════

fn context(app: &mut App, args: &[String], _path: &str) -> i32 {
    let pid = pid_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::get_execution_context_params::Builder<'_>>();
        params.reborrow().init_pid().set_id(pid);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getExecutionContext", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::process::get_execution_context_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let context = results.get_context().ok();
            let capabilities = context
                .as_ref()
                .and_then(|c| c.get_capabilities().ok())
                .map(|list| list.iter().count())
                .unwrap_or(0);
            (
                {
                    let mut out = String::from("{\"context\":");
                    let mut o = json::Obj::new(&mut out);
                    match &context {
                        Some(context) => {
                            o.key("pid")
                                .int(context.get_pid().map(|id| id.get_id()).unwrap_or(0));
                            o.key("capabilities").int(capabilities as u64);
                            o.key("quota").sub(|out| {
                                let mut q = json::Obj::new(out);
                                match context.get_quota() {
                                    Ok(quota) => {
                                        q.key("memoryBytes").int(quota.get_memory_bytes());
                                        q.key("cpuPercent").float(quota.get_cpu_percent() as f64);
                                        q.key("pids").int(quota.get_pids());
                                    }
                                    Err(_) => {
                                        q.key("memoryBytes").int(0);
                                    }
                                }
                                q.end();
                            });
                        }
                        None => {
                            o.key("pid").int(pid);
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                match &context {
                    Some(context) => {
                        let mut out = String::new();
                        out.push_str(&format!(
                            "  {} {} {} — what the process IS: its isolation boundary and the \
                             capabilities it holds\n",
                            domain::cyan(app, "🧭"),
                            domain::bold(app, "Execution context"),
                            domain::bold(app, &pid.to_string())
                        ));
                        out.push_str(&format!(
                            "  ├─ Capabilities {capabilities} held (reported, not checked — \
                             capability.capnp is the authority)\n"
                        ));
                        match context.get_quota() {
                            Ok(quota) => out.push_str(&format!(
                                "  └─ Quota       {} memory · {}% cpu · {} pid(s)",
                                present::human_bytes(quota.get_memory_bytes()),
                                quota.get_cpu_percent(),
                                quota.get_pids()
                            )),
                            Err(_) => {
                                out.push_str("  └─ Quota       — the process declares none")
                            }
                        }
                        out
                    }
                    None => format!(
                        "  {} {} {} — the host did not answer",
                        domain::cyan(app, "🧭"),
                        domain::bold(app, "Execution context"),
                        domain::bold(app, &pid.to_string())
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
// The gated writes: the CLI issues the call; the SERVICE decides.
// ═════════════════════════════════════════════════════════════════════════

/// A gated write: the CLI builds the CONTRACT'S params and issues the call;
/// the SERVICE decides (its capability class refuses a read-only connection).
macro_rules! gated_void {
    ($fn_name:ident, $method:literal, $params_ty:ident, |$p:ident, $pid:ident| $body:block) => {
        fn $fn_name(app: &mut App, args: &[String], path: &str) -> i32 {
            let $pid = pid_of(args);
            let params = msg(|m| {
                let mut $p = m.init_root::<wire::process::$params_ty::Builder<'_>>();
                $body
            });
            let bytes = match super::call_in(app, STEM, INTERFACE, $method, &params) {
                Ok(bytes) => bytes,
                Err(error) => return super::emit_error(app, &error),
            };
            let verb = path.rsplit(' ').next().unwrap_or(path);
            void_verb(app, verb, &format!("pid {}", $pid), &bytes)
        }
    };
}

gated_void!(write_attach, "attach", attach_params, |p, pid| {
    p.reborrow().init_pid().set_id(pid);
});
gated_void!(write_detach, "detach", detach_params, |p, pid| {
    p.reborrow().init_pid().set_id(pid);
});
gated_void!(write_step_process, "stepProcess", step_process_params, |p, pid| {
    p.reborrow().init_pid().set_id(pid);
});
gated_void!(write_continue_process, "continueProcess", continue_process_params, |p, pid| {
    p.reborrow().init_pid().set_id(pid);
});
gated_void!(write_set_environment, "setEnvironment", set_environment_params, |p, pid| {
    p.reborrow().init_pid().set_id(pid);
    p.reborrow().init_env();
});
gated_void!(write_set_resource_limit, "setResourceLimit", set_resource_limit_params, |p, pid| {
    p.reborrow().init_pid().set_id(pid);
    p.reborrow().init_limit();
});
gated_void!(write_set_execution_context, "setExecutionContext", set_execution_context_params, |p, pid| {
    p.reborrow().init_pid().set_id(pid);
    p.reborrow().init_context();
});
gated_void!(write_write_memory, "writeMemory", write_memory_params, |p, pid| {
    p.reborrow().init_pid().set_id(pid);
    p.set_address(0);
    p.reborrow().init_data();
});
gated_void!(write_report_failure, "reportProcessFailure", report_process_failure_params, |p, pid| {
    p.reborrow().init_pid().set_id(pid);
    p.reborrow().init_report();
});
gated_void!(write_exit_process, "exitProcess", exit_process_params, |p, _pid| {
    p.reborrow().init_reason();
});
gated_void!(write_remove_breakpoint, "removeBreakpoint", remove_breakpoint_params, |p, pid| {
    p.reborrow().init_pid().set_id(pid);
    p.reborrow().init_id().set_id(0);
});
gated_void!(write_remove_watchpoint, "removeWatchpoint", remove_watchpoint_params, |p, pid| {
    p.reborrow().init_pid().set_id(pid);
    p.reborrow().init_id().set_id(0);
});

/// The breakpoint write: the contract's own id.
fn write_set_breakpoint(app: &mut App, args: &[String], path: &str) -> i32 {
    let pid = pid_of(args);
    let params = msg(|m| {
        let mut p = m.init_root::<wire::process::set_breakpoint_params::Builder<'_>>();
        p.reborrow().init_pid().set_id(pid);
        p.reborrow().init_breakpoint();
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "setBreakpoint", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let verb = path.rsplit(' ').next().unwrap_or(path);
    let Some((json_text, human_text)) =
        tp::with_root::<wire::process::set_breakpoint_results::Owned, _, _>(&bytes, |r| {
            let id = r.get_id().map(|id| id.get_id()).unwrap_or(0);
            (
                format!("{{\"verb\":\"{verb}\",\"id\":{id}}}"),
                format!(
                    "  {} {} pid {} {}",
                    domain::green(app, "✎"),
                    domain::bold(app, verb),
                    pid,
                    domain::dim(app, &format!("— breakpoint {id}"))
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// The watchpoint write: the contract's own id.
fn write_set_watchpoint(app: &mut App, args: &[String], path: &str) -> i32 {
    let pid = pid_of(args);
    let params = msg(|m| {
        let mut p = m.init_root::<wire::process::set_watchpoint_params::Builder<'_>>();
        p.reborrow().init_pid().set_id(pid);
        p.reborrow().init_watchpoint();
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "setWatchpoint", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let verb = path.rsplit(' ').next().unwrap_or(path);
    let Some((json_text, human_text)) =
        tp::with_root::<wire::process::set_watchpoint_results::Owned, _, _>(&bytes, |r| {
            let id = r.get_id().map(|id| id.get_id()).unwrap_or(0);
            (
                format!("{{\"verb\":\"{verb}\",\"id\":{id}}}"),
                format!(
                    "  {} {} pid {} {}",
                    domain::green(app, "✎"),
                    domain::bold(app, verb),
                    pid,
                    domain::dim(app, &format!("— watchpoint {id}"))
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// The supervisor registration: the contract's own handle.
fn write_register_supervisor(app: &mut App, args: &[String], path: &str) -> i32 {
    let pid = pid_of(args);
    let params = msg(|m| {
        let mut p = m.init_root::<wire::process::register_supervisor_params::Builder<'_>>();
        p.reborrow().init_supervisor().set_id(pid);
        p.reborrow().init_target().set_id(pid);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "registerSupervisor", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let verb = path.rsplit(' ').next().unwrap_or(path);
    let Some((json_text, human_text)) =
        tp::with_root::<wire::process::register_supervisor_results::Owned, _, _>(&bytes, |r| {
            let handle = r.get_handle().map(|id| id.get_id()).unwrap_or(0);
            (
                format!("{{\"verb\":\"{verb}\",\"handle\":{handle}}}"),
                format!(
                    "  {} {} pid {} {}",
                    domain::green(app, "✎"),
                    domain::bold(app, verb),
                    pid,
                    domain::dim(app, &format!("— supervisor {handle}"))
                ),
            )
        })
    else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// `process spawn` — a new process, the contract's own way.
fn spawn(app: &mut App, args: &[String], _path: &str) -> i32 {
    let image = domain::positionals(args)
        .first()
        .map(|value| (*value).to_string())
        .or_else(|| domain::flag(args, "--image"))
        .unwrap_or_default();
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::spawn_params::Builder<'_>>();
        let mut spec = params.reborrow().init_spec();
        spec.set_image_ref(&image);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "spawn", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::process::spawn_results::Owned, _, _>(
        &bytes,
        |results| {
            let pid = results.get_pid().map(|id| id.get_id()).unwrap_or(0);
            (
                format!("{{\"pid\":{pid}}}"),
                format!(
                    "  {} {} #{} {}",
                    domain::green(app, "✎"),
                    domain::bold(app, "Spawn"),
                    domain::bold(app, &pid.to_string()),
                    domain::dim(app, "— the engine's own host-mutating path, accepted")
                ),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// `process wait` — the exit status, or the honest "still running".
fn wait(app: &mut App, args: &[String], _path: &str) -> i32 {
    let pid = pid_of(args);
    let millis = domain::number::<u64>(args, "--timeout", 0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::wait_params::Builder<'_>>();
        params.reborrow().init_pid().set_id(pid);
        params
            .reborrow()
            .init_timeout()
            .set_nanoseconds(millis * 1_000_000);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "wait", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::process::wait_results::Owned, _, _>(
        &bytes,
        |results| {
            let status = results.get_exit_status().ok();
            let verdict = exit_verdict(status.as_ref());
            (
                {
                    let mut out = String::from("{\"exitStatus\":");
                    let mut o = json::Obj::new(&mut out);
                    match &status {
                        Some(_) => {
                            o.key("kind").str(verdict.0);
                            o.key("detail").str(&verdict.1);
                        }
                        None => {
                            o.key("kind").str("unknown");
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} pid {} — {}",
                    domain::cyan(app, "⏳"),
                    domain::bold(app, "Wait"),
                    pid,
                    domain::bold(app, &verdict.1)
                ),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// `process read-memory` — the contract's own read, the attached reference
/// being the authority.
fn read_memory(app: &mut App, args: &[String], _path: &str) -> i32 {
    let pid = pid_of(args);
    let address = domain::number::<u64>(args, "--address", 0);
    let size = domain::number::<u64>(args, "--size", 0);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::read_memory_params::Builder<'_>>();
        params.reborrow().init_pid().set_id(pid);
        params.set_address(address);
        params.set_size(size);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "readMemory", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<wire::process::read_memory_results::Owned, _, _>(
        &bytes,
        |results| {
            let read = results.get_read().ok();
            let payload = read
                .as_ref()
                .and_then(|r| r.get_data().ok())
                .and_then(|handle| match handle.which() {
                    Ok(crate::schema::types_capnp::data_handle::Which::Inline(data)) => {
                        data.map(|bytes| bytes.to_vec()).ok()
                    }
                    _ => None,
                })
                .unwrap_or_default();
            (
                {
                    let mut out = String::from("{\"read\":");
                    let mut o = json::Obj::new(&mut out);
                    o.key("bytes").int(payload.len() as u64);
                    o.end();
                    out.push('}');
                    out
                },
                format!(
                    "  {} {} pid {} — {} byte(s) read at {address:#x}",
                    domain::cyan(app, "🧠"),
                    domain::bold(app, "Memory"),
                    pid,
                    payload.len()
                ),
            )
        },
    ) else {
        super::usage_error_app(app, "the service answered a result this build cannot read");
        return crate::registry::EXIT_USAGE;
    };
    print_answer(app, json_text, human_text)
}

/// `process respawn` / `process respawn-status`.
fn respawn(app: &mut App, args: &[String], _path: &str) -> i32 {
    let pid = pid_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::respawn_process_params::Builder<'_>>();
        params.reborrow().init_pid().set_id(pid);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "respawnProcess", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) =
        tp::with_root::<wire::process::respawn_process_results::Owned, _, _>(&bytes, |results| {
            let respawned = results.get_respawned().map(|id| id.get_id()).unwrap_or(0);
            (
                format!("{{\"respawned\":{respawned}}}"),
                format!(
                    "  {} {} #{} {}",
                    domain::green(app, "✎"),
                    domain::bold(app, "Respawn"),
                    domain::bold(app, &respawned.to_string()),
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

fn respawn_status(app: &mut App, args: &[String], _path: &str) -> i32 {
    let pid = pid_of(args);
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::get_process_respawn_status_params::Builder<'_>>();
        params.reborrow().init_pid().set_id(pid);
    });
    let bytes = match super::call_in(app, STEM, INTERFACE, "getProcessRespawnStatus", &params) {
        Ok(bytes) => bytes,
        Err(error) => return super::emit_error(app, &error),
    };
    let Some((json_text, human_text)) = tp::with_root::<
        wire::process::get_process_respawn_status_results::Owned,
        _,
        _,
    >(
        &bytes,
        |results| {
            let status = results.get_status().ok();
            (
                {
                    let mut out = String::from("{\"status\":");
                    let mut o = json::Obj::new(&mut out);
                    match &status {
                        Some(status) => {
                            o.key("pid").int(status.get_pid().map(|id| id.get_id()).unwrap_or(0));
                            o.key("attempts").int(status.get_attempts());
                            o.key("fallbackActive").boolean(status.get_fallback_active());
                        }
                        None => {
                            o.key("pid").int(pid);
                        }
                    }
                    o.end();
                    out.push('}');
                    out
                },
                match &status {
                    Some(status) => format!(
                        "  {} {} pid {} — {} attempt(s){}",
                        domain::cyan(app, "🔄"),
                        domain::bold(app, "Respawn status"),
                        pid,
                        status.get_attempts(),
                        if status.get_fallback_active() {
                            " · the fallback path is active"
                        } else {
                            ""
                        }
                    ),
                    None => format!(
                        "  {} {} pid {} — the host did not answer",
                        domain::cyan(app, "🔄"),
                        domain::bold(app, "Respawn status"),
                        pid
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
// The subscriptions: `process watch` / `process watch-exit`, and the
// subscription's own `pull` / `cancel`.
// ═════════════════════════════════════════════════════════════════════════

fn watch(app: &mut App, args: &[String], _path: &str) -> i32 {
    let pid = pid_of(args);
    let count = domain::number::<u32>(args, "--count", 1);
    let interval = domain::number::<u64>(args, "--interval", 400);
    let subscription = match subscribe(app, "subscribeProcess", "ProcessSubscription", pid) {
        Ok(subscription) => subscription,
        Err(error) => return super::emit_error(app, &error),
    };
    process_loop(app, &subscription, count, interval)
}

fn watch_exit(app: &mut App, args: &[String], _path: &str) -> i32 {
    let pid = pid_of(args);
    let count = domain::number::<u32>(args, "--count", 1);
    let interval = domain::number::<u64>(args, "--interval", 400);
    let subscription = match subscribe(app, "subscribeDeath", "ExitNotificationSubscription", pid) {
        Ok(subscription) => subscription,
        Err(error) => return super::emit_error(app, &error),
    };
    exit_loop(app, &subscription, count, interval)
}

/// The subscription's own bounded pull.
fn subscription_pull(app: &mut App, args: &[String], _path: &str) -> i32 {
    let pid = pid_of(args);
    let count = domain::number::<u32>(args, "--count", 1);
    let interval = domain::number::<u64>(args, "--interval", 400);
    let subscription = match subscribe(app, "subscribeProcess", "ProcessSubscription", pid) {
        Ok(subscription) => subscription,
        Err(error) => return super::emit_error(app, &error),
    };
    process_loop(app, &subscription, count, interval)
}

/// The subscription's own cancel.
fn subscription_cancel(app: &mut App, args: &[String], _path: &str) -> i32 {
    let pid = pid_of(args);
    let subscription = match subscribe(app, "subscribeProcess", "ProcessSubscription", pid) {
        Ok(subscription) => subscription,
        Err(error) => return super::emit_error(app, &error),
    };
    match subscription.cancel(app.conn()) {
        Ok(()) => print_answer(
            app,
            String::from("{\"cancelled\":true}"),
            format!(
                "  {} {} {}",
                domain::green(app, "✎"),
                domain::bold(app, "Subscription"),
                domain::dim(app, "cancelled — the contract's own cancel")
            ),
        ),
        Err(error) => super::emit_error(app, &error),
    }
}

/// Mint the contract's own subscription.
fn subscribe(
    app: &mut App,
    method: &str,
    interface: &'static str,
    pid: u64,
) -> Result<crate::transport::Subscription, crate::transport::CallError> {
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process::subscribe_process_params::Builder<'_>>();
        params.reborrow().init_pid().set_id(pid);
    });
    let (interface_id, method_id) = super::ids_in(STEM, INTERFACE, method).ok_or_else(|| {
        crate::transport::CallError::Transport(format!(
            "the contract has no `{method}` method in this build"
        ))
    })?;
    let reader = tp::decode_message::<wire::process::subscribe_process_params::Owned>(&params)
        .map_err(crate::transport::CallError::Transport)?;
    let call_params = reader
        .get()
        .map_err(|e| crate::transport::CallError::Transport(format!("{e}")))?;
    let response = app.conn().call_typed::<
        wire::process::subscribe_process_params::Owned,
        wire::process::subscribe_process_results::Owned,
    >(interface_id, method_id, call_params)?;
    let results = response
        .get()
        .map_err(|e| crate::transport::CallError::Transport(format!("{e}")))?;
    let capability = results
        .get_subscription()
        .map_err(|e| crate::transport::CallError::Transport(format!("{e}")))?;
    Ok(crate::transport::Subscription::new(
        capability.client,
        "process",
        interface,
    ))
}

/// The bounded pull loop the shell's watch had — the process subscription's
/// own snapshot.
fn process_loop(
    app: &mut App,
    subscription: &crate::transport::Subscription,
    count: u32,
    interval: u64,
) -> i32 {
    let json_mode = app.json();
    let quiet = app.quiet();
    let mut delivered = 0u32;
    let mut idle = 0u32;
    while delivered < count.max(1) && idle < 3 {
        let payload = match pull_process(app, subscription, count.max(1) - delivered) {
            Ok(payload) => payload,
            Err(error) => {
                let _ = subscription.cancel(app.conn());
                return super::emit_error(app, &error);
            }
        };
        // The pull's answer IS the contract's own results struct; the row is
        // painted inside the read, because a reader borrows its message.
        let batch = tp::with_root::<wire::process_subscription::pull_results::Owned, _, _>(
            &payload,
            |r| {
                let Ok(info) = r.get_info() else {
                    return 0;
                };
                let pid = info.get_pid().map(|id| id.get_id()).unwrap_or(0);
                let name = text(info.get_name());
                let state = state_token(info.get_state());
                if json_mode {
                    let mut out = String::new();
                    let mut o = json::Obj::new(&mut out);
                    o.key("pid").int(pid);
                    o.key("name").str(&name);
                    o.key("state").str(state);
                    o.key("cpuUsage").float(info.get_cpu_usage() as f64);
                    o.key("memoryUsage").int(info.get_memory_usage());
                    o.end();
                    println!("{out}");
                } else if !quiet {
                    println!(
                        "  {} {:<8} {:<24} {}",
                        domain::glyph(app, &state_glyph(state)),
                        pid,
                        present::truncate_to(&name, 24),
                        present::human_bytes(info.get_memory_usage())
                    );
                }
                1
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
    summary(app, delivered, idle, json_mode, quiet)
}

/// The exit subscription's own bounded pull.
fn exit_loop(
    app: &mut App,
    subscription: &crate::transport::Subscription,
    count: u32,
    interval: u64,
) -> i32 {
    let json_mode = app.json();
    let quiet = app.quiet();
    let mut delivered = 0u32;
    let mut idle = 0u32;
    while delivered < count.max(1) && idle < 3 {
        let payload = match pull_exit(app, subscription, count.max(1) - delivered) {
            Ok(payload) => payload,
            Err(error) => {
                let _ = subscription.cancel(app.conn());
                return super::emit_error(app, &error);
            }
        };
        let batch = tp::with_root::<wire::exit_notification_subscription::pull_results::Owned, _, _>(
            &payload,
            |r| {
                let Ok(notification) = r.get_notification() else {
                    return 0;
                };
                let pid = notification.get_pid().map(|id| id.get_id()).unwrap_or(0);
                let status = notification.get_exit_status().ok();
                let verdict = exit_verdict(status.as_ref());
                if json_mode {
                    let mut out = String::new();
                    let mut o = json::Obj::new(&mut out);
                    o.key("pid").int(pid);
                    o.key("exitStatus").str(verdict.0);
                    o.key("detail").str(&verdict.1);
                    o.end();
                    println!("{out}");
                } else if !quiet {
                    println!(
                        "  {} pid {} — {}",
                        domain::green(app, "◉"),
                        pid,
                        domain::bold(app, &verdict.1)
                    );
                }
                1
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
    summary(app, delivered, idle, json_mode, quiet)
}

/// The watch's own closing line.
fn summary(_app: &App, delivered: u32, idle: u32, json_mode: bool, quiet: bool) -> i32 {
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

/// The process subscription's own bounded pull (`pull(maxInfos)`).
fn pull_process(
    app: &mut App,
    subscription: &crate::transport::Subscription,
    max: u32,
) -> Result<Vec<u8>, crate::transport::CallError> {
    let info = crate::contract::interface_by_name("process", "ProcessSubscription").ok_or_else(|| {
        crate::transport::CallError::Transport(
            "the contract interface `ProcessSubscription` is not compiled into this build"
                .to_string(),
        )
    })?;
    let method_id =
        crate::contract::method_id("process", "ProcessSubscription", "pull").ok_or_else(|| {
            crate::transport::CallError::Transport(
                "the contract interface `ProcessSubscription` has no `pull` method in this build"
                    .to_string(),
            )
        })?;
    let params = msg(|m| {
        let mut params = m.init_root::<wire::process_subscription::pull_params::Builder<'_>>();
        params.set_max_infos(max);
    });
    app.conn()
        .call_capability(subscription.client(), info.type_id, method_id, &params)
}

/// The exit subscription's own bounded pull (`pull(maxNotifications)`).
fn pull_exit(
    app: &mut App,
    subscription: &crate::transport::Subscription,
    max: u32,
) -> Result<Vec<u8>, crate::transport::CallError> {
    let info = crate::contract::interface_by_name("process", "ExitNotificationSubscription")
        .ok_or_else(|| {
            crate::transport::CallError::Transport(
                "the contract interface `ExitNotificationSubscription` is not compiled into this                  build"
                    .to_string(),
            )
        })?;
    let method_id =
        crate::contract::method_id("process", "ExitNotificationSubscription", "pull").ok_or_else(
            || {
                crate::transport::CallError::Transport(
                    "the contract interface `ExitNotificationSubscription` has no `pull` method in                      this build"
                        .to_string(),
                )
            },
        )?;
    let params = msg(|m| {
        let mut params =
            m.init_root::<wire::exit_notification_subscription::pull_params::Builder<'_>>();
        params.set_max_notifications(max);
    });
    app.conn()
        .call_capability(subscription.client(), info.type_id, method_id, &params)
}

// ═════════════════════════════════════════════════════════════════════════
// The shared helpers.
// ═════════════════════════════════════════════════════════════════════════

/// A `T.VoidResult` answer, in the shell's own "✎ verb target" line.
fn void_verb(app: &mut App, verb: &str, target: &str, bytes: &[u8]) -> i32 {
    let Some((json_text, human_text)) =
        tp::with_root::<crate::schema::types_capnp::void_result::Owned, _, _>(bytes, |r| {
            match r.which() {
                Ok(crate::schema::types_capnp::void_result::Which::Success(_)) => (
                    format!("{{\"verb\":\"{verb}\",\"target\":\"{target}\",\"changed\":true}}"),
                    format!(
                        "  {} {} {} {}",
                        domain::green(app, "✎"),
                        domain::bold(app, verb),
                        target,
                        domain::dim(app, "— the engine's own host-mutating path, accepted")
                    ),
                ),
                Ok(crate::schema::types_capnp::void_result::Which::Error(error)) => {
                    let reason = error.map(|e| text(e.get_message())).unwrap_or_default();
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

/// The state glyph, in the shell's own vocabulary.
fn state_glyph(state: &str) -> present::Cell {
    match state {
        "active" | "running" => present::Cell::styled("◉", present::Sgr::Green),
        "blocked" | "sleeping" | "disk-sleep" => present::Cell::styled("◐", present::Sgr::Yellow),
        "suspended" | "stopped" | "tracing-stop" => present::Cell::styled("◑", present::Sgr::Yellow),
        "terminated" | "zombie" | "dead" => present::Cell::styled("✗", present::Sgr::Red),
        _ => present::Cell::styled("○", present::Sgr::Dimmed),
    }
}

/// The state cell: the glyph AND the state, in one cell.
fn state_cell(state: &str) -> present::Cell {
    let glyph = state_glyph(state);
    present::Cell::styled(&format!("{} {state}", glyph.content()), glyph.style())
}

/// A state token, from the contract's own enum.
fn state_token(state: core::result::Result<ExecutionState, capnp::NotInSchema>) -> &'static str {
    match state.unwrap_or(ExecutionState::Unknown) {
        ExecutionState::Unknown => "unknown",
        ExecutionState::Active => "active",
        ExecutionState::Blocked => "blocked",
        ExecutionState::Suspended => "suspended",
        ExecutionState::Terminated => "terminated",
        _ => "custom",
    }
}

/// A memory-permission token, from the contract's own enum.
fn perms_token(perms: MemoryPerms) -> String {
    match perms {
        MemoryPerms::Read => "r".to_string(),
        MemoryPerms::Write => "w".to_string(),
        MemoryPerms::Execute => "x".to_string(),
    }
}

/// The CPU column: the contract's own fraction of one core.
fn cpu_label(fraction: f64) -> String {
    format!("{:.1}%", fraction * 100.0)
}

/// An exit status, in the shell's own words.
fn exit_verdict(status: Option<&crate::schema::types_capnp::exit_status::Reader<'_>>) -> (&'static str, String) {
    let Some(status) = status else {
        return ("unknown", "the host did not answer".to_string());
    };
    match status.which() {
        Ok(crate::schema::types_capnp::exit_status::Which::Exited(code)) => {
            ("exited", format!("exited with {code}"))
        }
        Ok(crate::schema::types_capnp::exit_status::Which::Signaled(signal)) => (
            "signaled",
            format!(
                "killed by signal {}",
                signal_token(signal.unwrap_or(crate::schema::types_capnp::SignalKind::Terminate))
            ),
        ),
        Ok(crate::schema::types_capnp::exit_status::Which::TimedOut(_)) => {
            ("timedOut", "timed out".to_string())
        }
        _ => ("custom", "a backend-specific ending".to_string()),
    }
}

/// A signal's own token, from the contract's enum.
fn signal_token(signal: crate::schema::types_capnp::SignalKind) -> &'static str {
    use crate::schema::types_capnp::SignalKind;
    match signal {
        SignalKind::Terminate => "terminate",
        SignalKind::Interrupt => "interrupt",
        SignalKind::Kill => "kill",
        SignalKind::HangUp => "hangup",
        SignalKind::UserDefined1 => "user-defined-1",
        SignalKind::UserDefined2 => "user-defined-2",
        SignalKind::UserDefined3 => "user-defined-3",
        SignalKind::UserDefined4 => "user-defined-4",
        SignalKind::Custom => "custom",
        SignalKind::Child => "child",
        SignalKind::Stop => "stop",
        SignalKind::Continue => "continue",
    }
}

/// A timestamp, as the contract-shaped JSON.
fn timestamp(out: &mut String, stamp: &capnp::Result<crate::schema::types_capnp::timestamp::Reader<'_>>) {
    let mut o = json::Obj::new(out);
    o.key("nanoseconds")
        .int(stamp.as_ref().map(|s| s.get_nanoseconds()).unwrap_or(0));
    o.end();
}

/// A duration, as the contract-shaped JSON.
fn duration(out: &mut String, value: &capnp::Result<crate::schema::types_capnp::duration::Reader<'_>>) {
    let mut o = json::Obj::new(out);
    o.key("nanoseconds")
        .int(value.as_ref().map(|d| d.get_nanoseconds()).unwrap_or(0));
    o.end();
}

/// A duration in the shell's own words: days, hours, minutes, seconds.
fn duration_label(nanos: u64) -> String {
    let secs = nanos / 1_000_000_000;
    let millis = (nanos % 1_000_000_000) / 1_000_000;
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let minutes = (secs % 3_600) / 60;
    let seconds = secs % 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else if secs > 0 {
        format!("{seconds}s")
    } else if millis > 0 {
        format!("{millis} ms")
    } else {
        format!("{nanos} ns")
    }
}

/// A start time, in the shell's own civil spelling (UTC, no `date` fork).
fn stamp_label(nanos: u64) -> String {
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
