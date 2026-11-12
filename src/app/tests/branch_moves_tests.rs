//! Switch, reset, rebase and `:undo` with uncommitted changes in the tree,
//! against real repositories: what git carries, refuses and stashes is the
//! behaviour under test, so nothing here is scripted.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::app::*;
use crate::handler::handle_confirm_action;
use crate::input::Action;
use crate::model::review::SessionDiffSource;
use crate::vcs::DiffWhitespaceMode;
use crate::vcs::git::{GitBackend, GitBackendPreference};

/// A repository with three branches off one base commit:
///
/// - `main`: base, then "main: b" (first line of `b.txt`, adds `new.txt`)
/// - `feature`: base, then "feature: c" (first line of `c.txt`)
/// - `clash`: base, then "clash: b" (first line of `b.txt`, differently)
///
/// `main` is checked out.
struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fx = Self {
            dir: tempfile::tempdir().expect("tempdir"),
        };
        fx.git(&["init", "-q", "-b", "main"]);
        fx.git(&["config", "user.name", "tester"]);
        fx.git(&["config", "user.email", "tester@example.com"]);
        fx.git(&["config", "commit.gpgsign", "false"]);
        fx.write("a.txt", "a\n");
        fx.write("b.txt", "b1\nb2\nb3\n");
        fx.write("c.txt", "c1\nc2\nc3\nc4\nc5\n");
        fx.git(&["add", "-A"]);
        fx.git(&["commit", "-q", "-m", "base"]);
        fx.git(&["branch", "feature"]);
        fx.git(&["branch", "clash"]);

        fx.write("b.txt", "B1\nb2\nb3\n");
        fx.write("new.txt", "n\n");
        fx.git(&["add", "-A"]);
        fx.git(&["commit", "-q", "-m", "main: b"]);

        fx.git(&["checkout", "-q", "feature"]);
        fx.write("c.txt", "C1\nc2\nc3\nc4\nc5\n");
        fx.git(&["commit", "-q", "-am", "feature: c"]);

        fx.git(&["checkout", "-q", "clash"]);
        fx.write("b.txt", "X1\nb2\nb3\n");
        fx.git(&["commit", "-q", "-am", "clash: b"]);

        fx.git(&["checkout", "-q", "main"]);
        fx
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(self.root())
            .args(args)
            .output()
            .expect("failed to run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        // Leading blanks matter in `status --porcelain`.
        String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string()
    }

    fn write(&self, name: &str, content: &str) {
        std::fs::write(self.root().join(name), content).expect("write");
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.root().join(name)).expect("read")
    }

    fn id(&self, rev: &str) -> String {
        self.git(&["rev-parse", rev])
    }

    fn branch(&self) -> String {
        self.git(&["rev-parse", "--abbrev-ref", "HEAD"])
    }

    fn status(&self) -> String {
        self.git(&["status", "--porcelain"])
    }

    fn stashes(&self) -> Vec<String> {
        // The subjects `git stash list` shows.
        self.git(&["stash", "list", "--format=%gs"])
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// An app whose backend is this repository, and which reopens it here
    /// (not in the test process's working directory) after every operation.
    fn app(&self) -> App {
        let backend = GitBackend::discover_from(
            self.root(),
            GitBackendPreference::Libgit2,
            DiffWhitespaceMode::Normal,
        )
        .expect("open the fixture");
        let vcs_info = crate::vcs::VcsBackend::info(&backend).clone();
        let session = ReviewSession::new(
            vcs_info.root_path.clone(),
            vcs_info.head_commit.clone(),
            vcs_info.branch_name.clone(),
            SessionDiffSource::CommitRange,
        );
        let head = vcs_info.head_commit.clone();
        let mut app = App::build(
            Box::new(backend),
            vcs_info,
            Theme::dark(),
            None,
            false,
            Vec::new(),
            session,
            DiffSource::CommitRange(vec![head]),
            InputMode::Normal,
            Vec::new(),
            None,
            None,
        )
        .expect("failed to build test app");
        app.vcs_reopen_root = Some(PathBuf::from(self.root()));
        app
    }
}

fn message(app: &App) -> String {
    app.message
        .as_ref()
        .map(|m| m.content.clone())
        .unwrap_or_default()
}

