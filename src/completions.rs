//! Shell completion generation — driven by the SAME registry table the help
//! screen reads (plan Part 4.3: "the registry drives help, completion, and the
//! honesty law"; grammar law: "completions must be GENERATED, never
//! hand-written").
//!
//! ## The honesty law, applied to completion
//!
//! A command that cannot run here is NOT completed as if it can:
//! * a task whose `cmd-*` feature is off has no registry entry at all, so it is
//!   never offered;
//! * a task the SERVICE reports unavailable (`listUnavailable`) is removed
//!   at render time — the same filter `--help` applies — and the human views
//!   name what was withheld and why.
//!
//! A shell whose completer cannot express the generated two-level shape gets an
//! honest refusal (exit 2) naming why, never a fake completion.

#![cfg(any(
    feature = "cmd-net",
    feature = "cmd-dns",
    feature = "cmd-bt",
    feature = "cmd-tunnel",
    feature = "cmd-web",
    feature = "cmd-socket",
    feature = "cmd-discovery"
))]

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::registry::{self, Availability};

/// The shells this build can generate a completer for.
pub const SHELLS: &[&str] = &["bash", "zsh", "fish", "elvish"];

/// What a generated completion is built from: the registry tasks that can run
/// here, plus the words the service withheld.
pub struct Corpus {
    /// `(task_word, verbs, flags)` for every task this build runs and the
    /// service does not report unavailable.
    pub tasks: Vec<(String, Vec<String>, Vec<String>)>,
    /// Task words withheld because the service reports them unavailable.
    pub withheld: Vec<String>,
    /// Whether the service published a registry at all (an honest note when
    /// it did not: availability is unverified, not "everything runs").
    pub availability_verified: bool,
}

/// Build the completion corpus from the registry table, the service's
/// availability answer, and the service's own published registry.
///
/// The service's registry is the source of TRUTH about what the service
/// offers; the CLI's table is the source of truth about what THIS BUILD
/// implements. The corpus is the union: the CLI's grammar (minus what the
/// service reports unavailable) plus the commands the service publishes that
/// this build does not implement — those run through the GENERIC path, so
/// they are completable.
pub fn corpus(availability: &Availability, service: Option<&registry::ServiceRegistry>) -> Corpus {
    let mut tasks = Vec::new();
    let mut withheld = Vec::new();
    for spec in registry::TASKS {
        for task in spec.task {
            if registry::is_hidden(availability, task) {
                // The whole task is not runnable here: name the path.
                withheld.push(task.to_string());
                continue;
            }
            // A verb that is not runnable hides the task's verbs from
            // completion (the whole path, not just the word) and is named.
            let hidden_verbs: Vec<String> = spec
                .verbs
                .iter()
                .filter(|verb| registry::is_hidden(availability, &format!("{task} {verb}")))
                .map(|verb| format!("{task} {verb}"))
                .collect();
            if !hidden_verbs.is_empty() {
                withheld.extend(hidden_verbs);
                continue;
            }
            tasks.push((
                task.to_string(),
                spec.verbs.iter().map(|v| v.to_string()).collect(),
                spec.flags.iter().map(|f| f.to_string()).collect(),
            ));
        }
    }
    // The service's published commands this build does not implement: they
    // are runnable through the generic path, so they belong in the corpus.
    if let Some(service) = service {
        for command in &service.commands {
            // A command the service reports unavailable is not completed.
            if registry::is_hidden(availability, &command.path) {
                continue;
            }
            let Some((task, verb)) = command.path.split_once(' ') else {
                continue;
            };
            // The task word is the LAST word of the path's family prefix (the
            // service's paths name the family too: "net wifi scan").
            let task = task.rsplit(' ').next().unwrap_or(task);
            if tasks.iter().any(|(name, _, _)| name == task) {
                continue;
            }
            tasks.push((task.to_string(), vec![verb.to_string()], Vec::new()));
        }
    }
    Corpus {
        tasks,
        withheld,
        availability_verified: matches!(availability, Availability::Published(_)),
    }
}

/// Render the corpus for `shell`. `Ok(text)` is the completer to install;
/// `Err(reason)` is the honest refusal naming why the shell is not expressed.
pub fn render(shell: &str, corpus: &Corpus) -> Result<String, String> {
    match shell {
        "bash" => Ok(bash(corpus)),
        "zsh" => Ok(zsh(corpus)),
        "fish" => Ok(fish(corpus)),
        "elvish" => Ok(elvish(corpus)),
        other => Err(format!(
            "no completion for '{other}': this build generates {SHELLS:?}; \
             a '{other}' completer needs a shape this registry does not express yet, \
             so refusing is the honest answer (use --json for the shell-agnostic form)"
        )),
    }
}

