use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::Block,
};

use crate::app::{App, ConfirmAction, DiscardTarget, InputMode};
use crate::ui::comment_navigator::render_comment_navigator;
use crate::ui::diff_view::render_diff_view;
use crate::ui::file_list::render_file_list;
use crate::ui::inline_commit_selector::render_inline_commit_selector;
use crate::ui::log_pane::render_log_pane;
use crate::ui::selector::render_commit_select;
use crate::ui::{
    branch_picker, comment_panel, console, find_results, help_popup, merge_conflict, palette,
    rebase_editor, stash_picker, status_bar, styles, submit_modals, summary_popup, theme_picker,
};

const FILE_LIST_MIN_HEIGHT: u16 = 4;
const COMMENT_NAVIGATOR_MIN_HEIGHT: u16 = 4;
const COMMENT_NAVIGATOR_MAX_HEIGHT: u16 = 12;

pub fn render(frame: &mut Frame, app: &mut App) {
    // Color what this frame shows before any line is built; see
    // `App::color_visible_hunks`.
    app.color_visible_hunks(frame.area().height as usize);

    frame.render_widget(
        Block::default().style(styles::panel_style(&app.theme)),
        frame.area(),
    );

    if app.input_mode == InputMode::MessageDetails {
        help_popup::render_message_details(frame, app);
        return;
    }

    let selector_background = app.input_mode == InputMode::CommitSelect
        || (app.input_mode == InputMode::Confirm
            && app.pending_confirm.map(ConfirmAction::return_mode)
                == Some(InputMode::CommitSelect))
        || (app.input_mode == InputMode::Command
            && app.command_return_mode == InputMode::CommitSelect)
        || (app.input_mode == InputMode::Help
            && app.overlay_return_mode == InputMode::CommitSelect)
        || (app.searching_help() && app.overlay_return_mode == InputMode::CommitSelect)
        || (app.input_mode == InputMode::BranchPicker
            && app
                .branch_picker
                .as_ref()
                .is_some_and(|p| p.return_mode == InputMode::CommitSelect))
        || (app.input_mode == InputMode::Confirm
            && app
                .pending_history
                .as_ref()
                .is_some_and(|(_, mode)| *mode == InputMode::CommitSelect))
        || (app.input_mode == InputMode::Palette
            && app
                .palette
                .as_ref()
                .is_some_and(|p| p.return_mode == InputMode::CommitSelect));
    if selector_background {
        render_commit_select(frame, app);
        let area = frame.area();
        let footer = Rect::new(
            area.x,
            area.bottom().saturating_sub(1),
            area.width,
            area.height.min(1),
        );
        if matches!(app.input_mode, InputMode::Command | InputMode::Search) {
            status_bar::render_status_bar(frame, app, footer);
            status_bar::render_command_completion_popup(frame, app, footer);
        }
        if app.input_mode == InputMode::Help || app.searching_help() {
            help_popup::render_help(frame, app);
        }
        if app.input_mode == InputMode::BranchPicker {
            branch_picker::render_branch_picker(frame, app);
        }
        if app.input_mode == InputMode::Palette {
            palette::render_palette(frame, app);
        }
        render_active_confirm(frame, app);
        return;
    }

    // The merge conflict screen is fullscreen, like the selector. The
    // commit prompt (Confirm mode with a live merge view) renders the
    // dialog on top of it.
    if app.input_mode == InputMode::MergeConflict
        || (app.input_mode == InputMode::Confirm && app.merge_view.is_some())
    {
        merge_conflict::render_merge_conflict(frame, app);
        render_active_confirm(frame, app);
        return;
    }

    // Clear cursor position before rendering (will be set if in Comment mode)
    app.comment_cursor_screen_pos = None;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![
            Constraint::Length(1), // Header
            Constraint::Min(0),    // Main content
            Constraint::Length(1), // Status bar (also shows command input in command mode)
        ])
        .split(frame.area());

    status_bar::render_header(frame, app, chunks[0]);
    render_main_content(frame, app, chunks[1]);
    if app.input_mode == InputMode::Console {
        console::render_console(frame, app, chunks[1]);
    }
    status_bar::render_status_bar(frame, app, chunks[2]);
    status_bar::render_command_completion_popup(frame, app, chunks[2]);
    palette::render_which_key(frame, app, chunks[2]);

    // Keep help visible while its search prompt is active.
    if app.input_mode == InputMode::Help || app.searching_help() {
        help_popup::render_help(frame, app);
    }

    if app.input_mode == InputMode::BranchPicker {
        branch_picker::render_branch_picker(frame, app);
    }
    if app.input_mode == InputMode::Palette {
        palette::render_palette(frame, app);
    }
    if app.input_mode == InputMode::RebaseEditor {
        rebase_editor::render_rebase_editor(frame, app);
    }
    // The stash picker stays under its own drop-confirmation prompt.
    if app.input_mode == InputMode::StashPicker
        || (app.input_mode == InputMode::Confirm
            && app.pending_confirm == Some(ConfirmAction::DropStash))
    {
        stash_picker::render_stash_picker(frame, app);
    }

    // Comment input is now rendered inline in the diff view

    render_active_confirm(frame, app);

    // Submit-flow modals.
    if app.input_mode == InputMode::SubmitResolver {
        submit_modals::render_submit_resolver(frame, app);
    }
    if app.input_mode == InputMode::SubmitConfirm {
        submit_modals::render_submit_confirm(frame, app);
    }
    if app.input_mode == InputMode::SubmitActionPicker {
        submit_modals::render_submit_action_picker(frame, app);
    }
    if app.input_mode == InputMode::ThemePicker {
        theme_picker::render_theme_picker(frame, app);
    }

    // Position terminal cursor for IME when in Comment mode
    // Always set a cursor position to prevent IME from showing at (0,0)
    if app.input_mode == InputMode::Comment {
        let (col, row) = app.comment_cursor_screen_pos.unwrap_or_else(|| {
            // Fallback: position cursor in the diff area or at a reasonable default
            // Use the diff area if available, otherwise use the main content area
            if let Some(diff_area) = app.diff_area {
                // Position at the start of the diff inner area (after border)
                (diff_area.x + 1, diff_area.y + 1)
            } else {
                // Last resort: position at the main content area
                (chunks[1].x + 1, chunks[1].y + 1)
            }
        });
        frame.set_cursor_position(ratatui::layout::Position { x: col, y: row });
    }
}