fn choice(app: &App) -> &ChoicePrompt {
    assert_eq!(app.input_mode, InputMode::Confirm, "{}", message(app));
    assert_eq!(app.pending_confirm, Some(ConfirmAction::Choice));
    app.pending_choice.as_ref().expect("a choice prompt")
}

fn files_in_the_way(app: &App) -> Vec<String> {
    match &choice(app).then {
        ChoiceThen::InTheWay { files, .. } => files.clone(),
        other => panic!("expected changes in the way, got {other:?}"),
    }
}

// ── Switch ───────────────────────────────────────────────────────────────

#[test]
fn a_switch_takes_changes_along_when_nothing_is_in_the_way() {
    let fx = Fixture::new();
    fx.write("a.txt", "a local\n");
    fx.write("untracked.txt", "u\n");
    let mut app = fx.app();

    app.checkout_target("feature", false);

    assert_eq!(app.input_mode, InputMode::Normal, "no prompt");
    assert_eq!(fx.branch(), "feature");
    assert_eq!(app.vcs_info.branch_name.as_deref(), Some("feature"));
    assert_eq!(fx.read("a.txt"), "a local\n");
    assert_eq!(fx.read("untracked.txt"), "u\n");
    assert!(fx.stashes().is_empty());
    assert_eq!(message(&app), "Switched to feature");
}

#[test]
fn an_untracked_file_alone_never_blocks_a_switch() {
    let fx = Fixture::new();
    fx.write("untracked.txt", "u\n");
    let mut app = fx.app();

    app.checkout_target("feature", false);

    assert_eq!(fx.branch(), "feature");
    assert_eq!(app.input_mode, InputMode::Normal);
}

#[test]
fn a_switch_asks_about_the_files_in_the_way_and_stash_puts_them_back() {
    let fx = Fixture::new();
    // `c.txt` differs between main and feature; this edit is far enough
    // from feature's to merge.
    fx.write("c.txt", "c1\nc2\nc3\nc4\nc5 local\n");
    fx.write("a.txt", "a local\n");
    let mut app = fx.app();

    app.checkout_target("feature", false);

    assert_eq!(files_in_the_way(&app), vec!["c.txt"]);
    let prompt = choice(&app);
    assert_eq!(
        prompt.title,
        "1 changed file is in the way of switching to feature"
    );
    assert_eq!(prompt.default, 's');
    assert_eq!(
        prompt.options.iter().map(|o| o.key).collect::<Vec<_>>(),
        vec!['s', 'd']
    );
    assert_eq!(fx.branch(), "main", "nothing moves until the answer");

    app.choose_default();

    assert_eq!(app.input_mode, InputMode::Normal);
    assert_eq!(fx.branch(), "feature");
    assert_eq!(fx.read("c.txt"), "C1\nc2\nc3\nc4\nc5 local\n");
    assert_eq!(fx.read("a.txt"), "a local\n");
    assert!(fx.stashes().is_empty(), "the stash was popped");
    assert_eq!(
        message(&app),
        "Switched to feature \u{00b7} 1 changed file came along"
    );
}

#[test]
fn discard_switches_without_the_files_and_leaves_them_in_the_stash_list() {
    let fx = Fixture::new();
    fx.write("c.txt", "c1\nc2\nc3\nc4\nc5 local\n");
    fx.write("a.txt", "a local\n");
    let mut app = fx.app();

    app.checkout_target("feature", false);
    app.choose('d');

    assert_eq!(fx.branch(), "feature");
    assert_eq!(fx.read("c.txt"), "C1\nc2\nc3\nc4\nc5\n");
    assert_eq!(fx.read("a.txt"), "a local\n", "only c.txt was in the way");
    assert_eq!(
        fx.stashes(),
        vec!["On main: tuicr: set aside while switching to feature"]
    );
    assert!(message(&app).contains("1 changed file set aside in stash@{0}"));
}

#[test]
fn cancelling_the_prompt_leaves_the_tree_alone() {
    let fx = Fixture::new();
    fx.write("c.txt", "c1\nc2\nc3\nc4\nc5 local\n");
    let mut app = fx.app();

    app.checkout_target("feature", false);
    app.cancel_choice();

    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(app.pending_choice.is_none());
    assert_eq!(fx.branch(), "main");
    assert_eq!(fx.read("c.txt"), "c1\nc2\nc3\nc4\nc5 local\n");
    assert!(fx.stashes().is_empty());
}

