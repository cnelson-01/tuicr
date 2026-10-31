use crate::app::{
    DEFAULT_FILE_LIST_PCT, DEFAULT_SBS_SPLIT_PCT, MAX_SBS_SPLIT_PCT, MIN_FILE_LIST_PCT,
    MIN_SBS_SPLIT_PCT, sbs_split_widths,
};

use super::log_pane_tests::build_review_app;

fn app_with_commits(n: usize) -> crate::app::App {
    let ids: Vec<String> = (0..n).map(|i| format!("c{i}")).collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    build_review_app(&refs)
}

#[test]
fn split_widths_keep_every_column_at_fifty_percent() {
    assert_eq!(sbs_split_widths(101, 50), (50, 50));
    assert_eq!(sbs_split_widths(100, 70), (70, 30));
    // Off-center splits give the remainder to the new side.
    assert_eq!(sbs_split_widths(101, 30), (30, 71));
}

#[test]
fn split_widths_clamp_to_the_allowed_range() {
    let (left, _) = sbs_split_widths(100, 2);
    assert_eq!(left, MIN_SBS_SPLIT_PCT as usize);
    let (left, _) = sbs_split_widths(100, 99);
    assert_eq!(left, MAX_SBS_SPLIT_PCT as usize);
}

#[test]
fn adjust_split_and_tree_width_clamp_and_reset() {
    let mut app = app_with_commits(3);
    app.adjust_sbs_split(-100);
    assert_eq!(app.sbs_split_pct, MIN_SBS_SPLIT_PCT);
    app.adjust_file_list_pct(-100);
    assert_eq!(app.file_list_pct, MIN_FILE_LIST_PCT);
    app.reset_layout();
    assert_eq!(app.sbs_split_pct, DEFAULT_SBS_SPLIT_PCT);
    assert_eq!(app.file_list_pct, DEFAULT_FILE_LIST_PCT);
}

#[test]
fn zen_hides_every_panel_and_restores_them() {
    let mut app = app_with_commits(3);
    app.show_file_list = true;
    app.enable_log_pane();
    assert!(app.log_pane.is_some());

    app.toggle_zen();
    assert!(app.zen_active());
    assert!(!app.show_file_list);
    assert!(app.log_pane.is_none());

    app.toggle_zen();
    assert!(!app.zen_active());
    assert!(app.show_file_list);
    assert!(app.log_pane.is_some());
    assert_eq!(app.focused_panel, crate::app::FocusedPanel::Diff);
}

#[test]
fn diff_defaults_to_no_wrap() {
    let app = app_with_commits(1);
    assert!(!app.diff_state.wrap_lines);
}