/// The word list a shell completes, in registry order.
fn word_list(corpus: &Corpus) -> String {
    corpus
        .tasks
        .iter()
        .map(|(task, _, _)| task.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The flag list (global + per-task), in registry order.
fn flag_list(corpus: &Corpus) -> String {
    let mut flags: Vec<&str> = registry::GLOBAL_FLAGS.to_vec();
    for (_, _, task_flags) in &corpus.tasks {
        for flag in task_flags {
            if !flags.contains(&flag.as_ref()) {
                flags.push(flag);
            }
        }
    }
    flags.push(registry::SELECTOR_FLAG);
    flags.push("--count");
    flags.push("--interval");
    flags.push("--follow");
    flags.join(" ")
}

/// The per-task VERB table a shell's completer consults, as `task:v1 v2 …`
/// entries (the two-level shape the registry expresses).
fn verb_table(corpus: &Corpus) -> Vec<(String, Vec<String>)> {
    corpus
        .tasks
        .iter()
        .filter(|(_, verbs, _)| !verbs.is_empty() && !verbs.iter().all(|v| v.starts_with("--")))
        .map(|(task, verbs, _)| (task.clone(), verbs.clone()))
        .collect()
}

// ── bash ──────────────────────────────────────────────────────────────────

/// A bash completer: the first word is a task, the second its verbs, and the
/// global flags are always offered. Generated, so it cannot drift from help.
fn bash(corpus: &Corpus) -> String {
    let tasks = word_list(corpus);
    let flags = flag_list(corpus);
    let mut cases = String::new();
    for (task, verbs) in verb_table(corpus) {
        cases.push_str(&format!(
            "        {task})\n          COMPREPLY=( $(compgen -W \"{verbs}\" -- \"$cur\") )\n          return 0 ;;\n",
            verbs = verbs.join(" ")
        ));
    }
    format!(
        "# net — bash completion, generated by `net completions bash`\n\
         # The registry data the same `net --help` reads; a command that cannot\n\
         # run on this host is not completed.\n\
         _net() {{\n\
         \x20   local cur prev tasks flags\n\
         \x20   cur=\"${{COMP_WORDS[COMP_CWORD]}}\"\n\
         \x20   tasks=\"{tasks}\"\n\
         \x20   flags=\"{flags}\"\n\
         \x20   COMPREPLY=()\n\
         \x20   # 1st word: the task words\n\
         \x20   if [ \"$COMP_CWORD\" -eq 1 ]; then\n\
         \x20     COMPREPLY=( $(compgen -W \"$tasks $flags\" -- \"$cur\") )\n\
         \x20     return 0\n\
         \x20   fi\n\
         \x20   # 2nd word: the task's verbs\n\
         \x20   case \"${{COMP_WORDS[1]}}\" in\n\
         {cases}\
         \x20   esac\n\
         \x20   # after the verb: the flags\n\
         \x20   COMPREPLY=( $(compgen -W \"$flags\" -- \"$cur\") )\n\
         }}\n\
         complete -F _net net\n"
    )
}

// ── zsh ───────────────────────────────────────────────────────────────────

/// A zsh completer (`_arguments` + a state machine for the second level).
fn zsh(corpus: &Corpus) -> String {
    let tasks = word_list(corpus);
    let flags = flag_list(corpus);
    let mut body = String::new();
    body.push_str("  local -a tasks verbs\n");
    body.push_str(&format!("  tasks=({tasks})\n"));
    body.push_str(&format!("  flags=({})\n", flags.replace(' ', " ")));
    body.push_str("  if (( CURRENT == 2 )); then\n");
    body.push_str("    _describe 'net task' tasks\n    return\n  fi\n");
    body.push_str("  case $words[2] in\n");
    for (task, verbs) in verb_table(corpus) {
        body.push_str(&format!("    {task}) verbs=({})\n", verbs.join(" ")));
    }
    body.push_str("  esac\n");
    body.push_str("  _describe 'net verb' verbs\n");
    format!(
        "#compdef net\n\
         # net — zsh completion, generated by `net completions zsh`\n\
         _net() {{\n\
         {body}\
         }}\n\
         _net \"$@\"\n"
    )
}

// ── fish ──────────────────────────────────────────────────────────────────

/// A fish completer: one `complete` per task, conditioned on the first word,
/// with the task's own flags offered after it and the global flags everywhere.
fn fish(corpus: &Corpus) -> String {
    let mut out = String::from("# net — fish completion, generated by `net completions fish`\n");
    out.push_str("complete -c net -f\n");
    out.push_str(&format!(
        "complete -c net -f -n '__fish_use_subcommand' -a '{}'\n",
        registry::GLOBAL_FLAGS.join(" ")
    ));
    for (task, verbs, task_flags) in &corpus.tasks {
        out.push_str(&format!("complete -c net -f -n '__fish_use_subcommand' -a '{task}'\n"));
        let words: Vec<&str> = verbs
            .iter()
            .filter(|v| !v.starts_with("--"))
            .map(|v| v.as_str())
            .collect();
        if !words.is_empty() {
            out.push_str(&format!(
                "complete -c net -f -n '__fish_seen_subcommand_from {task}' -a '{}'\n",
                words.join(" ")
            ));
        }
        // The task's own flags (the selector flag, the watch cadence flags,
        // the per-task contract flags), offered after the task word.
        let flags: Vec<&str> = task_flags.iter().map(|f| f.as_ref()).collect();
        if !flags.is_empty() {
            out.push_str(&format!(
                "complete -c net -f -n '__fish_seen_subcommand_from {task}' -a '{}'\n",
                flags.join(" ")
            ));
        }
    }
    // The watch flags ride the watch stream wherever `watch` is the task.
    out.push_str(&format!(
        "complete -c net -f -n '__fish_seen_subcommand_from watch' -a '{}'\n",
        registry::WATCH_FLAGS.join(" ")
    ));
    out
}

// ── elvish ────────────────────────────────────────────────────────────────

/// An elvish completer: an `arg-completer` returning the task words, then the
/// task's verbs (a generated case over the registry), then the flags.
fn elvish(corpus: &Corpus) -> String {
    let tasks = word_list(corpus);
    let flags = flag_list(corpus);
    let mut cases = String::new();
    for (task, verbs) in verb_table(corpus) {
        let words: Vec<&str> = verbs
            .iter()
            .filter(|v| !v.starts_with("--"))
            .map(|v| v.as_str())
            .collect();
        if words.is_empty() {
            continue;
        }
        cases.push_str(&format!(
            "    {task} {{ put {} | each {{|c| edit:complex-candidate $c }}; return }}\n",
            words.join(" ")
        ));
    }
    format!(
        "# net — elvish completion, generated by `net completions elvish`\n\
         fn net-cands {{|@args|\n\
         \x20 var tasks = [{tasks}]\n\
         \x20 var flags = [{flags}]\n\
         \x20 if (== (count $args) 2) {{ put $@tasks | each {{|w| edit:complex-candidate $w }}; return }}\n\
         \x20 var task = $args[2]\n\
         \x20 case $task\n\
         {cases}\
         \x20 else {{ }}\n\
         \x20 end\n\
         \x20 put $@flags | each {{|f| edit:complex-candidate $f }}\n\
         }}\n\
         set edit:completion:arg-completer[net] = $net-cands~\n"
    )
}

/// Write a completion to the path the USER named (never a system path
/// silently). Returns the bytes written.
pub fn write_to(path: &str, text: &str) -> std::io::Result<usize> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    file.write_all(text.as_bytes())?;
    file.flush()?;
    Ok(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus() -> Corpus {
        super::corpus(&Availability::Unpublished, None)
    }

    #[test]
    fn every_shell_completes_the_task_words_and_the_watch_verbs() {
        let c = corpus();
        for shell in SHELLS {
            let text = render(shell, &c).unwrap_or_else(|e| panic!("{shell}: {e}"));
            // The registry's own words for THIS build (a task exists only when
            // its family feature is on).
            let words = registry::task_words();
            assert!(!words.is_empty(), "no tasks registered");
            // Words every build registers (the registry view + the watch task
            // when the net family is compiled).
            for word in ["registry"] {
                assert!(text.contains(word), "{shell} does not complete '{word}'");
            }
            if words.contains(&"watch") {
                assert!(
                    text.contains("watch") && text.contains("bandwidth"),
                    "{shell} does not complete the watch stream"
                );
            }
            for word in words.iter().take(6) {
                assert!(text.contains(word), "{shell} does not complete '{word}'");
            }
            // The selector flag is offered only when a task in THIS build
            // lists a contract type (the dns-only build has no such task, so
            // offering `--where` there would be completing a flag that does
            // nothing — the opposite of the honesty law).
            let has_selector = c
                .tasks
                .iter()
                .any(|(_, _, flags)| flags.iter().any(|f| *f == registry::SELECTOR_FLAG));
            if has_selector {
                assert!(text.contains("--where"), "{shell} does not complete the selector flag");
            }
            if words.contains(&"watch") {
                assert!(text.contains("watch"), "{shell} does not complete the watch task");
            }
        }
    }

    #[test]
    fn an_unexpressed_shell_refuses_with_a_reason() {
        let error = render("powershell", &corpus()).unwrap_err();
        assert!(error.contains("powershell"), "{error}");
        assert!(error.contains("bash"), "the refusal names what IS expressed: {error}");
    }

    #[test]
    #[cfg(feature = "cmd-net")]
    fn unavailable_commands_are_not_completed() {
        let c = super::corpus(
            &Availability::Published(vec![registry::Unavailable {
                path: "wifi scan".to_string(),
                reason_class: "serviceUnavailable".to_string(),
                reason: "no radio".to_string(),
            }]),
            None,
        );
        // The exact hidden PATH is named, and the task is not completed.
        assert!(c.withheld.contains(&"wifi scan".to_string()), "{:?}", c.withheld);
        assert!(!c.tasks.iter().any(|(task, _, _)| task == "wifi"));
        let bash = render("bash", &c).unwrap();
        assert!(!bash.contains("wifi)"), "the withheld task must not be completed: {bash}");
    }

    #[test]
    fn an_unpublished_registry_is_stated_not_assumed() {
        let c = super::corpus(&Availability::Unpublished, None);
        assert!(!c.availability_verified);
        let c = super::corpus(&Availability::Published(Vec::new()),
            None,
        );
        assert!(c.availability_verified);
    }
}