#[test]
fn checkout_bang_stashes_and_puts_back_without_asking() {
    let fx = Fixture::new();
    fx.write("c.txt", "c1\nc2\nc3\nc4\nc5 local\n");
    let mut app = fx.app();

    app.checkout_target("feature", true);

    assert_eq!(app.input_mode, InputMode::Normal);
    assert_eq!(fx.branch(), "feature");
    assert_eq!(fx.read("c.txt"), "C1\nc2\nc3\nc4\nc5 local\n");
    assert!(fx.stashes().is_empty());
}

#[test]
fn staged_changes_are_still_staged_after_being_carried() {
    let fx = Fixture::new();
    fx.write("c.txt", "c1\nc2\nc3\nc4\nc5 local\n");
    fx.git(&["add", "c.txt"]);
    let mut app = fx.app();

    app.checkout_target("feature", true);

    assert_eq!(fx.branch(), "feature");
    assert_eq!(fx.status(), "M  c.txt");
}

#[test]
fn an_untracked_file_the_branch_has_is_stashed_and_named() {
    let fx = Fixture::new();
    fx.git(&["checkout", "-q", "feature"]);
    // main tracks new.txt; here it is an untracked file of ours.
    fx.write("new.txt", "mine\n");
    let mut app = fx.app();

    app.checkout_target("main", false);
    assert_eq!(files_in_the_way(&app), vec!["new.txt"]);
    app.choose('d');

    assert_eq!(fx.branch(), "main");
    assert_eq!(fx.read("new.txt"), "n\n");
    assert_eq!(fx.stashes().len(), 1, "ours is in the stash list");
}

#[test]
fn an_untracked_file_that_cannot_go_back_stays_stashed_and_says_so() {
    let fx = Fixture::new();
    fx.git(&["checkout", "-q", "feature"]);
    // main tracks new.txt, so ours has nowhere to go back to there.
    fx.write("new.txt", "mine\n");
    let mut app = fx.app();

    app.checkout_target("main", false);
    app.choose('s');

    assert_eq!(fx.branch(), "main");
    assert_eq!(fx.read("new.txt"), "n\n", "main's file is untouched");
    assert_eq!(fx.status(), "", "nothing was left half applied");
    assert_eq!(fx.stashes().len(), 1, "ours is in the stash list");
    let message = app.message.as_ref().expect("a message");
    assert_eq!(message.message_type, MessageType::Warning);
    assert_eq!(
        message.content,
        "Switched to main \u{00b7} 1 changed file could not be put back here: it stays in stash@{0} (:stashes)"
    );
}

#[test]
fn changes_that_conflict_when_put_back_stay_in_the_stash_list() {
    let fx = Fixture::new();
    // The same line feature rewrote.
    fx.write("c.txt", "c1 local\nc2\nc3\nc4\nc5\n");
    let mut app = fx.app();

    app.checkout_target("feature", false);
    app.choose('s');

    assert_eq!(fx.branch(), "feature");
    assert_eq!(fx.stashes().len(), 1, "git keeps a stash that conflicts");
    assert!(fx.read("c.txt").contains("<<<<<<<"));
    let message = app.message.as_ref().expect("a message");
    assert_eq!(message.message_type, MessageType::Warning);
    assert!(
        message
            .content
            .contains("conflicted when put back: resolve them (:merge)"),
        "{}",
        message.content
    );
}

#[test]
fn a_checkout_that_fails_for_another_reason_reports_it() {
    let fx = Fixture::new();
    let mut app = fx.app();

    app.checkout_target("no-such-branch", false);

    assert_eq!(app.input_mode, InputMode::Normal);
    let message = app.message.as_ref().expect("a message");
    assert_eq!(message.message_type, MessageType::Error);
    assert!(message.content.starts_with("Checkout failed:"));
}

// ── Rebase ───────────────────────────────────────────────────────────────

