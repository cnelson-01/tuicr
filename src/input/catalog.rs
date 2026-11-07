//! The action catalog: every key and `:` command, with where it applies and
//! what it does. One table feeds the command palette (`Ctrl-p`, an IDE's
//! Find Action), the which-key popup shown after a prefix key, and the
//! descriptions next to `:` completions, so documentation can't drift from
//! what the keys do as long as entries land with the features they describe.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Where an entry's keys work. The palette ranks the focused scope first
/// and focuses the right pane before replaying keys from another scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Any Normal-mode pane (leader keys, `:` commands, toggles).
    Global,
    /// The diff pane.
    Diff,
    /// The file tree.
    Tree,
    /// The log pane (commit graph).
    Log,
    /// Inside the branch picker.
    Branches,
    /// Inside the stash picker.
    Stashes,
    /// Inside the merge conflict view.
    Merge,
    /// The changes window (working tree status).
    Changes,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::Global => "any",
            Scope::Diff => "diff",
            Scope::Tree => "tree",
            Scope::Log => "log",
            Scope::Branches => "branches",
            Scope::Stashes => "stashes",
            Scope::Merge => "merge",
            Scope::Changes => "changes",
        }
    }
}

/// How the palette performs an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Run {
    /// Replay keys (notation in [`parse_keys`]) in the entry's scope.
    Keys(&'static str),
    /// Run a complete `:` command.
    Command(&'static str),
    /// Open the `:` line pre-filled, for commands that take an argument.
    Prompt(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub struct Entry {
    pub scope: Scope,
    pub group: &'static str,
    /// Keys as shown to the user; `<leader>` is replaced by the configured
    /// leader when displayed.
    pub keys: &'static str,
    pub desc: &'static str,
    pub run: Run,
}

const fn e(
    scope: Scope,
    group: &'static str,
    keys: &'static str,
    desc: &'static str,
    run: Run,
) -> Entry {
    Entry {
        scope,
        group,
        keys,
        desc,
        run,
    }
}

use Run::{Command as Cmd, Keys, Prompt};
use Scope::{Branches, Changes, Diff, Global, Log, Merge, Stashes, Tree};

#[rustfmt::skip]
pub const ENTRIES: &[Entry] = &[
    // ── Help ─────────────────────────────────────────────────────────────
    e(Global, "Help", "Ctrl-p", "Search every action (this palette)", Cmd("palette")),
    e(Global, "Help", "<leader>p", "Command palette", Keys("<leader>p")),
    e(Global, "Help", "?", "Full key reference", Keys("?")),
    e(Global, "Help", ":messages", "Show the full current error message", Cmd("messages")),
    e(Global, "Help", "Ctrl-l", "Clear and redraw the whole screen", Keys("<C-l>")),
    // ── Navigate ─────────────────────────────────────────────────────────
    e(Diff, "Navigate", "j / k", "Move the cursor down / up", Keys("j")),
    e(Diff, "Navigate", "Ctrl-d / Ctrl-u", "Half page down / up", Keys("<C-d>")),
    e(Diff, "Navigate", "Ctrl-f / Ctrl-b", "Full page down / up", Keys("<C-f>")),
    e(Diff, "Navigate", "Ctrl-e / Ctrl-y", "Scroll the view without moving the cursor", Keys("<C-e>")),
    e(Diff, "Navigate", "g / G", "First / last file", Keys("g")),
    e(Diff, "Navigate", "{N}G", "Go to source line N", Prompt("")),
    e(Diff, "Navigate", ":N / :oN", "Go to new-side / old-side line N", Prompt("")),
    e(Diff, "Navigate", "} / {", "Next / previous file", Keys("}")),
    e(Diff, "Navigate", "] / [", "Next / previous hunk", Keys("]")),
    e(Diff, "Navigate", "n / N", "Next / previous change (next / previous match while a search is highlighted)", Keys("n")),
    e(Diff, "Navigate", "m / M", "Next / previous comment", Keys("m")),
    e(Diff, "Navigate", "zz", "Center the cursor line", Keys("zz")),
    e(Diff, "Navigate", "zt", "Cursor line to the top", Keys("zt")),
    e(Diff, "Navigate", "zb", "Cursor line to the bottom", Keys("zb")),
    e(Diff, "Navigate", "Enter", "Expand / collapse hidden context", Keys("<CR>")),
    e(Diff, "Navigate", "Shift-Enter", "Expand / collapse all hidden context", Keys("<S-CR>")),
    e(Diff, "Search", "/", "Search the diff", Keys("/")),
    e(Diff, "Search", "f", "Find in files", Keys("f")),
    e(Diff, "Search", "Esc", "Clear search highlighting", Keys("<Esc>")),
    // ── Diff view ────────────────────────────────────────────────────────
    e(Global, "View", ":diff", "Toggle side-by-side / unified", Cmd("diff")),
    e(Global, "View", "<leader>f", "Single-file view", Keys("<leader>f")),
    e(Diff, "View", "h / l", "Pan left / right (both sides together)", Keys("l")),
    e(Diff, "View", "zH", "Pan half a column left", Keys("zH")),
    e(Diff, "View", "zL", "Pan half a column right", Keys("zL")),
    e(Diff, "View", "zs", "Pan to the start of the line", Keys("zs")),
    e(Diff, "View", "ze", "Pan to the end of the longest line", Keys("ze")),
    e(Global, "View", ":wrap", "Toggle line wrap (off: pan instead)", Cmd("wrap")),
    e(Global, "View", "<leader>w", "Toggle ignore-whitespace", Keys("<leader>w")),
    e(Global, "View", "<leader>n", "Toggle relative line numbers", Keys("<leader>n")),
    e(Global, "View", ":theme", "Pick a colour theme, previewed live", Cmd("theme")),
    e(Global, "View", "<leader>a", "Toggle the blame gutter (annotate)", Keys("<leader>a")),
    e(Diff, "View", "<leader>A", "Open the commit that last changed this line", Keys("<leader>A")),
    // ── Layout ───────────────────────────────────────────────────────────
    e(Global, "Layout", "<leader><", "Move the side-by-side divider left", Keys("<leader><")),
    e(Global, "Layout", "<leader>>", "Move the side-by-side divider right", Keys("<leader>>")),
    e(Global, "Layout", ":set split N", "Old side's share of the width (15-85%)", Prompt("set split ")),
    e(Global, "Layout", "<leader>[", "Narrow the file tree", Keys("<leader>[")),
    e(Global, "Layout", "<leader>]", "Widen the file tree", Keys("<leader>]")),
    e(Global, "Layout", ":set treewidth N", "File tree width (8-60%)", Prompt("set treewidth ")),
    e(Global, "Layout", "<leader>0", "Reset divider and tree width", Keys("<leader>0")),
    e(Global, "Layout", "<leader>z", "Zen mode: diff only (again restores)", Keys("<leader>z")),
    e(Global, "Layout", "<leader>e", "Show / hide the file tree", Keys("<leader>e")),
    e(Global, "Layout", "<leader>g", "Open / focus / hide the log pane", Keys("<leader>g")),
    e(Global, "Layout", "<leader>t", "Open / focus / hide the changes window", Keys("<leader>t")),
    e(Global, "Layout", "<leader>+", "Taller log pane", Keys("<leader>+")),
    e(Global, "Layout", "<leader>-", "Shorter log pane", Keys("<leader>-")),
    e(Global, "Layout", "<leader>s", "Show / hide the commit selector", Keys("<leader>s")),
    // ── Focus ────────────────────────────────────────────────────────────
    e(Global, "Focus", "Tab / Shift-Tab", "Cycle focus between panes", Keys("<Tab>")),
    e(Global, "Focus", "<leader>h", "Focus the pane to the left", Keys("<leader>h")),
    e(Global, "Focus", "<leader>l", "Focus the pane to the right", Keys("<leader>l")),
    e(Global, "Focus", "<leader>k", "Focus the pane above", Keys("<leader>k")),
    e(Global, "Focus", "<leader>j", "Focus the pane below", Keys("<leader>j")),
    // ── Review ───────────────────────────────────────────────────────────
    e(Diff, "Review", "r", "Mark the file reviewed", Keys("r")),
    e(Diff, "Review", "R", "Mark the hunk reviewed", Keys("R")),
    e(Diff, "Review", "c", "Comment on the line", Keys("c")),
    e(Diff, "Review", "C", "Comment on the file", Keys("C")),
    e(Global, "Review", "<leader>c", "Review-level comment", Keys("<leader>c")),
    e(Diff, "Review", "v / V", "Select lines (comment, stage, copy)", Keys("V")),
    e(Diff, "Review", "i", "Edit the comment at the cursor", Keys("i")),
    e(Diff, "Review", "dd", "Delete the comment at the cursor", Keys("dd")),
    e(Diff, "Review", "y", "Copy the review to the clipboard", Keys("y")),
    e(Diff, "Review", "Y", "Copy the comment at the cursor", Keys("Y")),
    e(Global, "Review", ":summary", "List pending comments", Cmd("summary")),
    e(Global, "Review", ":reviewed", "Show / hide reviewed files", Cmd("reviewed")),
    e(Global, "Review", ":w", "Save the review", Cmd("w")),
    e(Global, "Review", ":clip", "Copy the review to the clipboard", Cmd("clip")),
    e(Global, "Review", ":clearc", "Delete every comment (prefills; Enter runs)", Prompt("clearc")),
    e(Global, "Review", ":clear", "Delete every comment and reviewed mark (prefills)", Prompt("clear")),
    e(Diff, "Review", "V then s", "Suggest a change for the selected lines", Keys("V")),
    e(Diff, "Edit", "e", "Open the file in $EDITOR at this line", Keys("e")),
    e(Global, "Edit", ":e", "Reload the diff", Cmd("e")),
    e(Global, "Edit", ":local", "Compare the commit range with local (working tree); again goes back", Cmd("local")),
    e(Global, "Edit", ":clearedits", "Forget lines edited in $EDITOR (drops their edit tint)", Cmd("clearedits")),
    // ── File tree ────────────────────────────────────────────────────────
    e(Tree, "Files", "Enter", "Open the file / toggle a folder", Keys("<CR>")),
    e(Tree, "Files", "Space", "Toggle a folder", Keys("<Space>")),
    e(Tree, "Files", "o / O", "Expand / collapse all folders", Keys("o")),
    e(Tree, "Files", "i", "Show only paths matching a regex", Keys("i")),
    e(Tree, "Files", "e", "Hide paths matching a regex", Keys("e")),
    e(Tree, "Files", "I / E", "Clear the include / exclude filter", Keys("I")),
    e(Tree, "Files", "/", "Find a file by path", Keys("/")),
    // ── Local changes ────────────────────────────────────────────────────
    e(Diff, "Local changes", "s", "Stage the file", Keys("s")),
    e(Diff, "Local changes", "u", "Unstage the file", Keys("u")),
    e(Diff, "Local changes", "S", "Stage the hunk", Keys("S")),
    e(Diff, "Local changes", "U", "Unstage the hunk", Keys("U")),
    e(Diff, "Local changes", "V then S", "Stage only the selected lines", Keys("V")),
    e(Diff, "Local changes", "V then U", "Unstage only the selected lines", Keys("V")),
    e(Diff, "Local changes", "X", "Discard the hunk (rollback)", Keys("X")),
    e(Global, "Local changes", ":discard", "Discard every change to the file", Cmd("discard")),
    e(Global, "Local changes", ":stage", "Stage the reviewed files", Cmd("stage")),
    e(Global, "Local changes", ":commit", "Commit the index (opens the message editor)", Cmd("commit")),
    e(Global, "Local changes", ":amend", "Amend HEAD", Cmd("amend")),
    e(Global, "Local changes", ":stash", "Stash the working tree", Cmd("stash")),
    e(Global, "Local changes", ":stash pop", "Restore the newest stash", Cmd("stash pop")),
    e(Global, "Local changes", ":stashes", "Stash picker", Cmd("stashes")),
    e(Global, "Local changes", ":changes", "Toggle the changes window (working tree status)", Cmd("changes")),
    e(Changes, "Local changes", "j / k", "Move; the diff follows the file", Keys("j")),
    e(Changes, "Local changes", "Enter", "Open the file's diff; fold a group or submodule", Keys("<CR>")),
    e(Changes, "Local changes", "Space", "Stage / unstage the file (the checkbox)", Keys("<Space>")),
    e(Changes, "Local changes", "s / u", "Stage / unstage the file", Keys("s")),
    e(Changes, "Local changes", "a / A", "Stage / unstage everything in this worktree", Keys("a")),
    e(Changes, "Local changes", "X", "Discard the file; a submodule returns to its recorded commit", Keys("X")),
    e(Changes, "Local changes", "c / C", "Commit / amend this worktree (inside a submodule from its rows)", Keys("c")),
    e(Changes, "Local changes", "h / l", "Fold / unfold a group or a submodule's files", Keys("h")),
    e(Changes, "Local changes", "o / O", "Unfold / fold every group", Keys("o")),
    e(Changes, "Local changes", "r", "Re-read the status", Keys("r")),
    e(Stashes, "Local changes", "Enter", "Pop the stash", Cmd("stashes")),
    e(Stashes, "Local changes", "a", "Apply the stash, keeping it", Cmd("stashes")),
    e(Stashes, "Local changes", "d", "Drop the stash", Cmd("stashes")),
    e(Stashes, "Local changes", "v", "Show the stash's diff", Cmd("stashes")),
    // ── Log ──────────────────────────────────────────────────────────────
    e(Global, "Log", ":commits", "Choose commits to review", Cmd("commits")),
    e(Global, "Log", ") / (", "Next / previous commit in the review", Keys(")")),
    e(Log, "Log", "j / k", "Move; the diff previews the commit", Keys("j")),
    e(Log, "Log", "Enter", "Load the commit (or picked range)", Keys("<CR>")),
    e(Log, "Log", "Space", "Pick a commit for a range", Keys("<Space>")),
    e(Log, "Log", "/", "Filter: text, author:, path:, branch:", Keys("/")),
    e(Global, "Log", ":history", "Commits that touched this file", Cmd("history")),
    e(Log, "Commit", "y", "Copy the commit hash", Keys("y")),
    e(Log, "Commit", "c", "Check out the branch / commit", Keys("c")),
    e(Log, "Commit", "n", "New branch at the commit", Keys("n")),
    e(Log, "Commit", "t", "Tag the commit", Keys("t")),
    e(Log, "Commit", "p", "Cherry-pick onto the current branch", Keys("p")),
    e(Log, "Commit", "v", "Revert the commit", Keys("v")),
    e(Log, "Commit", "R", "Reword the commit message", Keys("R")),
    e(Log, "Commit", "f", "Fixup into its parent", Keys("f")),
    e(Log, "Commit", "s", "Squash the picked commits", Keys("s")),
    e(Log, "Commit", "D", "Drop the commit", Keys("D")),
    e(Log, "Commit", "b", "Rebase the current branch onto the commit", Keys("b")),
    e(Log, "Commit", "i", "Interactively rebase from here (reorder, squash, ...)", Keys("i")),
    e(Global, "Commit", ":rebase -i REV", "Interactive rebase from a commit", Prompt("rebase -i ")),
    e(Log, "Commit", "x", "Reset the branch to the commit", Keys("x")),
    e(Log, "Commit", "a", "Abort the operation in progress", Keys("a")),
    e(Global, "Commit", ":undo", "Undo the last commit (keep changes staged)", Cmd("undo")),
    e(Global, "Commit", ":reset", "Reset soft / mixed / hard", Prompt("reset ")),
    e(Global, "Commit", ":tag NAME", "Tag a commit", Prompt("tag ")),
    e(Global, "Commit", ":tag -d NAME", "Delete a tag", Prompt("tag -d ")),
    // ── Branches ─────────────────────────────────────────────────────────
    e(Global, "Branches", "<leader>b", "Branch picker", Keys("<leader>b")),
    e(Global, "Branches", ":checkout NAME", "Check out a branch or commit", Prompt("checkout ")),
    e(Global, "Branches", ":checkout! NAME", "Stash, check out, pop", Prompt("checkout! ")),
    e(Global, "Branches", ":branch NAME", "Create a branch and switch to it", Prompt("branch ")),
    e(Global, "Branches", ":compare A B", "Compare two branches", Prompt("compare ")),
    e(Branches, "Branches", "Enter", "Check out the branch", Keys("<leader>b")),
    e(Branches, "Branches", "Ctrl-s", "Stash, check out, pop", Keys("<leader>b")),
    e(Branches, "Branches", "Tab", "Pick for compare / log", Keys("<leader>b")),
    e(Branches, "Branches", "Ctrl-d", "Compare with the current branch", Keys("<leader>b")),
    e(Branches, "Branches", "Ctrl-o", "Show the picked branches in the log", Keys("<leader>b")),
    e(Global, "Branches", ":merge BRANCH", "Merge a branch into the current one", Prompt("merge ")),
    e(Global, "Branches", ":rebase BRANCH", "Rebase the current branch onto another", Prompt("rebase ")),
    e(Global, "Branches", ":branch -d NAME", "Delete a branch (asks if unmerged)", Prompt("branch -d ")),
    e(Global, "Branches", ":branch -m OLD NEW", "Rename a branch (one name: the current)", Prompt("branch -m ")),
    e(Global, "Branches", ":upstream REMOTE/BRANCH", "Track a remote branch", Prompt("upstream ")),
    e(Branches, "Branches", "Ctrl-g", "Merge into the current branch", Keys("<leader>b")),
    e(Branches, "Branches", "Ctrl-b", "Rebase the current branch onto it", Keys("<leader>b")),
    e(Branches, "Branches", "Ctrl-x", "Delete the branch", Keys("<leader>b")),
    e(Branches, "Branches", "Ctrl-r", "Rename the branch", Keys("<leader>b")),
    e(Branches, "Branches", "Ctrl-t", "Track this remote branch", Keys("<leader>b")),
    e(Branches, "Branches", "Ctrl-f", "Fetch every remote", Keys("<leader>b")),
    e(Global, "Branches", ":merge", "Resolve merge conflicts", Cmd("merge")),
    e(Global, "Branches", ":continue", "Finish the merge / rebase / cherry-pick", Cmd("continue")),
    e(Global, "Branches", ":skip", "Skip the commit a rebase stopped on", Cmd("skip")),
    e(Global, "Branches", ":abort", "Abort the merge / rebase / cherry-pick", Cmd("abort")),
    // ── Remote ───────────────────────────────────────────────────────────
    e(Global, "Remote", ":fetch", "Fetch every remote", Cmd("fetch")),
    e(Global, "Remote", ":pull", "Pull, fast-forward only", Cmd("pull")),
    e(Global, "Remote", ":pull!", "Pull with rebase", Cmd("pull!")),
    e(Global, "Remote", ":push", "Push: review the outgoing commits first", Cmd("push")),
    e(Global, "Remote", ":push!", "Force-push with lease (after rewriting history)", Cmd("push!")),
    e(Global, "Remote", ":push --tags", "Push every tag", Cmd("push --tags")),
    e(Global, "Help", ":console", "Git console: the commands tuicr ran", Cmd("console")),
    // ── Search ───────────────────────────────────────────────────────────
    e(Global, "Find", "<leader>F", "Find in files (from any pane)", Keys("<leader>F")),
    // ── Merge view ───────────────────────────────────────────────────────
    e(Merge, "Merge", "o / t / b", "Take ours / theirs / both", Cmd("merge")),
    e(Merge, "Merge", "u", "Unresolve the conflict", Cmd("merge")),
    e(Merge, "Merge", "n / N", "Next / previous conflict", Cmd("merge")),
    e(Merge, "Merge", "w", "Write the file (stages when resolved)", Cmd("merge")),
    // ── Session ──────────────────────────────────────────────────────────
    e(Global, "Session", ":sessions", "Resume a saved review", Cmd("sessions")),
    e(Global, "Session", ":prs", "Pick a pull request", Cmd("prs")),
    e(Global, "Session", ":submit", "Submit the review to the forge", Cmd("submit")),
    e(Global, "Session", ":q", "Quit", Cmd("q")),
    e(Global, "Session", ":x", "Save and quit", Cmd("x")),
];

/// Which-key prefixes: keys that wait for another key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prefix {
    Leader,
    Z,
    ShiftZ,
    D,
}

impl Prefix {
    /// The notation an entry's `Keys` run starts with for this prefix.
    fn notation(self) -> &'static str {
        match self {
            Prefix::Leader => "<leader>",
            Prefix::Z => "z",
            Prefix::ShiftZ => "Z",
            Prefix::D => "d",
        }
    }

    pub fn title(self, leader: char) -> String {
        match self {
            Prefix::Leader => format!("{} …", display_leader(leader)),
            Prefix::Z => "z …".to_string(),
            Prefix::ShiftZ => "Z …".to_string(),
            Prefix::D => "d …".to_string(),
        }
    }
}

/// Continuations of a prefix: `(key after the prefix, description)`.
pub fn continuations(prefix: Prefix) -> Vec<(String, &'static str)> {
    let mut out: Vec<(String, &'static str)> = ENTRIES
        .iter()
        .filter_map(|entry| match entry.run {
            Keys(keys) => keys
                .strip_prefix(prefix.notation())
                .filter(|rest| !rest.is_empty())
                .map(|rest| (rest.to_string(), entry.desc)),
            _ => None,
        })
        .collect();
    if prefix == Prefix::ShiftZ {
        out.push(("Z".to_string(), "Save and quit"));
        out.push(("Q".to_string(), "Quit without saving"));
    }
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

pub fn display_leader(leader: char) -> String {
    if leader.is_whitespace() {
        "\u{2423}".to_string()
    } else {
        leader.to_string()
    }
}

/// Keys as shown to the user, with the configured leader filled in.
pub fn display_keys(keys: &str, leader: char) -> String {
    keys.replace("<leader>", &display_leader(leader))
}

/// Description of a `:` command, for the completion popup.
pub fn command_description(name: &str) -> Option<&'static str> {
    ENTRIES.iter().find_map(|entry| match entry.run {
        Cmd(cmd) if cmd == name => Some(entry.desc),
        Prompt(p) if !p.is_empty() && p.trim_end() == name => Some(entry.desc),
        _ => None,
    })
}

/// Parse key notation into events: `<leader>`, `<CR>`, `<S-CR>`, `<Esc>`,
/// `<Tab>`, `<S-Tab>`, `<Space>`, `<BS>`, `<C-x>`, and literal characters.
pub fn parse_keys(notation: &str, leader: char) -> Vec<KeyEvent> {
    let mut out = Vec::new();
    let mut rest = notation;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(end) = rest.find('>')
            && end > 1
        {
            let name = &rest[1..end];
            let event = match name {
                "leader" => Some(KeyEvent::new(KeyCode::Char(leader), KeyModifiers::NONE)),
                "CR" => Some(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                "S-CR" => Some(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
                "Esc" => Some(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
                "Tab" => Some(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
                "S-Tab" => Some(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
                "Space" => Some(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
                "BS" => Some(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
                _ => name
                    .strip_prefix("C-")
                    .and_then(|k| k.chars().next())
                    .map(|k| KeyEvent::new(KeyCode::Char(k), KeyModifiers::CONTROL)),
            };
            if let Some(event) = event {
                out.push(event);
                rest = &rest[end + 1..];
                continue;
            }
        }
        let mods = if c.is_ascii_uppercase() {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        };
        out.push(KeyEvent::new(KeyCode::Char(c), mods));
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// Score `haystack` against a space-separated query: every word must occur
/// (case-insensitive); earlier and word-start hits score higher. `None` when
/// a word is missing.
pub fn match_score(query: &str, haystack: &str) -> Option<i32> {
    let hay = haystack.to_lowercase();
    let mut score = 0;
    for word in query.split_whitespace() {
        let word = word.to_lowercase();
        let pos = hay.find(&word)?;
        let at_word_start = pos == 0
            || hay[..pos]
                .chars()
                .next_back()
                .is_some_and(|c| !c.is_alphanumeric());
        score += 100 - (pos.min(90) as i32) + if at_word_start { 50 } else { 0 };
    }
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_leader_and_named_keys() {
        let keys = parse_keys("<leader>z", ';');
        assert_eq!(keys[0].code, KeyCode::Char(';'));
        assert_eq!(keys[1].code, KeyCode::Char('z'));
        let keys = parse_keys("<C-d><CR>G", ';');
        assert_eq!(
            keys[0],
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)
        );
        assert_eq!(keys[1].code, KeyCode::Enter);
        assert_eq!(
            keys[2],
            KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT)
        );
    }

    #[test]
    fn literal_angle_brackets_survive() {
        let keys = parse_keys("<leader><", ',');
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[1].code, KeyCode::Char('<'));
    }

    #[test]
    fn leader_continuations_include_layout_keys() {
        let next = continuations(Prefix::Leader);
        assert!(next.iter().any(|(k, _)| k == "z"));
        assert!(next.iter().any(|(k, _)| k == "<"));
        assert!(next.iter().any(|(k, _)| k == "b"));
    }

    #[test]
    fn z_continuations_list_pans() {
        let next = continuations(Prefix::Z);
        let keys: Vec<&str> = next.iter().map(|(k, _)| k.as_str()).collect();
        for k in ["z", "t", "b", "H", "L", "s", "e"] {
            assert!(keys.contains(&k), "missing z{k}: {keys:?}");
        }
    }

    #[test]
    fn scoring_requires_every_word() {
        assert!(match_score("fet rem", "Fetch every remote").is_some());
        assert!(match_score("push force", "Push the current branch").is_none());
        let start = match_score("push", "Push the branch").unwrap();
        let mid = match_score("push", "Force push the branch").unwrap();
        assert!(start > mid);
    }

    #[test]
    fn commands_have_descriptions() {
        assert_eq!(command_description("fetch"), Some("Fetch every remote"));
        assert!(command_description("set split").is_some());
    }

    #[test]
    fn every_keys_run_parses() {
        for entry in ENTRIES {
            if let Keys(k) = entry.run {
                assert!(!parse_keys(k, ';').is_empty(), "{}", entry.desc);
            }
        }
    }
}
