use crossterm::event::{KeyCode, KeyModifiers};

use super::log_pane_tests::build_review_app;
use crate::app::{FocusedPanel, InputMode};
use crate::input::catalog::{ENTRIES, Run, Scope};

fn typed(app: &mut crate::app::App, text: &str) {
    for c in text.chars() {
        app.palette_push(c);
    }
}

fn top_entry(app: &crate::app::App) -> &'static crate::input::catalog::Entry {
    let palette = app.palette.as_ref().expect("palette open");
    &ENTRIES[palette.matches[palette.cursor]]
}

#[test]
fn opens_over_the_current_mode_and_restores_it() {
    let mut app = build_review_app(&["a"]);
    app.open_palette();
    assert_eq!(app.input_mode, InputMode::Palette);
    assert_eq!(app.palette.as_ref().unwrap().matches.len(), ENTRIES.len());
    app.close_palette();
    assert_eq!(app.input_mode, InputMode::Normal);
    assert!(app.palette.is_none());
}

#[test]
fn typing_narrows_to_matching_actions() {
    let mut app = build_review_app(&["a"]);
    app.open_palette();
    typed(&mut app, "zen");
    assert_eq!(top_entry(&app).desc, "Zen mode: diff only (again restores)");
    typed(&mut app, " nothing-matches-this");
    assert!(app.palette.as_ref().unwrap().matches.is_empty());
}

#[test]
fn running_a_key_entry_queues_its_keys_with_the_leader() {
    let mut app = build_review_app(&["a"]);
    app.leader_key = ',';
    app.open_palette();
    typed(&mut app, "zen mode");
    app.palette_run();
    assert_eq!(app.input_mode, InputMode::Normal);
    let keys: Vec<KeyCode> = app.injected_keys.iter().map(|k| k.code).collect();
    assert_eq!(keys, vec![KeyCode::Char(','), KeyCode::Char('z')]);
}

#[test]
fn running_a_command_entry_types_it_and_presses_enter() {
    let mut app = build_review_app(&["a"]);
    app.open_palette();
    typed(&mut app, "fetch every remote");
    assert_eq!(top_entry(&app).run, Run::Command("fetch"));
    app.palette_run();
    let text: String = app
        .injected_keys
        .iter()
        .filter_map(|k| match k.code {
            KeyCode::Char(c) => Some(c),
            _ => None,
        })
        .collect();
    assert_eq!(text, ":fetch");
    assert_eq!(app.injected_keys.back().unwrap().code, KeyCode::Enter);
}

#[test]
fn log_entries_focus_the_log_pane_before_replaying() {
    let mut app = build_review_app(&["a"]);
    app.focused_panel = FocusedPanel::Diff;
    app.open_palette();
    typed(&mut app, "reword the commit");
    assert_eq!(top_entry(&app).scope, Scope::Log);
    app.palette_run();
    assert!(app.log_pane.is_some());
    assert_eq!(app.focused_panel, FocusedPanel::CommitSelector);
    let first = app.injected_keys.front().unwrap();
    assert_eq!(
        (first.code, first.modifiers),
        (KeyCode::Char('R'), KeyModifiers::SHIFT)
    );
}

#[test]
fn focused_scope_ranks_first_on_ties() {
    let mut app = build_review_app(&["a"]);
    app.show_file_list = true;
    app.focused_panel = FocusedPanel::FileList;
    app.open_palette();
    assert_eq!(top_entry(&app).scope, Scope::Tree);
}