#[test]
fn a_rebase_starts_with_uncommitted_changes_and_undo_puts_the_branch_back() {
    let fx = Fixture::new();
    fx.git(&["checkout", "-q", "feature"]);
    fx.write("a.txt", "a local\n");
    let before = fx.id("feature");
    let mut app = fx.app();

    app.rebase_current_onto("main");

    assert_eq!(fx.id("feature~1"), fx.id("main"), "feature sits on main");
    assert_eq!(fx.read("a.txt"), "a local\n", "the autostash went back");
    assert!(fx.stashes().is_empty());
    assert_eq!(
        message(&app),
        "Rebased feature onto main \u{00b7} :undo puts it back"
    );
    let last = app.last_branch_move.clone().expect("recorded");
    assert_eq!(
        (last.from.as_str(), last.what.as_str()),
        (before.as_str(), "rebase")
    );

    app.undo();

    assert_eq!(fx.id("feature"), before);
    assert_eq!(fx.branch(), "feature");
    assert_eq!(fx.read("a.txt"), "a local\n");
    assert!(message(&app).starts_with("feature moved back to"));
}

#[test]
fn an_autostash_that_does_not_go_back_is_reported_not_lost() {
    let fx = Fixture::new();
    fx.git(&["checkout", "-q", "feature"]);
    // main rewrote this line of b.txt.
    fx.write("b.txt", "b1 mine\nb2\nb3\n");
    let mut app = fx.app();

    app.rebase_current_onto("main");

    assert_eq!(fx.id("feature~1"), fx.id("main"), "the rebase finished");
    assert_eq!(fx.stashes(), vec!["autostash"]);
    let message = app.message.as_ref().expect("a message");
    assert_eq!(message.message_type, MessageType::Warning);
    assert!(
        message
            .content
            .contains("your uncommitted changes conflicted when put back"),
        "{}",
        message.content
    );
}

#[test]
fn a_rebase_that_stops_is_recorded_once_continue_finishes_it() {
    let fx = Fixture::new();
    fx.git(&["checkout", "-q", "clash"]);
    let before = fx.id("clash");
    let mut app = fx.app();

    app.rebase_current_onto("main");
    assert!(app.last_branch_move.is_none(), "still in progress");
    assert_eq!(
        app.move_in_flight
            .as_ref()
            .map(|(at, what)| (at.id.as_str(), what.as_str())),
        Some((before.as_str(), "rebase"))
    );

    fx.write("b.txt", "resolved\nb2\nb3\n");
    fx.git(&["add", "b.txt"]);
    app.continue_operation();

    assert!(app.move_in_flight.is_none());
    assert_eq!(fx.branch(), "clash");
    let last = app.last_branch_move.clone().expect("recorded");
    assert_eq!(last.from, before);
    assert_eq!(last.at.id, fx.id("clash"));

    app.undo();
    assert_eq!(fx.id("clash"), before);
}

#[test]
fn aborting_a_stopped_rebase_forgets_it() {
    let fx = Fixture::new();
    fx.git(&["checkout", "-q", "clash"]);
    let before = fx.id("clash");
    let mut app = fx.app();

    app.rebase_current_onto("main");
    app.abort_operation();

    assert!(app.move_in_flight.is_none());
    assert!(app.last_branch_move.is_none());
    assert_eq!(fx.id("clash"), before);
}

#[test]
fn a_rebase_that_ended_outside_tuicr_is_not_what_undo_puts_back() {
    let fx = Fixture::new();
    fx.git(&["checkout", "-q", "clash"]);
    let mut app = fx.app();

    // A rebase stops on conflicts, and is aborted from a shell; work
    // carries on there.
    app.rebase_current_onto("main");
    assert!(app.move_in_flight.is_some());
    fx.git(&["rebase", "--abort"]);
    fx.write("a.txt", "a more\n");
    fx.git(&["commit", "-q", "-am", "clash: more work"]);
    let with_work = fx.id("clash");
    app.after_head_moved();

    // The next rebase is a move of its own, from where the branch is now.
    app.rebase_current_onto("feature");

    assert_eq!(fx.id("clash~2"), fx.id("feature"), "{}", message(&app));
    assert!(app.move_in_flight.is_none());
    let last = app.last_branch_move.clone().expect("recorded");
    assert_eq!(last.from, with_work);

    app.undo();
    assert_eq!(fx.id("clash"), with_work, "the later commit is still there");
}