fn render_main_content(frame: &mut Frame, app: &mut App, area: Rect) {
    let full_width_view = matches!(app.input_mode, InputMode::Summary | InputMode::FindResults);
    let content_area = if !full_width_view && app.log_pane_active() {
        // The persistent log pane replaces the inline selector while it is
        // open; its height is user-set but always leaves the diff some room.
        app.commit_list_inner_area = None;
        let height = app.log_pane_rows(area.height);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(height), Constraint::Min(0)])
            .split(area);
        render_log_pane(frame, app, chunks[0]);
        chunks[1]
    } else if !full_width_view && app.has_inline_commit_selector() {
        if let Some(pane) = app.log_pane.as_mut() {
            pane.inner_area = None;
        }
        let selector_height = (app.review_commits.len() as u16 + 2).min(8); // N items + 2 borders, capped
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(selector_height), Constraint::Min(0)])
            .split(area);
        render_inline_commit_selector(frame, app, chunks[0]);
        chunks[1]
    } else {
        app.commit_list_inner_area = None;
        if let Some(pane) = app.log_pane.as_mut() {
            pane.inner_area = None;
        }
        area
    };

    app.content_area = Some(content_area);
    if app.show_file_list {
        let tree_pct = app.file_list_pct;
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(tree_pct),       // File list
                Constraint::Percentage(100 - tree_pct), // Diff view
            ])
            .split(content_area);

        let comment_items = app.build_comment_navigator_items();
        if !comment_items.is_empty()
            && chunks[0].height >= FILE_LIST_MIN_HEIGHT + COMMENT_NAVIGATOR_MIN_HEIGHT
        {
            let available_comment_height = chunks[0].height.saturating_sub(FILE_LIST_MIN_HEIGHT);
            let max_comment_height = COMMENT_NAVIGATOR_MAX_HEIGHT.min(available_comment_height);
            let desired_comment_height = comment_items.len() as u16 + 2;
            let comment_height = desired_comment_height
                .min(max_comment_height)
                .max(COMMENT_NAVIGATOR_MIN_HEIGHT);
            let left_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(FILE_LIST_MIN_HEIGHT),
                    Constraint::Length(comment_height),
                ])
                .split(chunks[0]);

            app.file_list_area = Some(left_chunks[0]);
            app.comment_navigator_area = Some(left_chunks[1]);
            render_file_list(frame, app, left_chunks[0]);
            render_comment_navigator(frame, app, left_chunks[1], &comment_items);
        } else {
            app.file_list_area = Some(chunks[0]);
            app.comment_navigator_area = None;
            app.comment_navigator_inner_area = None;
            render_file_list(frame, app, chunks[0]);
        }
        render_content_view(frame, app, chunks[1]);
    } else {
        app.file_list_area = None;
        app.comment_navigator_area = None;
        app.comment_navigator_inner_area = None;
        render_content_view(frame, app, content_area);
    }
}

fn render_content_view(frame: &mut Frame, app: &mut App, area: Rect) {
    app.diff_area = Some(area);
    if app.input_mode == InputMode::Summary {
        summary_popup::render_summary(frame, app, area);
    } else if app.input_mode == InputMode::FindResults {
        find_results::render_find_results(frame, app, area);
    } else {
        render_diff_view(frame, app, area);
    }
}

/// The prompt to draw, if keys are currently routed to it. Gating on the mode
/// as well as `pending_confirm` keeps a prompt that outlived its mode (e.g. a
/// PR open landing mid-prompt) from being drawn where Y/N cannot reach it.
fn active_confirm(app: &App) -> Option<ConfirmAction> {
    (app.input_mode == InputMode::Confirm)
        .then_some(app.pending_confirm)
        .flatten()
}

/// Draw the Y/N prompt for the pending confirm, if one is up. Prompts that
/// act on something name it, and a git command lists its detail (the commits
/// a push would send) under the question.
fn render_active_confirm(frame: &mut Frame, app: &App) {
    let Some(confirm) = active_confirm(app) else {
        return;
    };
    let title = match confirm {
        ConfirmAction::DiscardChanges => app.pending_discard.as_ref().map(DiscardTarget::prompt),
        ConfirmAction::HistoryRewrite => app
            .pending_history
            .as_ref()
            .map(|(action, _)| action.prompt()),
        ConfirmAction::GitCommand => {
            if let Some(pending) = app.pending_git.as_ref() {
                comment_panel::render_confirm_dialog_with_detail(
                    frame,
                    app,
                    &pending.prompt,
                    &pending.detail,
                );
                return;
            }
            None
        }
        _ => None,
    };
    comment_panel::render_confirm_dialog(frame, app, title.as_deref().unwrap_or(confirm.prompt()));
}
