use crate::app::{
    App, InputMode, MERGE_SELECTION_ID, STAGED_SELECTION_ID, TargetTab, UNSTAGED_SELECTION_ID,
};
use crate::vcs::traits::VcsChangeStatus;

use super::log_pane_tests::build_review_app;

/// The selector open on its Local tab over `history`, with the pseudo rows
/// `status` calls for on top.
fn selector(history: &[&str], status: VcsChangeStatus) -> App {
    let mut app = build_review_app(history);
    let mut rows = Vec::new();
    if status.unstaged {
        rows.push(App::unstaged_commit_entry());
    }
    if status.staged {
        rows.push(App::staged_commit_entry());
    }
    rows.extend(app.commit_list.iter().cloned());
    app.commit_list = rows;
    app.visible_commit_count = app.commit_list.len();
    app.commit_list_cursor = 0;
    app.commit_list_scroll_offset = 0;
    app.commit_picks.clear();
    app.has_more_commit = false;
    app.input_mode = InputMode::CommitSelect;
    app.target_tab = TargetTab::Local;
    app
}

fn ids(app: &App) -> Vec<&str> {
    app.commit_list.iter().map(|c| c.id.as_str()).collect()
}

const CLEAN: VcsChangeStatus = VcsChangeStatus {
    staged: false,
    unstaged: false,
};
const UNSTAGED: VcsChangeStatus = VcsChangeStatus {
    staged: false,
    unstaged: true,
};
const BOTH: VcsChangeStatus = VcsChangeStatus {
    staged: true,
    unstaged: true,
};

#[test]
fn should_leave_rows_alone_when_the_status_has_not_changed() {
    let mut app = selector(&["a", "b"], UNSTAGED);
    assert!(!app.sync_selector_change_rows(UNSTAGED));
    assert_eq!(ids(&app), [UNSTAGED_SELECTION_ID, "a", "b"]);
}

#[test]
fn should_add_rows_above_the_history_and_keep_the_cursor_on_its_commit() {
    // given the selector opened on a remembered clean tree, cursor on `b`
    let mut app = selector(&["a", "b", "c"], CLEAN);
    app.commit_list_cursor = 1;
    app.commit_picks.insert(2);
    // when the re-read status says there are changes after all
    assert!(app.sync_selector_change_rows(BOTH));
    // then both rows appear in the usual order
    assert_eq!(
        ids(&app),
        [UNSTAGED_SELECTION_ID, STAGED_SELECTION_ID, "a", "b", "c"]
    );
    assert_eq!(app.visible_commit_count, 5);
    // and the cursor and the pick stay on the same commits
    assert_eq!(app.commit_list[app.commit_list_cursor].id, "b");
    assert_eq!(app.commit_picks.iter().copied().collect::<Vec<_>>(), [4]);
}

#[test]
fn should_remove_a_row_that_no_longer_applies() {
    // given both rows showing, cursor on the staged one, a pick on `a`
    let mut app = selector(&["a", "b"], BOTH);
    app.commit_list_cursor = 1;
    app.commit_picks.insert(2);
    // when only unstaged changes remain
    assert!(app.sync_selector_change_rows(UNSTAGED));
    // then the staged row is gone and the cursor falls back to the top
    assert_eq!(ids(&app), [UNSTAGED_SELECTION_ID, "a", "b"]);
    assert_eq!(app.commit_list_cursor, 0);
    assert_eq!(app.visible_commit_count, 3);
    assert_eq!(app.commit_picks.iter().copied().collect::<Vec<_>>(), [1]);
}

#[test]
fn should_keep_a_conflicts_row_on_top() {
    let mut app = selector(&["a"], UNSTAGED);
    let mut conflicts = App::unstaged_commit_entry();
    conflicts.id = MERGE_SELECTION_ID.to_string();
    app.commit_list.insert(0, conflicts);
    app.visible_commit_count = app.commit_list.len();
    assert!(app.sync_selector_change_rows(BOTH));
    assert_eq!(
        ids(&app),
        [
            MERGE_SELECTION_ID,
            UNSTAGED_SELECTION_ID,
            STAGED_SELECTION_ID,
            "a"
        ]
    );
}

#[test]
fn should_keep_the_cursor_on_the_load_more_row() {
    // given more history to page in, cursor parked on the overflow row
    let mut app = selector(&["a", "b"], CLEAN);
    app.has_more_commit = true;
    app.commit_list_cursor = app.visible_commit_count;
    assert!(app.is_on_expand_row());
    // when a row is added above
    assert!(app.sync_selector_change_rows(UNSTAGED));
    // then it is still on the overflow row
    assert!(app.is_on_expand_row());
}

#[test]
fn should_hold_a_probed_status_until_the_selector_rows_are_on_screen() {
    // given an answer that arrived while the command line was open
    let mut app = selector(&["a"], CLEAN);
    app.status_probe_pending = Some(UNSTAGED);
    app.input_mode = InputMode::Command;
    // then nothing moves under the prompt
    assert!(!app.poll_status_probe());
    assert_eq!(ids(&app), ["a"]);
    // when the selector is back
    app.input_mode = InputMode::CommitSelect;
    // then the rows catch up, once
    assert!(app.poll_status_probe());
    assert_eq!(ids(&app), [UNSTAGED_SELECTION_ID, "a"]);
    assert!(!app.poll_status_probe());
}

#[test]
fn should_only_open_on_a_remembered_status_with_background_probing_on() {
    let mut app = selector(&["a"], CLEAN);
    app.last_change_status = Some(UNSTAGED);
    // off by default: tests and any caller without the main loop read first
    assert_eq!(app.remembered_change_status(), None);
    app.background_status_probe = true;
    assert_eq!(app.remembered_change_status(), Some(UNSTAGED));
    // a path filter needs the diff to answer, so it reads first too
    app.path_filter = Some("src".to_string());
    assert_eq!(app.remembered_change_status(), None);
}

#[test]
fn should_drop_an_in_flight_probe_when_the_status_is_read_directly() {
    let mut app = selector(&["a"], CLEAN);
    let (_tx, rx) = std::sync::mpsc::channel();
    app.status_probe_rx = Some(rx);
    app.status_probe_pending = Some(UNSTAGED);
    app.remember_change_status(BOTH);
    assert!(app.status_probe_rx.is_none());
    assert_eq!(app.status_probe_pending, None);
    assert_eq!(app.last_change_status, Some(BOTH));
}