#[test]
fn a_rebase_that_stops_after_one_ended_outside_starts_its_own_record() {
    let fx = Fixture::new();
    fx.git(&["checkout", "-q", "clash"]);
    let mut app = fx.app();

    app.rebase_current_onto("main");
    fx.git(&["rebase", "--abort"]);
    fx.write("a.txt", "a more\n");
    fx.git(&["commit", "-q", "-am", "clash: more work"]);
    let with_work = fx.id("clash");
    app.after_head_moved();

    // This one stops too: what is in flight is this rebase, not the old one.
    app.rebase_current_onto("main");

    assert_eq!(
        app.move_in_flight.as_ref().map(|(at, _)| at.id.as_str()),
        Some(with_work.as_str())
    );
}

// ── Reset ────────────────────────────────────────────────────────────────

#[test]
fn reset_without_a_mode_asks_and_says_what_each_answer_does() {
    let fx = Fixture::new();
    fx.write("a.txt", "a local\n");
    let base = fx.id("main~1");
    let mut app = fx.app();

    app.reset_command(&base);

    let prompt = choice(&app);
    assert_eq!(
        prompt.title,
        format!("Reset main to {} \"base\"", &base[..7])
    );
    assert_eq!(
        prompt.detail,
        vec!["1 commit leaves the branch \u{00b7} 1 file has uncommitted changes"]
    );
    assert_eq!(
        prompt
            .options
            .iter()
            .map(|o| (o.key, o.label))
            .collect::<Vec<_>>(),
        vec![('k', "keep"), ('h', "hard"), ('m', "mixed"), ('s', "soft")]
    );
    assert_eq!(prompt.default, 'k');
    assert_ne!(fx.id("main"), base, "nothing moves until the answer");
}

#[test]
fn the_chooser_says_when_commits_arrive_and_the_tree_is_clean() {
    let fx = Fixture::new();
    let mut app = fx.app();

    app.reset_command("feature");

    let prompt = choice(&app);
    assert_eq!(
        prompt.detail,
        vec![
            "1 commit leaves the branch \u{00b7} 1 commit arrives \u{00b7} no uncommitted changes"
        ]
    );
    assert!(
        prompt.options[1]
            .description
            .contains("the same as keep here")
    );
}

#[test]
fn keep_moves_the_branch_with_the_changes_and_undo_moves_it_back() {
    let fx = Fixture::new();
    fx.write("a.txt", "a local\n");
    let tip = fx.id("main");
    let base = fx.id("main~1");
    let mut app = fx.app();

    app.reset_command(&base);
    app.choose_default();

    assert_eq!(app.input_mode, InputMode::Normal);
    assert_eq!(fx.id("main"), base);
    assert_eq!(fx.read("a.txt"), "a local\n");
    assert_eq!(
        fx.read("b.txt"),
        "b1\nb2\nb3\n",
        "the files follow the branch"
    );
    assert_eq!(
        message(&app),
        format!(
            "main \u{2192} {} \"base\" \u{00b7} was {}, :undo puts it back",
            &base[..7],
            &tip[..7]
        )
    );

    app.undo();
    assert_eq!(fx.id("main"), tip);
    assert_eq!(fx.read("a.txt"), "a local\n");
    assert!(message(&app).ends_with(":undo again reverses this"));

    app.undo();
    assert_eq!(fx.id("main"), base, "undo again re-does the reset");
}

#[test]
fn hard_saves_what_it_discards_and_undo_brings_it_back() {
    let fx = Fixture::new();
    fx.write("a.txt", "a local\n");
    fx.write("b.txt", "B1\nb2\nb3 staged\n");
    fx.git(&["add", "b.txt"]);
    fx.write("untracked.txt", "u\n");
    let tip = fx.id("main");
    let base = fx.id("main~1");
    let mut app = fx.app();

    app.reset_command(&format!("hard {base}"));

    assert_eq!(app.input_mode, InputMode::Normal, "hard does not ask twice");
    assert_eq!(fx.id("main"), base);
    assert_eq!(fx.status(), "?? untracked.txt", "tracked files are clean");
    assert_eq!(
        fx.stashes(),
        vec![format!("tuicr: before reset --hard to {}", &base[..7])]
    );
    assert!(message(&app).contains("discarded changes saved as stash@{0}"));

    app.undo();

    assert_eq!(fx.id("main"), tip);
    assert_eq!(fx.read("a.txt"), "a local\n");
    assert_eq!(fx.read("b.txt"), "B1\nb2\nb3 staged\n");
    assert!(fx.stashes().is_empty(), "the saved changes were popped");
    assert!(message(&app).contains("the discarded changes are back"));
}

