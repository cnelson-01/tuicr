//! A log of the git commands tuicr runs, like an IDE's Git Console:
//! command line, exit status, duration, and output, newest last. Recorded
//! where git is spawned (`process::run_command_output`, the network runner)
//! and shown by `:console`. Process-wide because the commands run from
//! backends, worker threads, and helpers that hold no `App`.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Local};

/// Entries kept; older ones fall off the front.
const CAPACITY: usize = 500;
/// Output lines kept per entry.
const MAX_OUTPUT_LINES: usize = 40;

#[derive(Debug, Clone)]
pub struct ConsoleEntry {
    pub at: DateTime<Local>,
    /// `git <args>` with tuicr's own `-c key=value` plumbing left out.
    pub command: String,
    /// Exit status; `None` when git could not be started.
    pub status: Option<i32>,
    pub ok: bool,
    pub duration: Duration,
    /// Head of stdout on success, of the error text on failure.
    pub output: Vec<String>,
    /// Whether the command changes the repository or a remote (as opposed
    /// to a read like `rev-parse` or `log`).
    pub mutating: bool,
}

static CONSOLE: Mutex<VecDeque<ConsoleEntry>> = Mutex::new(VecDeque::new());

/// Subcommands that never change a repository or a remote.
const READ_ONLY: &[&str] = &[
    "blame",
    "cat-file",
    "check-ignore",
    "config",
    "diff",
    "diff-files",
    "diff-index",
    "diff-tree",
    "for-each-ref",
    "grep",
    "log",
    "ls-files",
    "ls-remote",
    "merge-base",
    "remote",
    "rev-list",
    "rev-parse",
    "show",
    "show-ref",
    "status",
    "symbolic-ref",
    "var",
    "version",
];

fn subcommand(args: &[String]) -> Option<&str> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-c" | "-C" => {
                iter.next();
            }
            a if a.starts_with('-') => {}
            a => return Some(a),
        }
    }
    None
}

/// Whether `git <args>` writes. Listing forms of `branch`, `tag`, and
/// `stash` count as reads.
pub fn is_mutating(args: &[String]) -> bool {
    let Some(sub) = subcommand(args) else {
        return false;
    };
    if READ_ONLY.contains(&sub) {
        return false;
    }
    let rest: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .skip_while(|a| *a != sub)
        .skip(1)
        .collect();
    match sub {
        "branch" | "tag" => {
            let writes = rest.iter().any(|a| {
                matches!(
                    *a,
                    "-d" | "-D" | "--delete" | "-m" | "-M" | "--move" | "-u" | "-f" | "--force"
                ) || a.starts_with("--set-upstream-to")
            });
            let lists = rest.is_empty()
                || rest.iter().any(|a| {
                    matches!(
                        *a,
                        "--list" | "-l" | "-a" | "-r" | "-v" | "-vv" | "--merged" | "--no-merged"
                    ) || a.starts_with("--format")
                        || a.starts_with("--contains")
                        || a.starts_with("--points-at")
                });
            writes || !lists
        }
        "stash" => !matches!(rest.first(), Some(&"list") | Some(&"show")),
        _ => true,
    }
}

/// Drop tuicr's `-c key=value` plumbing so the logged line reads like what a
/// person would type.
fn display_args(args: &[String]) -> String {
    let mut shown = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "-c" {
            iter.next();
            continue;
        }
        if arg.contains(char::is_whitespace) || arg.is_empty() {
            shown.push(format!("'{arg}'"));
        } else {
            shown.push(arg.clone());
        }
    }
    format!("git {}", shown.join(" "))
}

/// Record one finished git command.
pub fn record(args: &[String], status: Option<i32>, ok: bool, duration: Duration, output: &str) {
    let entry = ConsoleEntry {
        at: Local::now(),
        command: display_args(args),
        status,
        ok,
        duration,
        output: output
            .lines()
            .take(MAX_OUTPUT_LINES)
            .map(str::to_string)
            .collect(),
        mutating: is_mutating(args),
    };
    if let Ok(mut log) = CONSOLE.lock() {
        if log.len() == CAPACITY {
            log.pop_front();
        }
        log.push_back(entry);
    }
}

/// A copy of the log, oldest first.
pub fn snapshot() -> Vec<ConsoleEntry> {
    CONSOLE
        .lock()
        .map(|log| log.iter().cloned().collect())
        .unwrap_or_default()
}

/// Forget every entry.
pub fn clear() {
    if let Ok(mut log) = CONSOLE.lock() {
        log.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn classifies_reads_and_writes() {
        assert!(!is_mutating(&args(
            "-c commit.gpgsign=false rev-parse HEAD"
        )));
        assert!(!is_mutating(&args("log --oneline")));
        assert!(!is_mutating(&args("branch --list")));
        assert!(!is_mutating(&args("branch")));
        assert!(!is_mutating(&args("stash list")));
        assert!(is_mutating(&args("branch -D topic")));
        assert!(is_mutating(&args("branch topic")));
        assert!(is_mutating(&args("push --force-with-lease")));
        assert!(is_mutating(&args("-c core.editor=true rebase --continue")));
        assert!(is_mutating(&args("stash push -m x")));
    }

    #[test]
    fn display_drops_config_plumbing_and_quotes_spaces() {
        let shown = display_args(&[
            "-c".to_string(),
            "commit.gpgsign=false".to_string(),
            "commit".to_string(),
            "-m".to_string(),
            "two words".to_string(),
        ]);
        assert_eq!(shown, "git commit -m 'two words'");
    }
}