#[test]
fn hard_on_the_same_commit_can_still_be_undone() {
    let fx = Fixture::new();
    fx.write("a.txt", "a local\n");
    let tip = fx.id("main");
    let mut app = fx.app();

    app.reset_command(&format!("hard {tip}"));
    assert_eq!(fx.status(), "");
    assert!(message(&app).ends_with(":undo puts them back"));

    app.undo();
    assert_eq!(fx.read("a.txt"), "a local\n");
    assert!(fx.stashes().is_empty());
}

#[test]
fn hard_sets_aside_an_untracked_file_the_target_would_overwrite() {
    let fx = Fixture::new();
    fx.git(&["checkout", "-q", "feature"]);
    fx.write("new.txt", "mine\n");
    let main = fx.id("main");
    let mut app = fx.app();

    app.reset_command(&format!("hard {main}"));

    assert_eq!(fx.id("feature"), main);
    assert_eq!(fx.read("new.txt"), "n\n");
    assert_eq!(fx.stashes().len(), 1);

    app.undo();
    assert_eq!(fx.read("new.txt"), "mine\n", "our untracked file is back");
    assert!(fx.stashes().is_empty());
}

#[test]
fn keep_asks_when_a_changed_file_differs_in_the_target() {
    let fx = Fixture::new();
    fx.write("b.txt", "B1\nb2\nb3 local\n");
    fx.write("a.txt", "a local\n");
    let base = fx.id("main~1");
    let mut app = fx.app();

    app.reset_command(&format!("keep {base}"));

    assert_eq!(files_in_the_way(&app), vec!["b.txt"]);
    assert_ne!(fx.id("main"), base);

    app.choose('s');

    assert_eq!(fx.id("main"), base);
    assert_eq!(fx.read("b.txt"), "b1\nb2\nb3 local\n", "carried onto base");
    assert_eq!(fx.read("a.txt"), "a local\n");
    assert!(fx.stashes().is_empty());
    assert!(message(&app).contains("1 changed file came along"));
}

#[test]
fn soft_and_mixed_resets_are_undone_the_same_way_they_were_made() {
    let fx = Fixture::new();
    let tip = fx.id("main");
    let base = fx.id("main~1");
    let mut app = fx.app();

    app.reset_command(&format!("soft {base}"));
    assert_eq!(fx.id("main"), base);
    assert_eq!(fx.status(), "M  b.txt\nA  new.txt");
    app.undo();
    assert_eq!(fx.id("main"), tip);
    assert_eq!(fx.status(), "");

    app.last_branch_move = None;
    app.reset_command(&format!("mixed {base}"));
    assert_eq!(fx.id("main"), base);
    assert_eq!(fx.status(), " M b.txt\n?? new.txt");
    app.undo();
    assert_eq!(fx.id("main"), tip);
    assert_eq!(fx.status(), "");
}

#[test]
fn a_name_that_is_not_a_commit_is_refused() {
    let fx = Fixture::new();
    let mut app = fx.app();

    app.reset_command("hard no-such-thing");

    assert_eq!(message(&app), "no-such-thing does not name a commit");
    assert!(fx.stashes().is_empty());
}

#[test]
fn the_prompt_is_answered_through_the_confirm_keys() {
    let fx = Fixture::new();
    let base = fx.id("main~1");
    let mut app = fx.app();

    app.reset_command(&base);
    handle_confirm_action(&mut app, Action::ConfirmKey('z'));
    assert_eq!(app.input_mode, InputMode::Confirm, "not an answer");
    handle_confirm_action(&mut app, Action::ConfirmNo);
    assert_eq!(app.input_mode, InputMode::Normal);
    assert_ne!(fx.id("main"), base);

    app.reset_command(&base);
    handle_confirm_action(&mut app, Action::ConfirmKey('S'));
    assert_eq!(fx.id("main"), base);
    assert_eq!(fx.status(), "M  b.txt\nA  new.txt", "soft");
}

// ── Undo ─────────────────────────────────────────────────────────────────

#[test]
fn undo_with_nothing_to_put_back_takes_the_last_commit_back() {
    let fx = Fixture::new();
    let base = fx.id("main~1");
    let mut app = fx.app();

    app.undo();

    assert_eq!(fx.id("main"), base);
    assert_eq!(fx.status(), "M  b.txt\nA  new.txt");
    assert_eq!(
        message(&app),
        "Undid the last commit; its changes are staged"
    );
}

#[test]
fn undo_ignores_a_branch_move_once_head_has_moved_on() {
    let fx = Fixture::new();
    let base = fx.id("main~1");
    let mut app = fx.app();
    app.reset_command(&format!("keep {base}"));
    assert!(app.last_branch_move.is_some());

    fx.write("later.txt", "later\n");
    fx.git(&["add", "later.txt"]);
    fx.git(&["commit", "-q", "-m", "later"]);
    app.undo();

    assert_eq!(fx.id("main"), base, "the later commit was taken back");
    assert_eq!(fx.status(), "A  later.txt");
    assert!(app.last_branch_move.is_none());
}

#[test]
fn a_failed_switch_does_not_forget_the_last_branch_move() {
    let fx = Fixture::new();
    let tip = fx.id("main");
    let base = fx.id("main~1");
    let mut app = fx.app();
    app.reset_command(&format!("keep {base}"));

    app.checkout_target("no-such-branch", false);
    app.undo();

    assert_eq!(fx.id("main"), tip);
}

// ── What is drawn ────────────────────────────────────────────────────────

fn screen(app: &mut App, width: u16, height: u16) -> Vec<String> {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| crate::ui::render(frame, app))
        .expect("draw frame");
    let buffer = terminal.backend().buffer().clone();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn the_reset_prompt_draws_every_answer_and_marks_the_default() {
    let fx = Fixture::new();
    fx.write("a.txt", "a local\n");
    let base = fx.id("main~1");
    let mut app = fx.app();
    app.reset_command(&base);

    let rows = screen(&mut app, 110, 30);
    let find = |needle: &str| {
        rows.iter()
            .find(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("no row with {needle:?} in:\n{}", rows.join("\n")))
            .clone()
    };

    assert!(find(" Reset ").contains('\u{250c}'), "the border title");
    find(&format!("Reset main to {} \"base\"", &base[..7]));
    find("1 commit leaves the branch \u{00b7} 1 file has uncommitted changes");
    assert!(find("k  keep   move the branch, keep my uncommitted changes").contains("(Enter)"));
    find("h  hard   move the branch, discard uncommitted changes (saved as a stash)");
    find("m  mixed  move the branch, the 1 commit's changes become unstaged");
    find("s  soft   move the branch, the 1 commit's changes become staged");
    find("Esc cancels");
    // The status bar lists the same keys.
    let hints = rows.last().expect("status bar");
    assert!(
        hints.contains("k keep") && hints.contains("h hard"),
        "{hints}"
    );
}

#[test]
fn the_prompt_for_changes_in_the_way_names_the_files_even_on_a_short_screen() {
    let fx = Fixture::new();
    fx.write("c.txt", "c1\nc2\nc3\nc4\nc5 local\n");
    let mut app = fx.app();
    app.checkout_target("feature", false);

    let rows = screen(&mut app, 80, 16);
    let all = rows.join("\n");
    assert!(
        all.contains("1 changed file is in the way of switching to feature"),
        "{all}"
    );
    assert!(all.contains("c.txt"), "{all}");
    assert!(
        all.contains("s  stash    set them aside, switch, put them back  (Enter)"),
        "{all}"
    );
    assert!(
        all.contains("d  discard  switch without them (they stay in the stash list)"),
        "{all}"
    );
}

#[test]
fn confirm_mode_passes_other_keys_through_as_answers() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    let map = |code| crate::input::map_key_to_action(key(code), InputMode::Confirm, ';');

    assert_eq!(map(KeyCode::Char('k')), Action::ConfirmKey('k'));
    assert_eq!(map(KeyCode::Enter), Action::ConfirmYes);
    assert_eq!(map(KeyCode::Esc), Action::ConfirmNo);
    assert_eq!(
        crate::input::map_key_to_action(
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
            InputMode::Confirm,
            ';'
        ),
        Action::None
    );
}
