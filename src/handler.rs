use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

use crate::app::{
    self, App, CommandCompletionState, ExpandDirection, FileTreeItem, FileTreePrompt, FocusedPanel,
    GapCursorHit, InputMode, RemoteOp, TargetTab, VisualSelection,
};
use crate::forge::remote_comments::PrCommentsVisibility;
use crate::forge::submit::SubmitEvent;
use crate::input::{Action, map_global_key};
use crate::model::{ClearScope, LineSide};
use crate::output::{copy_text_to_clipboard, export_to_clipboard, generate_export_content};
use crate::text_edit::{
    delete_char_before, delete_word_before, next_char_boundary, prev_char_boundary,
};

/// Columns scrolled per horizontal mouse wheel tick. Matches the default
/// step for keyboard arrow scrolling so the two input methods feel
/// interchangeable.
const WHEEL_COLS: usize = 4;

/// Shown when a bare `q` is pressed in a mode that used to quit on it.
/// Transitional — drop this a few releases after the `q` removal has landed.
const QUIT_HINT_MESSAGE: &str = "q no longer quits — use :q to quit";

const COMMAND_SPECS: &[CommandSpec] = &[
    CommandSpec::new(&["q", "quit"], CommandKind::Quit),
    CommandSpec::new(&["q!", "quit!"], CommandKind::ForceQuit),
    CommandSpec::new(&["w", "write"], CommandKind::Write),
    CommandSpec::new(&["x", "wq"], CommandKind::WriteQuit),
    CommandSpec::new(&["e", "reload"], CommandKind::Reload),
    CommandSpec::new(&["edit"], CommandKind::Edit),
    CommandSpec::new(&["clip", "export"], CommandKind::Export),
    CommandSpec::new(&["copy-url"], CommandKind::CopyUrl),
    CommandSpec::new(
        &["clear"],
        CommandKind::Clear(ClearScope::CommentsAndReviewed),
    ),
    CommandSpec::new(&["clearc"], CommandKind::Clear(ClearScope::CommentsOnly)),
    CommandSpec::new(&["help", "h"], CommandKind::Help),
    CommandSpec::new(&["messages"], CommandKind::MessageDetails),
    CommandSpec::new(&["version"], CommandKind::Version),
    CommandSpec::new(&["update"], CommandKind::Update),
    CommandSpec::new(&["set wrap"], CommandKind::SetWrap),
    CommandSpec::new(&["set wrap!"], CommandKind::ToggleWrap),
    CommandSpec::new(&["wrap"], CommandKind::ToggleWrap),
    CommandSpec::new(
        &["set relativenumber"],
        CommandKind::SetRelativeLineNumbers(true),
    ),
    CommandSpec::new(
        &["set norelativenumber"],
        CommandKind::SetRelativeLineNumbers(false),
    ),
    CommandSpec::new(
        &["set relativenumber!"],
        CommandKind::ToggleRelativeLineNumbers,
    ),
    CommandSpec::new(&["vim", "set vim!"], CommandKind::ToggleVim),
    CommandSpec::new(&["set vim"], CommandKind::SetVim(true)),
    CommandSpec::new(&["novim", "set novim"], CommandKind::SetVim(false)),
    CommandSpec::new(&["set commits"], CommandKind::SetCommitsVisible(true)),
    CommandSpec::new(&["set nocommits"], CommandKind::SetCommitsVisible(false)),
    CommandSpec::new(&["set commits!"], CommandKind::ToggleCommits),
    CommandSpec::new(&["set log"], CommandKind::SetLogVisible(true)),
    CommandSpec::new(&["set nolog"], CommandKind::SetLogVisible(false)),
    CommandSpec::new(&["log", "set log!"], CommandKind::ToggleLog),
    CommandSpec::new(&["zen"], CommandKind::ToggleZen),
    CommandSpec::new(&["palette", "actions"], CommandKind::Palette),
    CommandSpec::new(
        &["set ignorews"],
        CommandKind::SetIgnoreWhitespace(Some(true)),
    ),
    CommandSpec::new(
        &["set noignorews"],
        CommandKind::SetIgnoreWhitespace(Some(false)),
    ),
    CommandSpec::new(
        &["ignorews", "set ignorews!"],
        CommandKind::SetIgnoreWhitespace(None),
    ),
    CommandSpec::new(&["set mouse"], CommandKind::SetMouse(Some(true))),
    CommandSpec::new(&["set nomouse"], CommandKind::SetMouse(Some(false))),
    CommandSpec::new(&["set mouse!"], CommandKind::SetMouse(None)),
    CommandSpec::new(&["set split"], CommandKind::ResetSplit),
    CommandSpec::new(
        &["branches", "branch", "compare", "select"],
        CommandKind::BranchPicker,
    ),
    CommandSpec::new(&["abort"], CommandKind::AbortOperation),
    CommandSpec::new(&["fetch"], CommandKind::Remote(RemoteOp::Fetch)),
    CommandSpec::new(&["revert"], CommandKind::RevertCommit),
    CommandSpec::new(&["reword"], CommandKind::RewordCommit),
    CommandSpec::new(&["drop"], CommandKind::DropCommit),
    CommandSpec::new(&["fixup"], CommandKind::FixupCommit),
    CommandSpec::new(&["undo"], CommandKind::UndoCommit),
    CommandSpec::new(&["reset"], CommandKind::ResetPrompt),
    CommandSpec::new(&["pull"], CommandKind::Remote(RemoteOp::Pull)),
    CommandSpec::new(
        &["pull!", "pull --rebase"],
        CommandKind::Remote(RemoteOp::PullRebase),
    ),
    CommandSpec::new(&["push"], CommandKind::PushPreview(false)),
    CommandSpec::new(
        &["push!", "push --force-with-lease"],
        CommandKind::PushPreview(true),
    ),
    CommandSpec::new(
        &["push --tags", "push-tags"],
        CommandKind::Remote(RemoteOp::PushTags),
    ),
    CommandSpec::new(&["continue"], CommandKind::ContinueOperation),
    CommandSpec::new(&["skip"], CommandKind::SkipOperation),
    CommandSpec::new(&["console", "gitlog"], CommandKind::Console),
    CommandSpec::new(&["commit"], CommandKind::Commit(false)),
    CommandSpec::new(&["commit!", "amend"], CommandKind::Commit(true)),
    CommandSpec::new(&["unstage"], CommandKind::UnstageFile),
    CommandSpec::new(&["discard"], CommandKind::DiscardFile),
    CommandSpec::new(&["stash"], CommandKind::StashPush),
    CommandSpec::new(&["stash pop"], CommandKind::StashPop),
    CommandSpec::new(&["stashes", "stash list"], CommandKind::StashPicker),
    CommandSpec::new(
        &["changes", "status", "set changes!"],
        CommandKind::ToggleChanges,
    ),
    CommandSpec::new(&["set changes"], CommandKind::SetChangesVisible(true)),
    CommandSpec::new(&["set nochanges"], CommandKind::SetChangesVisible(false)),
    CommandSpec::new(&["find", "grep"], CommandKind::FindResults),
    CommandSpec::new(
        &["blame", "set blame!", "annotate"],
        CommandKind::ToggleBlame,
    ),
    CommandSpec::new(&["set blame"], CommandKind::SetBlame(true)),
    CommandSpec::new(&["set noblame"], CommandKind::SetBlame(false)),
    CommandSpec::new(&["history"], CommandKind::FileHistory),
    CommandSpec::new(&["blame-commit"], CommandKind::BlameCommit),
    CommandSpec::new(&["local"], CommandKind::CompareWithLocal),
    CommandSpec::new(&["clearedits"], CommandKind::ClearEdits),
    CommandSpec::new(&["set reviewed"], CommandKind::SetShowReviewed(true)),
    CommandSpec::new(&["set noreviewed"], CommandKind::SetShowReviewed(false)),
    CommandSpec::new(
        &["reviewed", "set reviewed!"],
        CommandKind::ToggleShowReviewed,
    ),
    CommandSpec::new(&["diff"], CommandKind::Diff),
    CommandSpec::new(&["focus", "f"], CommandKind::Focus),
    CommandSpec::new(&["stage"], CommandKind::Stage),
    CommandSpec::new(&["merge", "conflicts"], CommandKind::Merge),
    CommandSpec::new(
        &["commits", "targets"],
        CommandKind::Targets(TargetTab::Local),
    ),
    CommandSpec::new(&["prs"], CommandKind::Targets(TargetTab::PullRequests)),
    CommandSpec::new(
        &["sessions", "reviews"],
        CommandKind::Targets(TargetTab::Sessions),
    ),
    CommandSpec::new(&["submit"], CommandKind::SubmitPicker),
    CommandSpec::new(
        &["submit comment"],
        CommandKind::Submit(SubmitEvent::Comment),
    ),
    CommandSpec::new(
        &["submit approve"],
        CommandKind::Submit(SubmitEvent::Approve),
    ),
    CommandSpec::new(
        &["submit request-changes"],
        CommandKind::Submit(SubmitEvent::RequestChanges),
    ),
    CommandSpec::new(&["submit draft"], CommandKind::Submit(SubmitEvent::Draft)),
    CommandSpec::new(&["summary"], CommandKind::Summary),
    CommandSpec::new(&["theme"], CommandKind::ThemePicker),
    CommandSpec::new(
        &["comments unresolved"],
        CommandKind::Comments(PrCommentsVisibility::Unresolved),
    ),
    CommandSpec::new(
        &["comments all"],
        CommandKind::Comments(PrCommentsVisibility::All),
    ),
    CommandSpec::new(
        &["comments hide"],
        CommandKind::Comments(PrCommentsVisibility::Hide),
    ),
];

/// CommandSpec is the single registry entry used by both completion and
/// command-mode dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommandSpec {
    /// Accepted command strings, including aliases.
    names: &'static [&'static str],
    /// Behavior shared by every alias in `names`.
    kind: CommandKind,
}

impl CommandSpec {
    const fn new(names: &'static [&'static str], kind: CommandKind) -> Self {
        Self { names, kind }
    }
}

/// CommandKind is the parsed meaning of one command-mode input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandKind {
    Quit,
    ForceQuit,
    Write,
    WriteQuit,
    Reload,
    Edit,
    Export,
    CopyUrl,
    Clear(ClearScope),
    Help,
    MessageDetails,
    Summary,
    Version,
    Update,
    SetWrap,
    ToggleWrap,
    SetRelativeLineNumbers(bool),
    ToggleRelativeLineNumbers,
    ToggleVim,
    SetVim(bool),
    SetCommitsVisible(bool),
    ToggleCommits,
    SetLogVisible(bool),
    ToggleLog,
    ToggleZen,
    Palette,
    PushPreview(bool),
    ContinueOperation,
    SkipOperation,
    Console,
    BlameCommit,
    ResetSplit,
    /// `Some` sets, `None` toggles.
    SetIgnoreWhitespace(Option<bool>),
    /// `:set mouse` / `nomouse` / `mouse!` (`None` toggles): whether tuicr
    /// captures the mouse or leaves it to the terminal.
    SetMouse(Option<bool>),
    BranchPicker,
    AbortOperation,
    Remote(RemoteOp),
    RevertCommit,
    RewordCommit,
    DropCommit,
    FixupCommit,
    UndoCommit,
    ResetPrompt,
    Commit(bool),
    UnstageFile,
    DiscardFile,
    StashPush,
    StashPop,
    StashPicker,
    /// `:changes` / `:status`: show, focus, or hide the changes window.
    ToggleChanges,
    SetChangesVisible(bool),
    FindResults,
    ToggleBlame,
    SetBlame(bool),
    FileHistory,
    /// `:local`: commit range <-> the same commits plus the working tree.
    CompareWithLocal,
    /// `:clearedits`: forget the `$EDITOR` baselines behind the edit tint.
    ClearEdits,
    SetShowReviewed(bool),
    ToggleShowReviewed,
    Diff,
    Focus,
    Stage,
    Targets(TargetTab),
    Merge,
    SubmitPicker,
    Submit(SubmitEvent),
    Comments(PrCommentsVisibility),
    ThemePicker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandAfterDispatch {
    ExitCommandMode,
    KeepMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionDirection {
    Forward,
    Reverse,
}

/// Handle a key that means the same thing in every input mode (see
/// `map_global_key`). Returns `true` when the key was consumed, so the caller
/// skips the per-mode maps.
pub fn handle_global_key(app: &mut App, key: KeyEvent) -> bool {
    match map_global_key(key) {
        Action::Redraw => {
            app.request_full_redraw();
            true
        }
        _ => false,
    }
}

pub fn handle_mouse_event(app: &mut App, event: MouseEvent) {
    let pos = Position::new(event.column, event.row);
    // Splitter drags (file tree border, side-by-side divider) own the
    // gesture from press to release, before any selection logic sees it.
    if matches!(app.input_mode, InputMode::Normal | InputMode::VisualSelect) {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(splitter) = app.splitter_at(pos.x, pos.y) {
                    app.splitter_drag = Some(splitter);
                    return;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(splitter) = app.splitter_drag {
                    app.drag_splitter_to(splitter, pos.x);
                    return;
                }
            }
            MouseEventKind::Up(MouseButton::Left) if app.splitter_drag.take().is_some() => {
                return;
            }
            _ => {}
        }
    }
    match event.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let scroll_up = matches!(event.kind, MouseEventKind::ScrollUp);
            let action = if scroll_up {
                Action::MouseScrollUp(app.mouse_scroll_lines)
            } else {
                Action::MouseScrollDown(app.mouse_scroll_lines)
            };
            let over_file_list = app.file_list_area.is_some_and(|r| r.contains(pos));
            let over_diff = app.diff_area.is_some_and(|r| r.contains(pos));
            let over_commit_list = app.commit_list_inner_area.is_some_and(|r| r.contains(pos));
            let over_log = app.log_pane_inner_area().is_some_and(|r| r.contains(pos));
            let over_changes = app
                .changes_pane_inner_area()
                .is_some_and(|r| r.contains(pos));
            match app.input_mode {
                InputMode::Help | InputMode::MessageDetails => handle_help_action(app, action),
                InputMode::Summary => handle_summary_action(app, action),
                InputMode::FindResults => handle_find_results_action(app, action),
                InputMode::MergeConflict => {
                    if scroll_up {
                        app.merge_scroll_up(app.mouse_scroll_lines);
                    } else {
                        app.merge_scroll_down(app.mouse_scroll_lines);
                    }
                }
                InputMode::Normal if over_log => handle_log_pane_action(app, action),
                InputMode::Normal if over_changes => handle_changes_pane_action(app, action),
                InputMode::ThemePicker => {
                    app.move_theme_picker_selection(if scroll_up { -1 } else { 1 });
                }
                InputMode::CommitSelect | InputMode::Normal if over_commit_list => {
                    wheel_commit_list(app, scroll_up);
                }
                InputMode::Normal if over_file_list => handle_file_list_action(app, action),
                InputMode::Normal
                    if app.comment_navigator_area.is_some_and(|r| r.contains(pos)) =>
                {
                    handle_comment_navigator_action(app, action)
                }
                InputMode::Normal if over_diff => handle_diff_action(app, action),
                InputMode::VisualSelect if over_diff => handle_diff_action(app, action),
                _ => {}
            }
            clear_visual_if_cursor_offscreen(app);
        }
        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {
            // Bypass the Action layer: trackpad swipes dispatch directly to
            // the diff viewport so they don't trigger keyboard focus-slide
            // (FEAT-0012) or abort an active VisualSelect.
            let scroll_left = matches!(event.kind, MouseEventKind::ScrollLeft);
            let over_diff = app.diff_area.is_some_and(|r| r.contains(pos));
            if over_diff && matches!(app.input_mode, InputMode::Normal | InputMode::VisualSelect) {
                if scroll_left {
                    app.scroll_left(WHEEL_COLS);
                } else {
                    app.scroll_right(WHEEL_COLS);
                }
            }
        }
        MouseEventKind::Down(MouseButton::Left)
            if matches!(app.input_mode, InputMode::Normal | InputMode::VisualSelect) =>
        {
            // Set focus based on the outer panel area (includes the
            // border) so clicks on dead space still move focus.
            if app.file_list_area.is_some_and(|r| r.contains(pos)) {
                app.focused_panel = app.left_panel();
            } else if app.diff_area.is_some_and(|r| r.contains(pos)) {
                app.focused_panel = FocusedPanel::Diff;
            }
            // Reset back to Normal so an Up without drag falls through to handle_left_click.
            if app.input_mode == InputMode::VisualSelect {
                app.exit_visual_mode();
            }
            app.mouse_drag_active = false;
            if app.diff_inner_area.is_some_and(|r| r.contains(pos))
                && let Some(point) = app.cell_to_sel_point(pos.x, pos.y)
            {
                app.visual_selection = Some(VisualSelection::collapsed(point));
                if let Some(idx) = app.diff_annotation_at_screen_row(pos.y) {
                    app.move_cursor_to_annotation(idx);
                }
            } else {
                app.visual_selection = None;
                handle_left_click(app, pos);
            }
        }
        MouseEventKind::Down(MouseButton::Left) if app.input_mode == InputMode::CommitSelect => {
            if let Some(idx) = app.commit_list_idx_at_screen_row(pos.y) {
                app.commit_list_cursor = idx;
                handle_commit_select_action(app, Action::ToggleCommitSelect);
            }
        }
        MouseEventKind::Drag(MouseButton::Left)
            if matches!(app.input_mode, InputMode::Normal | InputMode::VisualSelect) =>
        {
            let Some(sel) = app.visual_selection else {
                return;
            };
            let Some(mut head) = app.cell_to_sel_point(pos.x, pos.y) else {
                return;
            };
            // Pin head to the anchor's pane so cross-side drags don't escape.
            head.side = sel.anchor.side;
            if head == sel.head && app.mouse_drag_active {
                return;
            }
            let moved = head != sel.head;
            let promoted_now = moved && !app.mouse_drag_active;
            app.visual_selection = Some(VisualSelection {
                anchor: sel.anchor,
                head,
            });
            if moved {
                app.mouse_drag_active = true;
            }
            if promoted_now && app.input_mode == InputMode::Normal {
                app.input_mode = InputMode::VisualSelect;
            }
            if app.input_mode == InputMode::VisualSelect
                && head.annotation_idx != sel.head.annotation_idx
            {
                app.move_cursor_to_annotation(head.annotation_idx);
            }
        }
        MouseEventKind::Up(MouseButton::Left)
            if matches!(app.input_mode, InputMode::Normal | InputMode::VisualSelect) =>
        {
            if app.visual_selection.is_none() {
                return;
            }
            if !app.mouse_drag_active {
                app.visual_selection = None;
                if app.input_mode == InputMode::VisualSelect {
                    app.exit_visual_mode();
                }
                handle_left_click(app, pos);
            }
            app.mouse_drag_active = false;
        }
        _ => {}
    }
}

/// Helix-style: scrolling the cursor off-viewport drops the selection.
pub fn clear_visual_if_cursor_offscreen(app: &mut App) {
    if app.input_mode == InputMode::VisualSelect && !app.is_cursor_visible() {
        app.exit_visual_mode();
    }
}

/// Wheel scroll inside a commit list (full-screen picker or inline selector).
/// The list is short and selection-oriented, so each tick moves the cursor
/// rather than the viewport, matching how arrow keys behave.
fn wheel_commit_list(app: &mut App, scroll_up: bool) {
    // Match screen direction: the inline pane (Normal mode) reverses its
    // display in ascending order, so wheel-up moves toward newer (lower) data
    // indices there. The full-screen picker (CommitSelect) is always
    // newest-first.
    let scroll_up = if app.input_mode == InputMode::Normal && app.commits_ascending() {
        !scroll_up
    } else {
        scroll_up
    };
    for _ in 0..app.mouse_scroll_lines {
        if scroll_up {
            app.commit_select_up();
        } else {
            app.commit_select_down();
        }
    }
}

fn handle_left_click(app: &mut App, pos: Position) {
    if let Some(area) = app.changes_pane_inner_area()
        && area.contains(pos)
    {
        app.focused_panel = FocusedPanel::Changes;
        if let Some(row) = app.changes_row_at_screen_row(pos.y) {
            app.changes_click_row(row);
        }
        return;
    }

    if app.file_list_inner_area.is_some_and(|r| r.contains(pos))
        && let Some(idx) = app.file_list_idx_at_screen_row(pos.y)
    {
        app.focused_panel = FocusedPanel::FileList;
        app.file_list_state.select(idx);
        if let Some(item) = app.build_visible_items().get(idx).cloned() {
            match item {
                FileTreeItem::Directory { path, .. } => app.toggle_directory(&path),
                FileTreeItem::File { file_idx, .. } => {
                    app.jump_to_file(file_idx);
                    app.focused_panel = FocusedPanel::Diff;
                }
            }
        }
        return;
    }

    if app
        .comment_navigator_inner_area
        .is_some_and(|r| r.contains(pos))
        && let Some(idx) = app.comment_navigator_idx_at_screen_row(pos.y)
    {
        app.focused_panel = FocusedPanel::Comments;
        app.comment_navigator_state.select(idx);
        app.jump_to_selected_comment();
        return;
    }

    if let Some(area) = app.log_pane_inner_area()
        && area.contains(pos)
    {
        app.focused_panel = FocusedPanel::CommitSelector;
        app.log_pane_click_row(usize::from(pos.y.saturating_sub(area.y)));
        return;
    }

    if app.has_inline_commit_selector()
        && app.commit_list_inner_area.is_some_and(|r| r.contains(pos))
        && let Some(idx) = app.commit_list_idx_at_screen_row(pos.y)
    {
        app.focused_panel = FocusedPanel::CommitSelector;
        app.commit_list_cursor = idx;
        handle_commit_selector_action(app, Action::SelectFile);
        return;
    }

    if app.diff_inner_area.is_some_and(|r| r.contains(pos))
        && let Some(idx) = app.diff_annotation_at_screen_row(pos.y)
    {
        app.focused_panel = FocusedPanel::Diff;
        app.move_cursor_to_annotation(idx);
        handle_diff_action(app, Action::SelectFile);
    }
}

fn handle_copy_pr_url(app: &mut App) {
    let app::DiffSource::PullRequest(pr) = &app.diff_source else {
        app.set_warning(":copy-url only applies in PR mode");
        return;
    };
    let url = pr.url.clone();

    match copy_text_to_clipboard(&url) {
        Ok(true) => app.set_message("PR URL copied to clipboard (via terminal)"),
        Ok(false) => app.set_message("PR URL copied to clipboard"),
        Err(e) => app.set_warning(format!("Failed to copy PR URL: {e}")),
    }
}

/// Export review: either to clipboard or set pending stdout output based on app.output_to_stdout.
/// When output_to_stdout is true, stores the content and sets should_quit.
fn handle_export(app: &mut App) {
    let slug = app.session_slug();
    if app.output_to_stdout {
        match generate_export_content(
            &app.session,
            &app.diff_source,
            &app.comment_types,
            &app.export,
            &app.forge_review_threads,
            slug.as_deref(),
        ) {
            Ok(content) => {
                app.pending_stdout_output = Some(content);
                app.should_quit = true;
            }
            Err(e) => app.set_warning(format!("{e}")),
        }
    } else {
        match export_to_clipboard(
            &app.session,
            &app.diff_source,
            &app.comment_types,
            &app.export,
            &app.forge_review_threads,
            slug.as_deref(),
        ) {
            Ok(msg) => app.set_message(msg),
            Err(e) => app.set_warning(format!("{e}")),
        }
    }
}

/// Copy just the comment under the cursor (`Y`). Unlike `y`, the rest of the
/// review stays out of the clipboard, so a single comment can go straight into
/// a chat message or an agent prompt.
fn handle_copy_comment_at_cursor(app: &mut App) {
    let Some(content) = app
        .comment_content_at_cursor()
        .or_else(|| app.remote_comment_content_at_cursor())
    else {
        if app.cursor_on_remote_thread() {
            let forge = app.forge_display_name();
            app.set_message(format!("No copyable comment in this {forge} thread"));
        } else {
            app.set_message("No comment at cursor");
        }
        return;
    };
    match copy_text_to_clipboard(&content) {
        Ok(true) => app.set_message("Comment copied to clipboard (via terminal)"),
        Ok(false) => app.set_message("Comment copied to clipboard"),
        Err(e) => app.set_warning(format!("{e}")),
    }
}

/// Export and quit (used by ZZ keybinding).
/// When --stdout is set, stores export content and quits.
/// Otherwise, exports to clipboard and quits.
pub fn handle_export_and_quit(app: &mut App) {
    handle_export(app);
    app.should_quit = true;
}

fn comment_line_start(buffer: &str, cursor: usize) -> usize {
    let cursor = cursor.min(buffer.len());
    match buffer[..cursor].rfind('\n') {
        Some(pos) => pos + 1,
        None => 0,
    }
}

fn comment_line_end(buffer: &str, cursor: usize) -> usize {
    let cursor = cursor.min(buffer.len());
    match buffer[cursor..].find('\n') {
        Some(pos) => cursor + pos,
        None => buffer.len(),
    }
}

fn comment_word_left(buffer: &str, cursor: usize) -> usize {
    let cursor = cursor.min(buffer.len());
    if cursor == 0 {
        return 0;
    }
    let before = &buffer[..cursor];
    let mut idx = 0;
    let mut found_word = false;
    for (pos, ch) in before.char_indices().rev() {
        if !ch.is_whitespace() {
            idx = pos;
            found_word = true;
            break;
        }
    }

    if !found_word {
        return 0;
    }

    for (pos, ch) in before[..idx].char_indices().rev() {
        if ch.is_whitespace() {
            return pos + ch.len_utf8();
        }
        idx = pos;
    }

    idx
}

fn comment_word_right(buffer: &str, cursor: usize) -> usize {
    let cursor = cursor.min(buffer.len());
    if cursor >= buffer.len() {
        return buffer.len();
    }

    let mut chars = buffer[cursor..].char_indices();
    if let Some((_, ch)) = chars.next()
        && ch.is_whitespace()
    {
        for (pos, ch) in buffer[cursor..].char_indices() {
            if !ch.is_whitespace() {
                return cursor + pos;
            }
        }
        return buffer.len();
    }

    let mut word_end = buffer.len();
    for (pos, ch) in buffer[cursor..].char_indices() {
        if ch.is_whitespace() {
            word_end = cursor + pos;
            break;
        }
    }

    if word_end >= buffer.len() {
        return buffer.len();
    }

    for (pos, ch) in buffer[word_end..].char_indices() {
        if !ch.is_whitespace() {
            return word_end + pos;
        }
    }

    buffer.len()
}

/// Append a pasted payload into a single-line buffer, dropping embedded
/// newlines so a multi-line paste doesn't smear across the prompt.
/// Whether a bracketed paste has somewhere to go: a text input is open and
/// its handler takes `Action::Paste`. Everywhere else a paste is dropped,
/// so its characters never run as keys.
pub fn paste_target_open(app: &App) -> bool {
    match app.input_mode {
        InputMode::Comment
        | InputMode::Command
        | InputMode::Search
        | InputMode::Palette
        | InputMode::BranchPicker => true,
        InputMode::CommitSelect => {
            app.pr_filter_editing()
                || (app.target_tab == TargetTab::Local && app.commit_filter_editing())
        }
        InputMode::Normal => app.commit_filter_editing() || app.file_tree_prompt_editing(),
        InputMode::ThemePicker => app.theme_picker_filtering(),
        InputMode::RebaseEditor => app
            .rebase_editor
            .as_ref()
            .is_some_and(|e| e.editing.is_some()),
        _ => false,
    }
}

fn push_single_line(buffer: &mut String, text: &str) {
    for ch in text.chars() {
        if matches!(ch, '\n' | '\r') {
            continue;
        }
        buffer.push(ch);
    }
}

/// Handle actions in Help mode (scrolling only)
pub fn handle_help_action(app: &mut App, action: Action) {
    match action {
        Action::CursorDown(n) => app.help_scroll_down(n),
        Action::CursorUp(n) => app.help_scroll_up(n),
        Action::ScrollLeft(n) => app.help_scroll_left(n),
        Action::ScrollRight(n) => app.help_scroll_right(n),
        Action::HalfPageDown => app.help_scroll_down(app.help_state.viewport_height / 2),
        Action::HalfPageUp => app.help_scroll_up(app.help_state.viewport_height / 2),
        Action::PageDown => app.help_scroll_down(app.help_state.viewport_height),
        Action::PageUp => app.help_scroll_up(app.help_state.viewport_height),
        Action::GoToTop => app.help_scroll_to_top(),
        Action::GoToBottom => app.help_scroll_to_bottom(),
        Action::MouseScrollDown(n) => app.help_scroll_down(n),
        Action::MouseScrollUp(n) => app.help_scroll_up(n),
        Action::EnterSearchMode => app.enter_search_mode(),
        Action::SearchNext => {
            app.search_next_in_help();
        }
        Action::SearchPrev => {
            app.search_prev_in_help();
        }
        Action::ToggleHelp => app.toggle_help(),
        Action::Quit => app.should_quit = true,
        _ => {}
    }
}

/// Handle selection, scrolling, activation, and dismissal in the pending-comments summary view.
pub fn handle_summary_action(app: &mut App, action: Action) {
    match action {
        Action::CursorDown(n) => app.summary_select_down(n),
        Action::CursorUp(n) => app.summary_select_up(n),
        Action::HalfPageDown => {
            app.summary_scroll_down(app.summary_state.viewport_height / 2);
        }
        Action::HalfPageUp => {
            app.summary_scroll_up(app.summary_state.viewport_height / 2);
        }
        Action::PageDown => app.summary_scroll_down(app.summary_state.viewport_height),
        Action::PageUp => app.summary_scroll_up(app.summary_state.viewport_height),
        Action::GoToTop => app.summary_select_first(),
        Action::GoToBottom => app.summary_select_last(),
        Action::MouseScrollDown(n) => app.summary_scroll_down(n),
        Action::MouseScrollUp(n) => app.summary_scroll_up(n),
        Action::SubmitInput => {
            let selected_target = app
                .summary_state
                .targets
                .get(app.summary_state.selected_comment)
                .cloned()
                .flatten();
            if let Some(target) = selected_target {
                app.jump_to_summary_comment(target);
            } else if !app.summary_state.targets.is_empty() {
                app.set_warning("That comment is hidden by the current diff or filters");
            }
        }
        Action::ExitMode => app.exit_summary_mode(),
        Action::Quit => app.should_quit = true,
        _ => {}
    }
}

/// Handle actions in Command mode (text input for :commands)
pub fn handle_command_action(app: &mut App, action: Action) {
    match action {
        Action::InsertChar(c) => {
            app.command_completion = None;
            app.command_buffer.push(c);
        }
        Action::Paste(text) => {
            app.command_completion = None;
            push_single_line(&mut app.command_buffer, &text);
        }
        Action::DeleteChar => {
            app.command_completion = None;
            app.command_buffer.pop();
        }
        Action::DeleteWord => {
            app.command_completion = None;
            let cursor = app.command_buffer.len();
            delete_word_before(&mut app.command_buffer, cursor);
        }
        Action::ClearLine => {
            app.command_completion = None;
            app.command_buffer.clear();
        }
        Action::CompleteCommand => complete_command(app, CompletionDirection::Forward),
        Action::CompleteCommandReverse => complete_command(app, CompletionDirection::Reverse),
        Action::ExitMode => app.exit_command_mode(),
        Action::SubmitInput => {
            app.command_completion = None;
            let cmd = app.command_buffer.trim().to_string();
            let after_dispatch = if let Some(spec) = command_spec_for(&cmd) {
                dispatch_command(app, spec.kind)
            } else if let Some((lineno, side)) = parse_lineno_command(&cmd) {
                app.go_to_source_line(lineno, side);
                CommandAfterDispatch::ExitCommandMode
            } else if let Some(rows) = parse_log_height_command(&cmd) {
                app.set_log_pane_height(rows);
                CommandAfterDispatch::ExitCommandMode
            } else if let Some(pct) = parse_set_number(&cmd, "split") {
                app.set_sbs_split(pct);
                CommandAfterDispatch::ExitCommandMode
            } else if let Some(pct) = parse_set_number(&cmd, "treewidth") {
                app.set_file_list_pct(pct);
                CommandAfterDispatch::ExitCommandMode
            } else if let Some(result) = parse_git_op_command(&cmd) {
                app.exit_command_mode();
                match result {
                    GitOpCommand::Merge(branch) => app.merge_into_current(&branch),
                    GitOpCommand::Rebase(onto) => app.rebase_current_onto(&onto),
                    GitOpCommand::RebaseInteractive(Some(rev)) => app.open_rebase_editor(&rev),
                    GitOpCommand::RebaseInteractive(None) => app.open_rebase_editor_at_cursor(),
                    GitOpCommand::DeleteBranch(name) => app.delete_branch(&name),
                    GitOpCommand::RenameBranch(old, new) => app.rename_branch(old.as_deref(), &new),
                    GitOpCommand::Upstream(up) => app.set_upstream(&up),
                    GitOpCommand::DeleteTag(name) => app.delete_tag(&name),
                }
                CommandAfterDispatch::KeepMode
            } else if let Some((target, smart)) = parse_checkout_command(&cmd) {
                app.checkout_target(target, smart);
                CommandAfterDispatch::ExitCommandMode
            } else if let Some(name) = cmd.strip_prefix("branch ") {
                let name = name.to_string();
                app.create_branch(&name);
                CommandAfterDispatch::ExitCommandMode
            } else if let Some(message) = cmd.strip_prefix("commit! ") {
                let message = message.to_string();
                app.commit_with_message(&message, true);
                CommandAfterDispatch::ExitCommandMode
            } else if let Some(message) = cmd.strip_prefix("commit ") {
                let message = message.to_string();
                app.commit_with_message(&message, false);
                CommandAfterDispatch::ExitCommandMode
            } else if let Some(message) = cmd.strip_prefix("stash ") {
                let message = message.to_string();
                app.stash_push_current(Some(&message));
                CommandAfterDispatch::ExitCommandMode
            } else if let Some(args) = cmd.strip_prefix("reset ") {
                let args = args.to_string();
                app.reset_command(&args);
                history_after_dispatch(app)
            } else if let Some(args) = cmd.strip_prefix("tag ") {
                let args = args.to_string();
                app.create_tag(&args);
                CommandAfterDispatch::ExitCommandMode
            } else if let Some(args) = cmd
                .strip_prefix("compare ")
                .or_else(|| cmd.strip_prefix("select "))
            {
                let args = args.to_string();
                app.exit_command_mode();
                app.compare_command(&args);
                CommandAfterDispatch::KeepMode
            } else if let Some(query) = cmd
                .strip_prefix("find ")
                .or_else(|| cmd.strip_prefix("grep "))
            {
                let query = query.to_string();
                app.exit_command_mode();
                app.find_in_files(&query);
                CommandAfterDispatch::KeepMode
            } else if let Some(name) = cmd.strip_prefix("theme ") {
                let name = name.trim();
                if name.is_empty() {
                    app.set_message(format!("Unknown command: {cmd}"));
                } else {
                    app.apply_theme(name);
                }
                CommandAfterDispatch::ExitCommandMode
            } else {
                app.set_message(format!("Unknown command: {cmd}"));
                CommandAfterDispatch::ExitCommandMode
            };
            if matches!(after_dispatch, CommandAfterDispatch::ExitCommandMode) {
                app.exit_command_mode();
            }
        }
        Action::Quit => app.should_quit = true,
        _ => {}
    }
}

fn command_spec_for(cmd: &str) -> Option<&'static CommandSpec> {
    COMMAND_SPECS.iter().find(|spec| spec.names.contains(&cmd))
}

/// CommandCompleter computes command-buffer replacements without mutating App.
struct CommandCompleter<'a> {
    /// Registry whose command names are exposed as completion candidates.
    specs: &'a [CommandSpec],
}

impl<'a> CommandCompleter<'a> {
    /// Build a completer over the command registry it should expose.
    pub fn new(specs: &'a [CommandSpec]) -> Self {
        Self { specs }
    }

    /// Return the next command-completion state for the current prompt state.
    ///
    /// `buffer` is the command text currently shown to the user. `active` is
    /// the previous completion cycle, if repeated Tab presses are cycling an
    /// existing match set. The result describes the next buffer contents and
    /// active completion cycle without mutating App.
    pub fn complete(
        &self,
        buffer: &str,
        active: Option<&CommandCompletionState>,
        direction: CompletionDirection,
    ) -> CompletionResult {
        if let Some(result) = self.cycle_existing(buffer, active, direction) {
            return result;
        }

        let prefix = buffer.trim_start().to_string();
        let matches: Vec<String> = self
            .command_names()
            .filter(|command| command.starts_with(&prefix))
            .map(str::to_string)
            .collect();

        match matches.len() {
            0 => CompletionResult::NoMatch,
            1 => CompletionResult::replace(matches[0].clone(), None),
            _ => {
                let common_prefix = Self::common_prefix(&matches);
                if common_prefix.len() > prefix.len() {
                    CompletionResult::replace(common_prefix, None)
                } else {
                    self.start_cycle(buffer, prefix, matches, direction)
                }
            }
        }
    }

    fn command_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.specs
            .iter()
            .flat_map(|spec| spec.names.iter().copied())
    }

    fn cycle_existing(
        &self,
        buffer: &str,
        active: Option<&CommandCompletionState>,
        direction: CompletionDirection,
    ) -> Option<CompletionResult> {
        let completion = active?;
        let current = completion.matches.get(completion.selected)?;
        if buffer != *current {
            return None;
        }

        let selected = Self::next_index(completion.selected, completion.matches.len(), direction);
        Some(CompletionResult::replace(
            completion.matches[selected].clone(),
            Some(CommandCompletionState {
                prefix: completion.prefix.clone(),
                matches: completion.matches.clone(),
                selected,
            }),
        ))
    }

    fn start_cycle(
        &self,
        buffer: &str,
        prefix: String,
        matches: Vec<String>,
        direction: CompletionDirection,
    ) -> CompletionResult {
        let selected = matches
            .iter()
            .position(|command| command == buffer)
            .map(|idx| Self::next_index(idx, matches.len(), direction))
            .unwrap_or_else(|| match direction {
                CompletionDirection::Forward => 0,
                CompletionDirection::Reverse => matches.len() - 1,
            });

        CompletionResult::replace(
            matches[selected].clone(),
            Some(CommandCompletionState {
                prefix,
                matches,
                selected,
            }),
        )
    }

    fn next_index(selected: usize, match_count: usize, direction: CompletionDirection) -> usize {
        match direction {
            CompletionDirection::Forward => (selected + 1) % match_count,
            CompletionDirection::Reverse => (selected + match_count - 1) % match_count,
        }
    }

    fn common_prefix(matches: &[String]) -> String {
        let mut prefix = matches
            .first()
            .map(|command| (*command).to_string())
            .unwrap_or_default();
        for command in &matches[1..] {
            while !command.starts_with(&prefix) {
                prefix.pop();
            }
        }
        prefix
    }
}

/// CompletionResult tells the handler how to update command-mode state.
enum CompletionResult {
    /// Leave the buffer unchanged and surface a no-match message.
    NoMatch,
    /// Replace the buffer and optionally keep a cycling state active.
    Replace {
        /// New command-buffer contents.
        buffer: String,
        /// Completion state used by the next Tab press, if cycling started.
        completion: Option<CommandCompletionState>,
    },
}

impl CompletionResult {
    fn replace(
        buffer: impl Into<String>,
        completion: Option<CommandCompletionState>,
    ) -> CompletionResult {
        CompletionResult::Replace {
            buffer: buffer.into(),
            completion,
        }
    }
}

fn dispatch_command(app: &mut App, kind: CommandKind) -> CommandAfterDispatch {
    match kind {
        CommandKind::Quit => {
            if app.dirty && app.session.has_comments() {
                app.set_error("No write since last change (add ! to override)");
            } else if app.dirty {
                // Dirty from reviewed-file markers only: discard the state and
                // quit instead of requiring `:q!`.
                app.discard_session_and_quit();
            } else {
                app.should_quit = true;
            }
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::ForceQuit => {
            app.should_quit = true;
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::Write => {
            match app.save_current_session_merging_external() {
                Ok(path) => {
                    app.set_message(format!("Saved to {}", path.display()));
                }
                Err(e) => app.set_error(format!("Save failed: {e}")),
            }
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::WriteQuit => {
            match app.save_current_session_merging_external() {
                Ok(_) => {
                    if app.session.has_comments() {
                        if app.output_to_stdout {
                            // Skip confirmation dialog, export directly.
                            handle_export(app);
                        } else {
                            app.exit_command_mode();
                            app.enter_confirm_mode(app::ConfirmAction::CopyAndQuit);
                            return CommandAfterDispatch::KeepMode;
                        }
                    } else {
                        app.should_quit = true;
                    }
                }
                Err(e) => app.set_error(format!("Save failed: {e}")),
            }
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::Reload => {
            reload_review(app);
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::Edit => {
            app.queue_editor_for_focused_item();
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::Export => {
            handle_export(app);
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::CopyUrl => {
            handle_copy_pr_url(app);
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::Clear(scope) => {
            app.clear_comments(scope);
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::Help => {
            // Leave command mode by hand: the dispatch loop's ExitCommandMode
            // path resets `input_mode` to Normal, which would clobber Help.
            app.exit_command_mode();
            app.toggle_help();
            CommandAfterDispatch::KeepMode
        }
        CommandKind::MessageDetails => {
            app.exit_command_mode();
            app.open_message_details();
            CommandAfterDispatch::KeepMode
        }
        CommandKind::Summary => {
            // See Help above: leave command mode before opening the view so
            // the common post-dispatch cleanup cannot clobber Summary.
            app.exit_command_mode();
            app.enter_summary_mode();
            CommandAfterDispatch::KeepMode
        }
        CommandKind::ThemePicker => {
            app.exit_command_mode();
            app.enter_theme_picker_mode();
            CommandAfterDispatch::KeepMode
        }
        CommandKind::Version => {
            app.set_message(format!("tuicr v{}", env!("CARGO_PKG_VERSION")));
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::Update => {
            check_for_updates(app);
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::SetWrap => {
            app.set_diff_wrap(true);
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::ToggleWrap => {
            app.toggle_diff_wrap();
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::SetRelativeLineNumbers(enabled) => {
            app.relative_line_numbers = enabled;
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::ToggleRelativeLineNumbers => {
            app.relative_line_numbers = !app.relative_line_numbers;
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::ToggleVim => {
            app.toggle_comment_vim();
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::SetVim(enabled) => {
            app.set_comment_vim(enabled);
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::SetCommitsVisible(visible) => {
            set_commit_selector_visible(app, visible);
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::ToggleCommits => {
            app.toggle_commit_selector();
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::SetLogVisible(visible) => {
            app.set_log_pane_visible(visible);
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::ToggleLog => {
            app.toggle_log_pane();
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::ToggleZen => {
            app.toggle_zen();
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::SetMouse(enabled) => {
            app.mouse_enabled = enabled.unwrap_or(!app.mouse_enabled);
            app.set_message(if app.mouse_enabled {
                "Mouse: tuicr (click, wheel, drag lines)"
            } else {
                "Mouse: terminal (drag selects text)"
            });
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::SetIgnoreWhitespace(value) => {
            let ignore = value.unwrap_or(!app.ignores_whitespace());
            app.set_ignore_whitespace(ignore);
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::PushPreview(force) => {
            app.exit_command_mode();
            app.push_preview(force);
            CommandAfterDispatch::KeepMode
        }
        CommandKind::ContinueOperation => {
            app.exit_command_mode();
            app.continue_operation();
            CommandAfterDispatch::KeepMode
        }
        CommandKind::SkipOperation => {
            app.exit_command_mode();
            app.skip_operation();
            CommandAfterDispatch::KeepMode
        }
        CommandKind::BlameCommit => {
            app.exit_command_mode();
            app.show_blame_commit();
            CommandAfterDispatch::KeepMode
        }
        CommandKind::Console => {
            app.exit_command_mode();
            app.open_console();
            CommandAfterDispatch::KeepMode
        }
        CommandKind::Palette => {
            app.exit_command_mode();
            app.open_palette();
            CommandAfterDispatch::KeepMode
        }
        CommandKind::ResetSplit => {
            app.set_sbs_split(app::DEFAULT_SBS_SPLIT_PCT);
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::BranchPicker => {
            app.open_branch_picker();
            if app.input_mode == InputMode::BranchPicker {
                CommandAfterDispatch::KeepMode
            } else {
                CommandAfterDispatch::ExitCommandMode
            }
        }
        CommandKind::Remote(op) => {
            app.start_remote_op(op);
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::RevertCommit => {
            app.revert_cursor_commit();
            history_after_dispatch(app)
        }
        CommandKind::RewordCommit => {
            app.start_reword_composer();
            if app.input_mode == InputMode::Comment {
                CommandAfterDispatch::KeepMode
            } else {
                CommandAfterDispatch::ExitCommandMode
            }
        }
        CommandKind::DropCommit => {
            app.request_drop_commit();
            history_after_dispatch(app)
        }
        CommandKind::FixupCommit => {
            app.fixup_cursor_commit();
            history_after_dispatch(app)
        }
        CommandKind::UndoCommit => {
            app.undo_last_commit();
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::ResetPrompt => {
            app.reset_command("");
            history_after_dispatch(app)
        }
        CommandKind::AbortOperation => {
            app.abort_operation();
            if app.input_mode == InputMode::MergeConflict {
                CommandAfterDispatch::KeepMode
            } else {
                CommandAfterDispatch::ExitCommandMode
            }
        }
        CommandKind::Commit(amend) => {
            app.start_commit_composer(amend);
            if app.input_mode == InputMode::Comment {
                CommandAfterDispatch::KeepMode
            } else {
                CommandAfterDispatch::ExitCommandMode
            }
        }
        CommandKind::UnstageFile => {
            app.unstage_file_at_cursor();
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::DiscardFile => {
            app.request_discard_file();
            if app.input_mode == InputMode::Confirm {
                CommandAfterDispatch::KeepMode
            } else {
                CommandAfterDispatch::ExitCommandMode
            }
        }
        CommandKind::StashPush => {
            app.stash_push_current(None);
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::StashPop => {
            app.stash_pop_newest();
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::StashPicker => {
            app.open_stash_picker();
            if app.input_mode == InputMode::StashPicker {
                CommandAfterDispatch::KeepMode
            } else {
                CommandAfterDispatch::ExitCommandMode
            }
        }
        CommandKind::ToggleChanges => {
            app.exit_command_mode();
            app.toggle_changes_pane();
            CommandAfterDispatch::KeepMode
        }
        CommandKind::SetChangesVisible(visible) => {
            app.exit_command_mode();
            app.set_changes_pane_visible(visible);
            CommandAfterDispatch::KeepMode
        }
        CommandKind::ToggleBlame => {
            app.toggle_blame();
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::SetBlame(enabled) => {
            app.set_blame_enabled(enabled);
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::FileHistory => {
            app.exit_command_mode();
            app.show_file_history();
            CommandAfterDispatch::KeepMode
        }
        CommandKind::CompareWithLocal => {
            app.toggle_compare_with_local();
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::ClearEdits => {
            app.clear_local_edits();
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::FindResults => {
            app.reopen_find_results();
            if matches!(app.input_mode, InputMode::FindResults | InputMode::Command) {
                CommandAfterDispatch::KeepMode
            } else {
                CommandAfterDispatch::ExitCommandMode
            }
        }
        CommandKind::SetShowReviewed(show) => {
            app.set_show_reviewed(show);
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::ToggleShowReviewed => {
            app.toggle_show_reviewed();
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::Diff => {
            app.toggle_diff_view_mode();
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::Focus => {
            app.toggle_single_file_view();
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::Stage => {
            app.stage_reviewed_files();
            CommandAfterDispatch::ExitCommandMode
        }
        CommandKind::Merge => {
            if let Err(e) = app.open_merge_view(InputMode::Normal) {
                app.set_error(format!("Failed to open merge view: {e}"));
            }
            if app.input_mode == InputMode::MergeConflict {
                CommandAfterDispatch::KeepMode
            } else {
                CommandAfterDispatch::ExitCommandMode
            }
        }
        CommandKind::Targets(tab) => {
            let result = app.enter_target_selector(tab);
            match (tab, result) {
                (_, Ok(())) => CommandAfterDispatch::KeepMode,
                (TargetTab::Local, Err(e)) => {
                    app.set_error(format!("Failed to load commits: {e}"));
                    CommandAfterDispatch::ExitCommandMode
                }
                (TargetTab::PullRequests, Err(e)) => {
                    app.set_error(format!("Failed to open PR selector: {e}"));
                    CommandAfterDispatch::ExitCommandMode
                }
                (TargetTab::Sessions, Err(e)) => {
                    app.set_error(format!("Failed to open session selector: {e}"));
                    CommandAfterDispatch::ExitCommandMode
                }
            }
        }
        CommandKind::SubmitPicker => {
            app.exit_command_mode();
            app.start_submit_action_picker();
            CommandAfterDispatch::KeepMode
        }
        CommandKind::Submit(event) => {
            app.exit_command_mode();
            app.start_submit(event);
            CommandAfterDispatch::KeepMode
        }
        CommandKind::Comments(visibility) => {
            set_remote_comments_visibility(app, visibility);
            CommandAfterDispatch::ExitCommandMode
        }
    }
}

fn reload_review(app: &mut App) {
    let comment_reload = app.reload_persisted_session_if_changed(true);
    if matches!(app.diff_source, app::DiffSource::PullRequest(_)) {
        if let Err(e) = comment_reload {
            app.set_warning(format!("Comment reload failed: {e}"));
        }
        // Async: shows a spinner in the status bar; result is applied in
        // `poll_pr_reload_events` and the cursor is restored to the captured
        // anchor.
        if let Err(e) = app.spawn_pr_reload() {
            app.set_error(format!("Reload failed: {e}"));
        }
    } else {
        match app.reload_diff_files() {
            Ok((count, invalidated)) => {
                let comment_suffix = match comment_reload {
                    Ok(added) if added > 0 => {
                        format!(", loaded {added} external comments")
                    }
                    Ok(_) => String::new(),
                    Err(e) => format!(", comment reload failed: {e}"),
                };
                if invalidated > 0 {
                    app.set_message(format!(
                        "Reloaded {count} files, {invalidated} changed since last review{comment_suffix}"
                    ));
                } else {
                    app.set_message(format!("Reloaded {count} files{comment_suffix}"));
                }
            }
            Err(e) => app.set_error(format!("Reload failed: {e}")),
        }
    }
}

fn check_for_updates(app: &mut App) {
    match crate::update::check_for_updates() {
        crate::update::UpdateCheckResult::UpdateAvailable(info) => {
            app.set_message(format!(
                "Update available: v{} -> v{}",
                info.current_version, info.latest_version
            ));
        }
        crate::update::UpdateCheckResult::UpToDate(info) => {
            app.set_message(format!("tuicr v{} is up to date", info.current_version));
        }
        crate::update::UpdateCheckResult::AheadOfRelease(info) => {
            app.set_message(format!(
                "You're from the future! v{} > v{}",
                info.current_version, info.latest_version
            ));
        }
        crate::update::UpdateCheckResult::Failed(err) => {
            app.set_warning(format!("Update check failed: {err}"));
        }
    }
}

fn set_commit_selector_visible(app: &mut App, visible: bool) {
    app.show_commit_selector = visible;
    if !visible && app.focused_panel == FocusedPanel::CommitSelector {
        app.focused_panel = FocusedPanel::Diff;
    }
    let status = if visible { "visible" } else { "hidden" };
    app.set_message(format!("Commit selector: {status}"));
}

fn set_remote_comments_visibility(app: &mut App, visibility: PrCommentsVisibility) {
    if !matches!(app.diff_source, app::DiffSource::PullRequest(_)) {
        app.set_warning(":comments only applies in PR mode");
        return;
    }

    let changed = app.set_remote_comments_visibility(visibility);
    let label = visibility.label();
    if changed {
        app.set_message(format!("Remote comments: {label}"));
    } else {
        app.set_message(format!("Remote comments: already {label}"));
    }
}

fn complete_command(app: &mut App, direction: CompletionDirection) {
    let completer = CommandCompleter::new(COMMAND_SPECS);
    let (result, what) = match complete_branch_argument(app, &completer, direction) {
        Some(result) => (result, "branch"),
        None => (
            completer.complete(
                &app.command_buffer,
                app.command_completion.as_ref(),
                direction,
            ),
            "command",
        ),
    };
    match result {
        CompletionResult::NoMatch => {
            app.command_completion = None;
            app.set_message(format!("No {what} matches"));
        }
        CompletionResult::Replace { buffer, completion } => {
            app.command_buffer = buffer;
            app.command_completion = completion;
        }
    }
}

/// Commands whose arguments are branch names, so Tab / Down complete them.
const BRANCH_ARGUMENT_COMMANDS: &[&str] = &["compare", "select", "checkout", "checkout!"];

/// `:compare ma`, `:compare master ori`, `:checkout ma`: complete the word
/// being typed against branch names. Unlike command completion there is no
/// common-prefix stop: the first press lands on the best match and further
/// presses cycle, so `:compare ma`, Down, Down, Enter works.
fn complete_branch_argument(
    app: &mut App,
    completer: &CommandCompleter<'_>,
    direction: CompletionDirection,
) -> Option<CompletionResult> {
    let buffer = app.command_buffer.clone();
    let (head, word) = buffer.rsplit_once(' ')?;
    let command = head.split_whitespace().next()?;
    if !BRANCH_ARGUMENT_COMMANDS.contains(&command) {
        return None;
    }
    if let Some(result) =
        completer.cycle_existing(&buffer, app.command_completion.as_ref(), direction)
    {
        return Some(result);
    }
    let branches = app.vcs.list_branches().ok()?;
    let names: Vec<String> = branches.into_iter().map(|b| b.name).collect();
    let head = format!("{head} ");
    let matches: Vec<String> = rank_branch_matches(&names, word)
        .into_iter()
        .map(|name| format!("{head}{name}"))
        .collect();
    if matches.is_empty() {
        return Some(CompletionResult::NoMatch);
    }
    Some(completer.start_cycle(&buffer, buffer.clone(), matches, direction))
}

/// Branch names matching `word` case-insensitively: prefix matches first,
/// then names containing it, each group in the list's own order (local
/// branches before remote-tracking ones).
fn rank_branch_matches(names: &[String], word: &str) -> Vec<String> {
    let needle = word.to_lowercase();
    let mut prefix_hits = Vec::new();
    let mut inner_hits = Vec::new();
    for name in names {
        let lower = name.to_lowercase();
        if lower.starts_with(&needle) {
            prefix_hits.push(name.clone());
        } else if lower.contains(&needle) {
            inner_hits.push(name.clone());
        }
    }
    prefix_hits.extend(inner_hits);
    prefix_hits
}

/// Parse `:<n>` (new-side) or `:o<n>` (old-side) jump targets. The leading `:`
/// has already been stripped by the time we get here.
/// History actions may have opened a prompt or the merge view; leave the
/// command line only when nothing modal took over.
fn history_after_dispatch(app: &App) -> CommandAfterDispatch {
    if matches!(
        app.input_mode,
        InputMode::Confirm | InputMode::MergeConflict | InputMode::Comment
    ) {
        CommandAfterDispatch::KeepMode
    } else {
        CommandAfterDispatch::ExitCommandMode
    }
}

/// `:checkout <target>` / `:checkout! <target>` — the target and whether to
/// stash a dirty tree around the switch.
fn parse_checkout_command(cmd: &str) -> Option<(&str, bool)> {
    if let Some(rest) = cmd.strip_prefix("checkout! ") {
        let target = rest.trim();
        return (!target.is_empty()).then_some((target, true));
    }
    let target = cmd.strip_prefix("checkout ")?.trim();
    (!target.is_empty()).then_some((target, false))
}

/// `:set logheight N` — the log pane height in rows, borders included.
fn parse_log_height_command(cmd: &str) -> Option<u16> {
    let rest = cmd.strip_prefix("set logheight")?.trim();
    let rest = rest.strip_prefix('=').map(str::trim).unwrap_or(rest);
    rest.parse().ok()
}

/// Branch, tag, merge, and rebase commands that take arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GitOpCommand {
    /// `:merge <branch>` (bare `:merge` opens the conflict view).
    Merge(String),
    /// `:rebase <branch>`.
    Rebase(String),
    /// `:rebase -i [rev]` (the log cursor without a rev).
    RebaseInteractive(Option<String>),
    /// `:branch -d <name>` / `:branch -D <name>`.
    DeleteBranch(String),
    /// `:branch -m [old] <new>`.
    RenameBranch(Option<String>, String),
    /// `:upstream <remote/branch>`.
    Upstream(String),
    /// `:tag -d <name>`.
    DeleteTag(String),
}

fn parse_git_op_command(cmd: &str) -> Option<GitOpCommand> {
    let mut words = cmd.split_whitespace();
    let verb = words.next()?;
    let rest: Vec<&str> = words.collect();
    let one = |v: &[&str]| (v.len() == 1).then(|| v[0].to_string());
    match (verb, rest.as_slice()) {
        ("merge", [branch]) => Some(GitOpCommand::Merge(branch.to_string())),
        ("rebase", ["-i" | "--interactive"]) => Some(GitOpCommand::RebaseInteractive(None)),
        ("rebase", ["-i" | "--interactive", rev]) => {
            Some(GitOpCommand::RebaseInteractive(Some(rev.to_string())))
        }
        ("rebase", [onto]) => Some(GitOpCommand::Rebase(onto.to_string())),
        ("upstream", [up]) => Some(GitOpCommand::Upstream(up.to_string())),
        ("branch", ["-d" | "-D" | "--delete", names @ ..]) => {
            one(names).map(GitOpCommand::DeleteBranch)
        }
        ("branch", ["-m" | "-M" | "--move", new]) => {
            Some(GitOpCommand::RenameBranch(None, new.to_string()))
        }
        ("branch", ["-m" | "-M" | "--move", old, new]) => Some(GitOpCommand::RenameBranch(
            Some(old.to_string()),
            new.to_string(),
        )),
        ("tag", ["-d" | "--delete", names @ ..]) => one(names).map(GitOpCommand::DeleteTag),
        _ => None,
    }
}

/// `:set <name> N` / `:set <name>=N` for a numeric layout option.
fn parse_set_number(cmd: &str, name: &str) -> Option<u16> {
    let rest = cmd
        .strip_prefix("set ")?
        .trim_start()
        .strip_prefix(name)?
        .trim();
    let rest = rest.strip_prefix('=').map(str::trim).unwrap_or(rest);
    rest.trim_end_matches('%').parse().ok()
}

fn parse_lineno_command(cmd: &str) -> Option<(u32, LineSide)> {
    if let Some(rest) = cmd.strip_prefix('o') {
        rest.parse::<u32>().ok().map(|n| (n, LineSide::Old))
    } else {
        cmd.parse::<u32>().ok().map(|n| (n, LineSide::New))
    }
}

/// Handle actions in Search mode (text input for /pattern)
pub fn handle_search_action(app: &mut App, action: Action) {
    match action {
        Action::InsertChar(c) => app.search_buffer.push(c),
        Action::Paste(text) => push_single_line(&mut app.search_buffer, &text),
        Action::DeleteChar => {
            app.search_buffer.pop();
        }
        Action::DeleteWord if !app.search_buffer.is_empty() => {
            while app
                .search_buffer
                .chars()
                .last()
                .map(|c| c.is_whitespace())
                .unwrap_or(false)
            {
                app.search_buffer.pop();
            }
            while app
                .search_buffer
                .chars()
                .last()
                .map(|c| !c.is_whitespace())
                .unwrap_or(false)
            {
                app.search_buffer.pop();
            }
        }
        Action::ClearLine => {
            app.search_buffer.clear();
        }
        Action::ExitMode => app.exit_search_mode(),
        Action::SubmitInput => {
            if app.searching_help() {
                app.search_in_help_from_scroll();
            } else {
                app.search_in_diff_from_cursor();
            }
            app.exit_search_mode();
        }
        Action::Quit => app.should_quit = true,
        _ => {}
    }
}

/// Handle actions in Comment mode (text input for comments)
pub fn handle_comment_action(app: &mut App, action: Action) {
    if matches!(
        action,
        Action::InsertChar(_)
            | Action::Paste(_)
            | Action::DeleteChar
            | Action::DeleteWord
            | Action::ClearLine
            | Action::TextCursorLeft
            | Action::TextCursorRight
            | Action::TextCursorLineStart
            | Action::TextCursorLineEnd
            | Action::TextCursorWordLeft
            | Action::TextCursorWordRight
    ) {
        app.comment_preferred_column = None;
    }
    match action {
        Action::InsertChar(c) => {
            app.comment_buffer.insert(app.comment_cursor, c);
            app.comment_cursor += c.len_utf8();
        }
        Action::Paste(text) => {
            let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
            app.comment_buffer
                .insert_str(app.comment_cursor, &normalized);
            app.comment_cursor += normalized.len();
        }
        Action::DeleteChar => {
            app.comment_cursor = delete_char_before(&mut app.comment_buffer, app.comment_cursor);
        }
        Action::ExitMode => app.exit_comment_mode(),
        Action::SubmitInput => app.save_comment(),
        Action::CycleCommentType => app.cycle_comment_type(),
        Action::CycleCommentTypeReverse => app.cycle_comment_type_reverse(),
        Action::TextCursorUp | Action::TextCursorDown => {
            app.comment_cursor = crate::ui::comment_panel::move_comment_cursor(
                &app.comment_buffer,
                app.comment_cursor,
                app.diff_state.viewport_width.saturating_sub(1),
                matches!(action, Action::TextCursorDown),
                &mut app.comment_preferred_column,
            );
        }
        Action::TextCursorLeft => {
            app.comment_cursor = prev_char_boundary(&app.comment_buffer, app.comment_cursor);
        }
        Action::TextCursorRight => {
            app.comment_cursor = next_char_boundary(&app.comment_buffer, app.comment_cursor);
        }
        Action::TextCursorLineStart => {
            app.comment_cursor = comment_line_start(&app.comment_buffer, app.comment_cursor);
        }
        Action::TextCursorLineEnd => {
            app.comment_cursor = comment_line_end(&app.comment_buffer, app.comment_cursor);
        }
        Action::TextCursorWordLeft => {
            app.comment_cursor = comment_word_left(&app.comment_buffer, app.comment_cursor);
        }
        Action::TextCursorWordRight => {
            app.comment_cursor = comment_word_right(&app.comment_buffer, app.comment_cursor);
        }
        Action::DeleteWord => {
            app.comment_cursor = delete_word_before(&mut app.comment_buffer, app.comment_cursor);
        }
        Action::ClearLine => {
            app.comment_buffer.clear();
            app.comment_cursor = 0;
        }
        Action::Quit => app.should_quit = true,
        _ => {}
    }
}

/// Handle actions in Confirm mode (Y/N prompts)
pub fn handle_confirm_action(app: &mut App, action: Action) {
    // The merge-commit prompt returns to the merge view instead of
    // quitting, so route it before the quit-flow arms below.
    if app.pending_confirm == Some(app::ConfirmAction::ContinueMergeOperation) {
        match action {
            Action::ConfirmYes => {
                app.pending_confirm = None;
                app.confirm_continue_merge_operation();
            }
            Action::ConfirmNo => {
                app.pending_confirm = None;
                app.input_mode = InputMode::MergeConflict;
            }
            _ => {}
        }
        return;
    }

    // Destructive local-change prompts: act, then return to the review or
    // the stash picker rather than running the quit flow below.
    match app.pending_confirm {
        Some(app::ConfirmAction::GitCommand) => {
            match action {
                Action::ConfirmYes => app.confirm_git_yes(),
                Action::ConfirmNo => app.confirm_git_no(),
                _ => {}
            }
            return;
        }
        Some(app::ConfirmAction::DiscardChanges) => {
            match action {
                Action::ConfirmYes => app.confirm_discard(),
                Action::ConfirmNo => app.cancel_discard(),
                _ => {}
            }
            return;
        }
        Some(app::ConfirmAction::DropStash) => {
            match action {
                Action::ConfirmYes => app.confirm_drop_stash(),
                Action::ConfirmNo => app.cancel_drop_stash(),
                _ => {}
            }
            return;
        }
        Some(app::ConfirmAction::HistoryRewrite) => {
            match action {
                Action::ConfirmYes => app.confirm_history_action(),
                Action::ConfirmNo => app.cancel_history_action(),
                _ => {}
            }
            return;
        }
        _ => {}
    }

    // Only the quit prompts end the session. A Sessions-tab delete answers
    // in place and returns to the selector.
    let quits = app.pending_confirm != Some(app::ConfirmAction::DeleteSession);
    match action {
        Action::ConfirmYes => {
            if let Some(app::ConfirmAction::DeleteSession) = app.pending_confirm {
                app.sessions_tab_delete_selected();
            }
            if let Some(app::ConfirmAction::CopyAndQuit) = app.pending_confirm {
                let slug = app.session_slug();
                if app.output_to_stdout {
                    match generate_export_content(
                        &app.session,
                        &app.diff_source,
                        &app.comment_types,
                        &app.export,
                        &app.forge_review_threads,
                        slug.as_deref(),
                    ) {
                        Ok(content) => app.pending_stdout_output = Some(content),
                        Err(e) => app.set_warning(format!("{e}")),
                    }
                } else {
                    match export_to_clipboard(
                        &app.session,
                        &app.diff_source,
                        &app.comment_types,
                        &app.export,
                        &app.forge_review_threads,
                        slug.as_deref(),
                    ) {
                        Ok(msg) => app.set_message(msg),
                        Err(e) => app.set_warning(format!("{e}")),
                    }
                }
            }
            app.exit_confirm_mode();
            app.should_quit = quits;
        }
        Action::ConfirmNo => {
            app.exit_confirm_mode();
            app.should_quit = quits;
        }
        Action::Quit => app.should_quit = true,
        _ => {}
    }
}

/// Handle actions in CommitSelect mode.
///
/// CommitSelect actually drives the review target selector, which has two
/// tabs (Local and Pull Requests). Tab-shared actions (switch tab, quit) are
/// handled first; per-tab dispatch follows.
pub fn handle_commit_select_action(app: &mut App, action: Action) {
    // Filter-editing sub-state on the PR tab. Routed before tab dispatch
    // because typed characters must go to the filter buffer rather than
    // to local-commit movement.
    if app.pr_filter_editing() {
        handle_pr_filter_action(app, action);
        return;
    }
    // Same for the Local tab's log filter prompt.
    if app.target_tab == TargetTab::Local && app.commit_filter_editing() {
        handle_commit_filter_action(app, action);
        return;
    }

    match action {
        Action::TargetSelectorTabNext => app.cycle_target_tab(true),
        Action::TargetSelectorTabPrev => app.cycle_target_tab(false),
        Action::EnterCommandMode => app.enter_command_mode(),
        Action::Quit => app.should_quit = true,
        Action::QuitHint => app.set_message(QUIT_HINT_MESSAGE),
        Action::ExitMode => {
            // Esc during an in-flight PR open aborts the load and stays
            // in the selector. Takes precedence over the
            // commit-selection-range exit so the user isn't stuck staring
            // at a spinner.
            if app.cancel_pr_open() {
                return;
            }
            // An active log filter clears before the selector closes.
            if app.target_tab == TargetTab::Local && app.clear_commit_filter() {
                return;
            }
            if app.commit_selection_range.is_none() {
                return;
            }
            if let Err(e) = app.exit_commit_select_mode() {
                app.set_error(format!("Failed to reload changes: {e}"));
            }
        }
        other => match app.target_tab {
            TargetTab::Local => handle_local_target_action(app, other),
            TargetTab::PullRequests => handle_pr_target_action(app, other),
            TargetTab::Sessions => handle_sessions_target_action(app, other),
        },
    }
}

fn handle_sessions_target_action(app: &mut App, action: Action) {
    match action {
        Action::CommitSelectUp => app.sessions_tab_cursor_up(),
        Action::CommitSelectDown => app.sessions_tab_cursor_down(),
        Action::ConfirmCommitSelect => {
            if let Err(e) = app.sessions_tab_select() {
                app.set_error(format!("Failed to open session: {e}"));
            }
        }
        // `a` arrives as the Local tab's abort; here it shows empty reviews.
        Action::SessionsTabToggleShowEmpty | Action::AbortOperation => {
            app.sessions_tab_toggle_show_empty()
        }
        Action::SessionsTabDelete if app.sessions_tab.cursor_session().is_some() => {
            app.enter_confirm_mode(app::ConfirmAction::DeleteSession);
        }
        // Space is a no-op: sessions are picked, not multi-selected.
        Action::ToggleCommitSelect => {}
        _ => {}
    }
}

fn handle_local_target_action(app: &mut App, action: Action) {
    match action {
        Action::CommitSelectUp => app.commit_select_up(),
        Action::CommitSelectDown => app.commit_select_down(),
        Action::ToggleCommitSelect => {
            // If on expand row, expand commits instead of toggling selection
            if app.is_on_expand_row() {
                if let Err(e) = app.expand_commit() {
                    app.set_error(format!("Failed to load commits: {e}"));
                }
            } else {
                // Toggle + auto-advance so repeated Space sweeps a range.
                app.toggle_commit_selection_and_advance();
            }
        }
        Action::ConfirmCommitSelect => {
            // if on expand row, expand commit instead of confirming
            if app.is_on_expand_row() {
                if let Err(e) = app.expand_commit() {
                    app.set_error(format!("Failed to load commits: {e}"));
                }
            } else if app.cursor_on_merge_row() {
                if let Err(e) = app.open_merge_view(InputMode::CommitSelect) {
                    app.set_error(format!("Failed to open merge view: {e}"));
                }
            } else if let Err(e) = app.confirm_commit_selection() {
                app.set_error(format!("Failed to load commits: {e}"));
            }
        }
        Action::SelectorCherryPick if !app.is_on_expand_row() => {
            app.selector_cherry_pick();
        }
        Action::SelectorRebase if !app.is_on_expand_row() => {
            app.selector_rebase_onto_cursor();
        }
        Action::SelectorSquash if !app.is_on_expand_row() => {
            app.selector_squash();
        }
        Action::SelectorCheckout => match app.cursor_checkout_target() {
            Some(target) => app.checkout_target(&target, false),
            None => app.set_message("Move the cursor onto a commit to check out"),
        },
        Action::SelectorNewBranch => {
            let start = app.cursor_commit_id();
            app.start_new_branch_prompt(start);
        }
        Action::AbortOperation => app.abort_operation(),
        Action::OpenBranchPicker => app.open_branch_picker(),
        Action::SelectorRevert => app.revert_cursor_commit(),
        Action::SelectorReword => app.start_reword_composer(),
        Action::SelectorDrop => app.request_drop_commit(),
        Action::SelectorFixup => app.fixup_cursor_commit(),
        Action::SelectorReset => app.start_reset_prompt(),
        Action::SelectorTag => app.start_tag_prompt(),
        Action::SelectorCopyHash => app.copy_cursor_commit_hash(),
        Action::SelectorFetch => app.start_remote_op(RemoteOp::Fetch),
        Action::SelectorShowBranch => app.open_branch_picker_for_log(),
        Action::SelectorInteractiveRebase => app.open_rebase_editor_at_cursor(),
        Action::BeginTargetFilter => app.begin_commit_filter(),
        _ => {}
    }
}

fn handle_pr_target_action(app: &mut App, action: Action) {
    match action {
        Action::CommitSelectUp => app.pr_tab_cursor_up(),
        Action::CommitSelectDown => app.pr_tab_cursor_down(),
        Action::ConfirmCommitSelect => {
            app.pr_tab_select();
        }
        Action::ToggleCommitSelect => {
            // Space is a no-op on the PR tab (spec).
        }
        Action::BeginTargetFilter => {
            app.begin_pr_filter();
        }
        Action::TogglePrReviewRequestedFilter => {
            app.toggle_pr_review_requested_filter();
        }
        _ => {}
    }
}

fn handle_pr_filter_action(app: &mut App, action: Action) {
    match action {
        Action::InsertChar(c) => app.pr_filter_insert_char(c),
        Action::Paste(text) => {
            for ch in text.chars().filter(|c| !matches!(*c, '\n' | '\r')) {
                app.pr_filter_insert_char(ch);
            }
        }
        Action::DeleteChar => app.pr_filter_delete_char(),
        Action::DeleteWord => {
            // Soft word-delete: collapses to clear-line for the v1 cut.
            app.pr_filter_clear();
        }
        Action::ClearLine => app.pr_filter_clear(),
        Action::SubmitInput => app.commit_pr_filter(),
        Action::ExitMode => app.cancel_pr_filter(),
        Action::Quit => app.should_quit = true,
        _ => {}
    }
}

/// Handle actions when inline commit selector panel is focused
/// Keys while the log filter prompt is open (log pane or the selector's
/// Local tab). Mirrors `handle_pr_filter_action`.
fn handle_commit_filter_action(app: &mut App, action: Action) {
    match action {
        Action::InsertChar(c) => app.commit_filter_insert_char(c),
        Action::Paste(text) => {
            for ch in text.chars().filter(|c| !c.is_control()) {
                app.commit_filter_insert_char(ch);
            }
        }
        Action::DeleteChar => app.commit_filter_delete_char(),
        Action::DeleteWord => app.commit_filter_delete_word(),
        Action::ClearLine => app.commit_filter_clear_draft(),
        Action::SubmitInput => app.apply_commit_filter(),
        Action::ExitMode => app.cancel_commit_filter(),
        _ => {}
    }
}

/// Keys in the find-in-files results view.
pub fn handle_find_results_action(app: &mut App, action: Action) {
    match action {
        Action::CursorDown(n) | Action::MouseScrollDown(n) => app.find_select_down(n),
        Action::CursorUp(n) | Action::MouseScrollUp(n) => app.find_select_up(n),
        Action::HalfPageDown => app.find_page(true, true),
        Action::HalfPageUp => app.find_page(false, true),
        Action::PageDown => app.find_page(true, false),
        Action::PageUp => app.find_page(false, false),
        Action::GoToTop => app.find_select_first(),
        Action::GoToBottom => app.find_select_last(),
        Action::SubmitInput => app.find_open_selected(false),
        Action::EditFile => app.find_open_selected(true),
        Action::BeginTargetFilter => app.start_find_prompt(),
        Action::EnterCommandMode => app.enter_command_mode(),
        Action::ExitMode => app.exit_find_results(),
        _ => {}
    }
}

/// Keys in the stash picker modal.
pub fn handle_stash_picker_action(app: &mut App, action: Action) {
    match action {
        Action::StashPickerDown => app.stash_picker_down(),
        Action::StashPickerUp => app.stash_picker_up(),
        Action::StashPickerPop => app.stash_picker_apply(true),
        Action::StashPickerApply => app.stash_picker_apply(false),
        Action::StashPickerView => app.stash_picker_view(),
        Action::StashPickerDrop => app.request_drop_stash(),
        Action::ExitMode => app.close_stash_picker(),
        _ => {}
    }
}

/// Keys in the branch picker modal.
/// Interactive rebase editor. Letters are commands unless a reword subject
/// is being typed, when they are text.
pub fn handle_rebase_editor_action(app: &mut App, action: Action) {
    use crate::vcs::traits::RebaseAction;
    let editing = app
        .rebase_editor
        .as_ref()
        .is_some_and(|e| e.editing.is_some());
    if editing {
        match action {
            Action::InsertChar(c) => app.rebase_editor_type(c),
            Action::Paste(text) => {
                for c in text.chars().filter(|c| !c.is_control()) {
                    app.rebase_editor_type(c);
                }
            }
            Action::DeleteChar => app.rebase_editor_backspace(),
            Action::ClearLine => app.rebase_editor_clear_subject(),
            Action::SubmitInput => app.rebase_editor_finish_reword(),
            Action::ExitMode => app.rebase_editor_cancel_reword(),
            _ => {}
        }
        return;
    }
    match action {
        Action::CursorDown(n) => app.rebase_editor_move(n as isize),
        Action::CursorUp(n) => app.rebase_editor_move(-(n as isize)),
        Action::InsertChar('j') => app.rebase_editor_move(1),
        Action::InsertChar('k') => app.rebase_editor_move(-1),
        Action::InsertChar('J') => app.rebase_editor_shift(1),
        Action::InsertChar('K') => app.rebase_editor_shift(-1),
        Action::InsertChar('p') => app.rebase_editor_set(RebaseAction::Pick),
        Action::InsertChar('r') => app.rebase_editor_set(RebaseAction::Reword),
        Action::InsertChar('s') => app.rebase_editor_set(RebaseAction::Squash),
        Action::InsertChar('f') => app.rebase_editor_set(RebaseAction::Fixup),
        Action::InsertChar('d') => app.rebase_editor_set(RebaseAction::Drop),
        Action::InsertChar('u') => app.rebase_editor_reset(),
        Action::SubmitInput => app.rebase_editor_run(),
        Action::ExitMode | Action::InsertChar('q') => app.close_rebase_editor(),
        _ => {}
    }
}

/// Git console: scroll, toggle reads, clear, close.
pub fn handle_console_action(app: &mut App, action: Action) {
    match action {
        Action::CursorDown(n) => app.console_scroll(-(n as isize)),
        Action::CursorUp(n) => app.console_scroll(n as isize),
        Action::PageDown => app.console_page(false),
        Action::PageUp => app.console_page(true),
        Action::GoToTop => app.console_scroll(isize::MAX / 2),
        Action::GoToBottom => app.console_scroll(-(isize::MAX / 2)),
        Action::ToggleExpand => app.console_toggle_mutating(),
        Action::ClearLine => app.console_clear(),
        Action::EnterCommandMode => {
            app.close_console();
            app.enter_command_mode();
        }
        Action::ExitMode => app.close_console(),
        _ => {}
    }
}

/// Command palette: typing filters, arrows move, Enter runs the entry.
pub fn handle_palette_action(app: &mut App, action: Action) {
    match action {
        Action::CursorDown(n) => app.palette_move(n as isize),
        Action::CursorUp(n) => app.palette_move(-(n as isize)),
        Action::PageDown => app.palette_move(10),
        Action::PageUp => app.palette_move(-10),
        Action::InsertChar(c) => app.palette_push(c),
        Action::Paste(text) => {
            for c in text.chars().filter(|c| !c.is_control()) {
                app.palette_push(c);
            }
        }
        Action::DeleteChar => app.palette_pop(),
        Action::ClearLine => app.palette_clear(),
        Action::SubmitInput => app.palette_run(),
        Action::ExitMode => {
            if app.palette.as_ref().is_some_and(|p| !p.query.is_empty()) {
                app.palette_clear();
            } else {
                app.close_palette();
            }
        }
        _ => {}
    }
}

pub fn handle_branch_picker_action(app: &mut App, action: Action) {
    match action {
        Action::BranchPickerDown => app.branch_picker_down(),
        Action::BranchPickerUp => app.branch_picker_up(),
        Action::BranchPickerConfirm => app.branch_picker_confirm(false),
        Action::BranchPickerConfirmSmart => app.branch_picker_confirm(true),
        Action::BranchPickerPick => app.branch_picker_toggle_pick(),
        Action::BranchPickerCompare => app.branch_picker_compare(),
        Action::BranchPickerLog => app.branch_picker_log(),
        Action::BranchPickerFetch => app.start_remote_op(RemoteOp::Fetch),
        Action::BranchPickerDelete => app.branch_picker_delete(),
        Action::BranchPickerRename => app.branch_picker_rename(),
        Action::BranchPickerMerge => app.branch_picker_merge(),
        Action::BranchPickerRebase => app.branch_picker_rebase(),
        Action::BranchPickerTrack => app.branch_picker_track(),
        Action::InsertChar(c) => app.branch_picker_push(c),
        Action::Paste(text) => {
            for c in text.chars().filter(|c| !c.is_control()) {
                app.branch_picker_push(c);
            }
        }
        Action::DeleteChar => app.branch_picker_pop(),
        Action::ClearLine => app.branch_picker_clear(),
        Action::ExitMode => {
            // Esc clears an active filter first, then closes.
            if app
                .branch_picker
                .as_ref()
                .is_some_and(|p| !p.filter.is_empty())
            {
                app.branch_picker_clear();
            } else {
                app.close_branch_picker();
            }
        }
        _ => {}
    }
}

/// Keys while the persistent log pane holds focus (Normal mode). Movement
/// schedules a preview of the cursor commit; Space builds a pick set; Enter
/// loads it as the review; `p` / `b` / `s` run the history operations.
fn handle_log_pane_action(app: &mut App, action: Action) {
    if app.commit_filter_editing() {
        handle_commit_filter_action(app, action);
        return;
    }
    match action {
        Action::BeginTargetFilter => app.begin_commit_filter(),
        Action::CommitSelectDown | Action::CursorDown(_) | Action::MouseScrollDown(_) => {
            let n = match action {
                Action::CursorDown(n) | Action::MouseScrollDown(n) => n,
                _ => 1,
            };
            app.log_pane_move_down(n);
        }
        Action::CommitSelectUp | Action::CursorUp(_) | Action::MouseScrollUp(_) => {
            let n = match action {
                Action::CursorUp(n) | Action::MouseScrollUp(n) => n,
                _ => 1,
            };
            app.log_pane_move_up(n);
        }
        Action::GoToTop => app.log_pane_go_top(),
        Action::GoToBottom => app.log_pane_go_bottom(),
        Action::HalfPageDown => app.log_pane_page(true, true),
        Action::HalfPageUp => app.log_pane_page(false, true),
        Action::PageDown => app.log_pane_page(true, false),
        Action::PageUp => app.log_pane_page(false, false),
        Action::ToggleCommitSelect | Action::ToggleExpand => app.log_pane_toggle_pick(),
        Action::ConfirmCommitSelect | Action::SelectFile => {
            if let Err(e) = app.log_pane_confirm() {
                app.set_error(format!("Failed to load commits: {e}"));
            }
        }
        Action::SelectorCherryPick => app.log_pane_history_op(|a| a.selector_cherry_pick()),
        Action::SelectorRebase => app.log_pane_history_op(|a| a.selector_rebase_onto_cursor()),
        Action::SelectorSquash => app.log_pane_history_op(|a| a.selector_squash()),
        // Read the cursor row inside the swap, act on it outside: the
        // post-checkout refresh touches the pane's own state.
        Action::SelectorCheckout => match app.with_log_state(|a| a.cursor_checkout_target()) {
            Some(target) => app.checkout_target(&target, false),
            None => app.set_message("Move the cursor onto a commit to check out"),
        },
        Action::SelectorNewBranch => {
            let start = app.with_log_state(|a| a.cursor_commit_id());
            app.start_new_branch_prompt(start);
        }
        Action::AbortOperation => app.abort_operation(),
        Action::OpenBranchPicker => app.open_branch_picker(),
        Action::SelectorRevert => app.revert_cursor_commit(),
        Action::SelectorReword => app.start_reword_composer(),
        Action::SelectorDrop => app.request_drop_commit(),
        Action::SelectorFixup => app.fixup_cursor_commit(),
        Action::SelectorReset => app.start_reset_prompt(),
        Action::SelectorTag => app.start_tag_prompt(),
        Action::SelectorCopyHash => app.copy_cursor_commit_hash(),
        Action::SelectorFetch => app.start_remote_op(RemoteOp::Fetch),
        Action::SelectorShowBranch => app.open_branch_picker_for_log(),
        Action::SelectorInteractiveRebase => app.open_rebase_editor_at_cursor(),
        // Esc clears an active filter first; a second Esc leaves the pane.
        Action::ExitMode => {
            if !app.clear_commit_filter() {
                app.focused_panel = FocusedPanel::Diff;
            }
        }
        _ => handle_shared_normal_action(app, action),
    }
}

pub fn handle_commit_selector_action(app: &mut App, action: Action) {
    if app.log_pane_active() {
        handle_log_pane_action(app, action);
        return;
    }
    match action {
        // `j`/`k` track screen direction: in ascending (oldest-first) order the
        // display is reversed, so moving down the screen moves toward newer
        // (lower) data indices.
        Action::CursorDown(_) => {
            if app.commits_ascending() {
                app.commit_select_up();
            } else {
                app.commit_select_down();
            }
        }
        Action::CursorUp(_) => {
            if app.commits_ascending() {
                app.commit_select_down();
            } else {
                app.commit_select_up();
            }
        }
        // Toggle + auto-advance so repeated presses sweep a contiguous run.
        Action::ToggleExpand | Action::ToggleCommitSelect | Action::SelectFile => {
            app.toggle_commit_selection_and_advance();
            // PR mode reloads via the forge `compare` API on a background
            // thread (persisting the new range so it survives a restart);
            // local reviews reload from the VCS.
            if let Err(e) = app.reload_inline_selection_for_source() {
                app.set_error(format!("Failed to load diff: {e}"));
            }
        }
        Action::ExitMode => {
            app.focused_panel = FocusedPanel::Diff;
        }
        _ => handle_shared_normal_action(app, action),
    }
}

/// Handle actions in VisualSelect mode
/// Handle actions on the fullscreen merge conflict screen.
pub fn handle_merge_conflict_action(app: &mut App, action: Action) {
    use crate::vcs::ConflictResolution;

    // Any action other than Esc cancels a pending discard warning.
    if action != Action::ExitMode {
        app.merge_disarm_discard();
    }

    match action {
        Action::AbortOperation => app.abort_operation(),
        Action::CursorDown(n) => app.merge_scroll_down(n),
        Action::CursorUp(n) => app.merge_scroll_up(n),
        Action::HalfPageDown => app.merge_scroll_down(app.merge_half_page()),
        Action::HalfPageUp => app.merge_scroll_up(app.merge_half_page()),
        Action::PageDown => app.merge_scroll_down(app.merge_full_page()),
        Action::PageUp => app.merge_scroll_up(app.merge_full_page()),
        Action::GoToTop => app.merge_scroll_to_top(),
        Action::GoToBottom => app.merge_scroll_to_bottom(),
        Action::MergeNextConflict => app.merge_next_conflict(),
        Action::MergePrevConflict => app.merge_prev_conflict(),
        Action::MergeNextFile => app.merge_next_file(),
        Action::MergePrevFile => app.merge_prev_file(),
        Action::MergeTakeOurs => app.merge_resolve_current(ConflictResolution::Ours),
        Action::MergeTakeTheirs => app.merge_resolve_current(ConflictResolution::Theirs),
        Action::MergeTakeBoth => app.merge_resolve_current(ConflictResolution::Both),
        Action::MergeUnresolve => app.merge_resolve_current(ConflictResolution::Unresolved),
        Action::MergeSaveFile => {
            if let Err(e) = app.merge_save_current_file() {
                app.set_error(format!("Failed to write file: {e}"));
            }
        }
        Action::MergeEditFile => app.merge_edit_current_file(),
        Action::ExitMode => app.close_merge_view(),
        Action::Quit => app.should_quit = true,
        _ => {}
    }
}

pub fn handle_visual_action(app: &mut App, action: Action) {
    match action {
        Action::CursorDown(n) => {
            app.cursor_down(n);
            app.extend_visual_to_cursor();
        }
        Action::CursorUp(n) => {
            app.cursor_up(n);
            app.extend_visual_to_cursor();
        }
        Action::AddRangeComment | Action::AddRangeSuggestion => {
            if app.visual_selection_line_range().is_some() {
                if action == Action::AddRangeSuggestion {
                    app.enter_suggestion_from_visual();
                } else {
                    app.enter_comment_from_visual();
                }
            } else {
                app.set_warning("Invalid selection - move cursor to a diff line");
                app.exit_visual_mode();
            }
        }
        Action::StageHunk => app.stage_selected_lines(false),
        Action::UnstageHunk => app.stage_selected_lines(true),
        Action::ExportToClipboard => {
            match app.copy_visual_selection() {
                Ok(0) => app.set_message("Nothing to copy"),
                Ok(n) => app.set_message(format!("Copied {n} chars")),
                Err(e) => app.set_warning(format!("{e}")),
            }
            app.exit_visual_mode();
        }
        Action::ExitMode => app.exit_visual_mode(),
        Action::Quit => app.should_quit = true,
        Action::QuitHint => app.set_message(QUIT_HINT_MESSAGE),
        Action::ScrollViewDown(n) | Action::MouseScrollDown(n) => app.scroll_view_down(n),
        Action::ScrollViewUp(n) | Action::MouseScrollUp(n) => app.scroll_view_up(n),
        Action::HalfPageDown => app.page_down(app.diff_state.viewport_height / 2),
        Action::HalfPageUp => app.page_up(app.diff_state.viewport_height / 2),
        Action::PageDown => app.page_down(app.diff_state.viewport_height),
        Action::PageUp => app.page_up(app.diff_state.viewport_height),
        _ => {}
    }
    clear_visual_if_cursor_offscreen(app);
}

/// Handle actions when file list panel is focused
pub fn handle_file_list_action(app: &mut App, action: Action) {
    // An open `i`/`e`/`/` prompt owns keyboard input until it is submitted or
    // cancelled. The wheel still scrolls the list underneath it.
    if app.file_tree_prompt_editing()
        && !matches!(
            action,
            Action::MouseScrollDown(_) | Action::MouseScrollUp(_)
        )
    {
        handle_file_tree_prompt_action(app, action);
        return;
    }
    match action {
        Action::FileTreeFilterInclude => app.begin_file_tree_prompt(FileTreePrompt::Include),
        Action::FileTreeFilterExclude => app.begin_file_tree_prompt(FileTreePrompt::Exclude),
        Action::FileTreeClearInclude => app.clear_include_filter(),
        Action::FileTreeClearExclude => app.clear_exclude_filter(),
        Action::FileTreeSearch => app.begin_file_tree_prompt(FileTreePrompt::Search),
        // With a tree search active, n/N step file matches. Otherwise they
        // fall through to the diff search so the shared binding still works.
        Action::NextChange if app.file_tree_search_active() => app.file_tree_search_next(),
        Action::PrevChange if app.file_tree_search_active() => app.file_tree_search_prev(),
        Action::CursorDown(n) => app.file_list_down(n),
        Action::CursorUp(n) => app.file_list_up(n),
        Action::ScrollLeft(n) => app.file_list_state.scroll_left(n),
        Action::ScrollRight(n) => app.file_list_state.scroll_right(n),
        Action::MouseScrollDown(n) => app.file_list_viewport_scroll_down(n),
        Action::MouseScrollUp(n) => app.file_list_viewport_scroll_up(n),
        Action::SelectFile | Action::ToggleExpand => {
            if let Some(item) = app.get_selected_tree_item() {
                match item {
                    FileTreeItem::Directory { path, .. } => app.toggle_directory(&path),
                    FileTreeItem::File { file_idx, .. } => {
                        app.jump_to_file(file_idx);
                        app.focused_panel = FocusedPanel::Diff;
                    }
                }
            }
        }
        Action::ToggleReviewed => {
            if let Some(FileTreeItem::File { file_idx, .. }) = app.get_selected_tree_item() {
                app.toggle_reviewed_for_file_idx(file_idx, false);
            } else {
                app.set_warning("Select a file to toggle reviewed");
            }
        }
        _ => handle_shared_normal_action(app, action),
    }
}

/// The changes window: movement, folding, staging, discard and commit. The
/// shared normal-mode actions (`:`, `?`, Tab, the leader) still apply.
pub fn handle_changes_pane_action(app: &mut App, action: Action) {
    match action {
        Action::CursorDown(n) | Action::MouseScrollDown(n) => app.changes_move_down(n),
        Action::CursorUp(n) | Action::MouseScrollUp(n) => app.changes_move_up(n),
        Action::GoToTop => app.changes_go_top(),
        Action::GoToBottom => app.changes_go_bottom(),
        Action::HalfPageDown => app.changes_page(true, true),
        Action::HalfPageUp => app.changes_page(false, true),
        Action::PageDown => app.changes_page(true, false),
        Action::PageUp => app.changes_page(false, false),
        Action::SelectFile | Action::SelectFileFull | Action::ToggleExpand => app.changes_open(),
        Action::ChangesToggleStage => app.changes_toggle_stage(),
        Action::StageFile => app.changes_stage(),
        Action::UnstageFile => app.changes_unstage(),
        Action::ChangesStageAll => app.changes_stage_all(),
        Action::ChangesUnstageAll => app.changes_unstage_all(),
        Action::ChangesDiscard => app.changes_request_discard(),
        Action::ChangesCommit => app.changes_commit(false),
        Action::ChangesAmend => app.changes_commit(true),
        Action::ChangesRefresh => {
            app.refresh_changes_pane();
            app.set_message("Changes: status re-read");
        }
        Action::ChangesCollapse => app.changes_collapse(),
        Action::ChangesExpand => app.changes_expand(),
        Action::ExpandAll => app.changes_expand_all(),
        Action::CollapseAll => app.changes_collapse_all(),
        Action::ExitMode => app.focused_panel = FocusedPanel::Diff,
        _ => handle_shared_normal_action(app, action),
    }
}

/// Handle input while a file-tree prompt (`i` include, `e` exclude, `/`
/// search) is open. Mirrors `handle_pr_filter_action` for the target selector.
fn handle_file_tree_prompt_action(app: &mut App, action: Action) {
    match action {
        Action::InsertChar(c) => app.file_tree_prompt_insert_char(c),
        Action::Paste(text) => app.file_tree_prompt_insert_str(&text),
        Action::DeleteChar => app.file_tree_prompt_delete_char(),
        Action::DeleteWord => app.file_tree_prompt_delete_word(),
        Action::ClearLine => app.file_tree_prompt_clear_line(),
        Action::SubmitInput => app.commit_file_tree_prompt(),
        Action::ExitMode => app.cancel_file_tree_prompt(),
        _ => {}
    }
}

/// Handle actions in `InputMode::ThemePicker`. Mirrors
/// `handle_file_list_action`'s prompt-editing gate: while the `/` filter
/// draft is open, input goes to `handle_theme_picker_filter_action` instead
/// of list navigation.
pub fn handle_theme_picker_action(app: &mut App, action: Action) {
    if app.theme_picker_filtering() {
        handle_theme_picker_filter_action(app, action);
        return;
    }
    match action {
        Action::CursorDown(n) => app.move_theme_picker_selection(n as isize),
        Action::CursorUp(n) => app.move_theme_picker_selection(-(n as isize)),
        Action::ThemePickerFilter => app.begin_theme_picker_filter(),
        Action::SubmitInput => app.confirm_theme_picker(),
        Action::ExitMode => app.cancel_theme_picker(),
        Action::Quit => app.should_quit = true,
        _ => {}
    }
}

/// Handle input while the theme picker's `/` filter draft is open. Mirrors
/// `handle_file_tree_prompt_action`.
fn handle_theme_picker_filter_action(app: &mut App, action: Action) {
    match action {
        Action::InsertChar(c) => app.theme_picker_filter_insert_char(c),
        Action::Paste(text) => {
            for c in text.chars().filter(|c| !c.is_control()) {
                app.theme_picker_filter_insert_char(c);
            }
        }
        Action::DeleteChar => app.theme_picker_filter_delete_char(),
        Action::ClearLine => app.theme_picker_filter_clear_line(),
        Action::SubmitInput => app.commit_theme_picker_filter(),
        Action::ExitMode => app.cancel_theme_picker_filter(),
        _ => {}
    }
}

/// Handle actions when the comment navigator panel is focused
pub fn handle_comment_navigator_action(app: &mut App, action: Action) {
    match action {
        Action::CursorDown(n) => {
            app.comment_navigator_down(n);
            // Auto-follow: scroll the diff to the selected comment without
            // stealing focus from the navigator (Enter still jumps + focuses).
            let saved_focus = app.focused_panel;
            app.jump_to_selected_comment();
            app.focused_panel = saved_focus;
        }
        Action::CursorUp(n) => {
            app.comment_navigator_up(n);
            let saved_focus = app.focused_panel;
            app.jump_to_selected_comment();
            app.focused_panel = saved_focus;
        }
        Action::ScrollLeft(n) => app.comment_navigator_state.scroll_left(n),
        Action::ScrollRight(n) => app.comment_navigator_state.scroll_right(n),
        Action::MouseScrollDown(n) => app.comment_navigator_viewport_scroll_down(n),
        Action::MouseScrollUp(n) => app.comment_navigator_viewport_scroll_up(n),
        Action::SelectFile => {
            app.jump_to_selected_comment();
        }
        _ => handle_shared_normal_action(app, action),
    }
}

/// Enter edit mode for the comment under the cursor, placing the text cursor at
/// the end (vim `A` / non-vim default) or beginning (vim `i`). Surfaces the
/// right message when the comment is read-only or absent.
fn edit_comment_at_cursor(app: &mut App, cursor_at_end: bool) {
    if app.cursor_on_locked_comment() {
        let forge = app.forge_display_name();
        app.set_message(format!(
            "Comment already pushed to {forge} — read only in tuicr"
        ));
    } else if !app.enter_edit_mode(cursor_at_end) {
        if app.cursor_on_remote_thread() {
            let forge = app.forge_display_name();
            app.set_message(format!("{forge} comment — read only in tuicr"));
        } else {
            app.set_message("No comment at cursor");
        }
    }
}

/// Handle actions when diff panel is focused
pub fn handle_diff_action(app: &mut App, action: Action) {
    match action {
        Action::CursorDown(n) => app.cursor_down(n),
        Action::CursorUp(n) => app.cursor_up(n),
        Action::ScrollViewDown(n) => app.scroll_view_down(n),
        Action::ScrollViewUp(n) => app.scroll_view_up(n),
        Action::ScrollLeft(n) => app.scroll_left(n),
        Action::ScrollRight(n) => app.scroll_right(n),
        Action::MouseScrollDown(n) => app.scroll_view_down(n),
        Action::MouseScrollUp(n) => app.scroll_view_up(n),
        Action::SelectFile => {
            if let Some(hit) = app.get_gap_at_cursor() {
                match hit {
                    GapCursorHit::Expander(gap_id, dir) => {
                        let limit = if dir == ExpandDirection::Both {
                            None
                        } else {
                            Some(20)
                        };
                        if let Err(e) = app.expand_gap(gap_id, dir, limit) {
                            app.set_error(format!("Failed to expand: {e}"));
                        }
                    }
                    GapCursorHit::HiddenLines(gap_id) => {
                        if let Err(e) = app.expand_gap(gap_id, ExpandDirection::Both, None) {
                            app.set_error(format!("Failed to expand: {e}"));
                        }
                    }
                    GapCursorHit::ExpandedContent(gap_id) => {
                        app.collapse_gap(gap_id);
                    }
                }
            }
        }
        Action::SelectFileFull => {
            if let Some(hit) = app.get_gap_at_cursor() {
                match hit {
                    GapCursorHit::Expander(gap_id, _) | GapCursorHit::HiddenLines(gap_id) => {
                        if let Err(e) = app.expand_gap(gap_id, ExpandDirection::Both, None) {
                            app.set_error(format!("Failed to expand: {e}"));
                        }
                    }
                    GapCursorHit::ExpandedContent(gap_id) => {
                        app.collapse_gap(gap_id);
                    }
                }
            }
        }
        // n/N step changes. While a diff search is highlighted they fall
        // through to the shared binding and step matches instead.
        Action::NextChange if !app.diff_search_active() => app.next_change(),
        Action::PrevChange if !app.diff_search_active() => app.prev_change(),
        _ => handle_shared_normal_action(app, action),
    }
}

/// Handle actions shared between file list and diff panels in Normal mode
fn handle_shared_normal_action(app: &mut App, action: Action) {
    // Reset quit_warned on any non-quit action
    if !matches!(action, Action::Quit) {
        app.quit_warned = false;
    }

    match action {
        Action::Quit => {
            if app.dirty && app.session.has_comments() && !app.quit_warned {
                app.set_sticky_warning("Unsaved changes. Press q again to quit.");
                app.quit_warned = true;
            } else if app.dirty && !app.session.has_comments() {
                // Dirty from reviewed-file markers only: discard the state and
                // quit instead of warning about unsaved changes.
                app.discard_session_and_quit();
            } else {
                app.should_quit = true;
            }
        }
        Action::QuitHint => app.set_message(QUIT_HINT_MESSAGE),
        Action::FindInFiles => app.start_find_prompt(),
        Action::ExitMode => {
            app.show_file_list = false;
            app.focused_panel = FocusedPanel::Diff;
        }
        Action::HalfPageDown => app.page_down(app.diff_state.viewport_height / 2),
        Action::HalfPageUp => app.page_up(app.diff_state.viewport_height / 2),
        Action::PageDown => app.page_down(app.diff_state.viewport_height),
        Action::PageUp => app.page_up(app.diff_state.viewport_height),
        Action::GoToTop => app.jump_to_file(0),
        Action::GoToBottom => app.jump_to_bottom(),
        Action::NextFile => app.next_file(),
        Action::PrevFile => app.prev_file(),
        Action::NextHunk => app.next_hunk(),
        Action::PrevHunk => app.prev_hunk(),
        Action::NextComment => app.next_comment(),
        Action::PrevComment => app.prev_comment(),
        Action::ToggleReviewed => app.toggle_reviewed(),
        Action::ToggleHunkReviewed => app.toggle_hunk_reviewed(),
        Action::ToggleFocus => {
            let has_selector = app.has_commit_pane();
            // The changes window takes the tree's slot and has no comment
            // navigator under it.
            let has_comments = app.has_comment_navigator_items() && !app.changes_pane_active();
            let left = app.left_panel();
            // Cycle: FileList -> Diff -> CommitSelector -> Comments -> FileList,
            // skipping panels that aren't present.
            app.focused_panel = match (app.focused_panel, has_selector, has_comments) {
                (FocusedPanel::FileList | FocusedPanel::Changes, _, _) => FocusedPanel::Diff,
                (FocusedPanel::Diff, true, _) => FocusedPanel::CommitSelector,
                (FocusedPanel::Diff, false, true) => FocusedPanel::Comments,
                (FocusedPanel::Diff, false, false) => left,
                (FocusedPanel::CommitSelector, _, true) => FocusedPanel::Comments,
                (FocusedPanel::CommitSelector, _, false) => left,
                (FocusedPanel::Comments, _, _) => left,
            };
            if matches!(
                app.focused_panel,
                FocusedPanel::FileList | FocusedPanel::Comments
            ) {
                app.show_file_list = true;
            }
        }
        Action::ToggleFocusReverse => {
            let has_selector = app.has_commit_pane();
            let has_comments = app.has_comment_navigator_items() && !app.changes_pane_active();
            let left = app.left_panel();
            app.focused_panel = match (app.focused_panel, has_selector, has_comments) {
                (FocusedPanel::FileList | FocusedPanel::Changes, _, true) => FocusedPanel::Comments,
                (FocusedPanel::FileList | FocusedPanel::Changes, true, false) => {
                    FocusedPanel::CommitSelector
                }
                (FocusedPanel::FileList | FocusedPanel::Changes, false, false) => {
                    FocusedPanel::Diff
                }
                (FocusedPanel::Comments, true, _) => FocusedPanel::CommitSelector,
                (FocusedPanel::Comments, false, _) => FocusedPanel::Diff,
                (FocusedPanel::Diff, _, _) => left,
                (FocusedPanel::CommitSelector, _, _) => FocusedPanel::Diff,
            };
            if matches!(
                app.focused_panel,
                FocusedPanel::FileList | FocusedPanel::Comments
            ) {
                app.show_file_list = true;
            }
        }
        Action::ExpandAll => {
            app.expand_all_dirs();
            app.set_message("All directories expanded");
        }
        Action::CollapseAll => {
            app.collapse_all_dirs();
            app.set_message("All directories collapsed");
        }
        Action::ToggleHelp => app.toggle_help(),
        Action::EnterCommandMode => app.enter_command_mode(),
        Action::EnterSearchMode => app.enter_search_mode(),
        Action::AddLineComment => {
            let line = app.get_line_at_cursor();
            if line.is_some() {
                app.enter_comment_mode(false, line);
            } else {
                app.set_message("Move cursor to a diff line to add a line comment");
            }
        }
        Action::AddFileComment => app.enter_comment_mode(true, None),
        // `i` edits the comment at cursor. In vim mode the text cursor starts at
        // the beginning; otherwise (and for `A`) it starts at the end.
        Action::EditComment => edit_comment_at_cursor(app, !app.comment_vim_enabled),
        // `A` (vim only) edits with the text cursor at end-of-line.
        Action::EditCommentAtEnd if app.comment_vim_enabled => edit_comment_at_cursor(app, true),
        Action::ExportToClipboard => handle_export(app),
        Action::CopyCommentAtCursor => handle_copy_comment_at_cursor(app),
        // n/N: the diff pane steps changes unless a search is highlighted
        // (see `handle_diff_action`); every other pane keeps them for search.
        Action::NextChange => {
            app.search_next_in_diff();
        }
        Action::PrevChange => {
            app.search_prev_in_diff();
        }
        Action::ClearSearchHighlight => app.clear_search_highlight(),
        Action::EnterVisualMode => {
            if app.get_line_at_cursor().is_some() {
                app.enter_visual_mode_at_cursor();
            } else {
                app.set_message("Move cursor to a diff line to start visual selection");
            }
        }
        // `(` moves toward the top row, `)` toward the bottom row. The cycle
        // methods walk data indices (newest-first), so ascending display order
        // swaps which one runs to keep the on-screen direction stable.
        Action::CycleCommitNext if app.has_review_commits() => {
            if app.commits_ascending() {
                app.cycle_commit_prev();
            } else {
                app.cycle_commit_next();
            }
            if let Err(e) = app.reload_inline_selection_for_source() {
                app.set_error(format!("Failed to load diff: {e}"));
            }
        }
        Action::CycleCommitPrev if app.has_review_commits() => {
            if app.commits_ascending() {
                app.cycle_commit_next();
            } else {
                app.cycle_commit_prev();
            }
            if let Err(e) = app.reload_inline_selection_for_source() {
                app.set_error(format!("Failed to load diff: {e}"));
            }
        }
        Action::StageFile => app.stage_file_at_cursor(),
        Action::UnstageFile => app.unstage_file_at_cursor(),
        Action::StageHunk => app.stage_hunk_at_cursor(),
        Action::UnstageHunk => app.unstage_hunk_at_cursor(),
        Action::DiscardHunk => app.request_discard_hunk(),
        Action::EditFile => app.queue_editor_for_focused_item(),
        _ => {}
    }
}

/// Handle actions in the submit resolver modal: pick what to do with each
/// comment that did not map to an inline GitHub review comment.
pub fn handle_submit_resolver_action(app: &mut App, action: Action) {
    match action {
        Action::SubmitResolverDown => app.submit_resolver_cursor_down(),
        Action::SubmitResolverUp => app.submit_resolver_cursor_up(),
        Action::SubmitResolverToggle => app.submit_resolver_toggle(),
        Action::SubmitResolverAdvance => app.submit_resolver_advance(),
        Action::ExitMode => app.cancel_submit(),
        Action::Quit => app.should_quit = true,
        _ => {}
    }
}

/// Handle actions in the bare-`:submit` action picker. Up/down move the
/// cursor through Comment/Approve/Request changes/Draft; Enter dispatches
/// preflight with the picked event (skipping the confirmation modal); Esc
/// cancels back to normal.
pub fn handle_submit_action_picker_action(app: &mut App, action: Action) {
    match action {
        Action::SubmitPickerDown => app.submit_picker_cursor_down(),
        Action::SubmitPickerUp => app.submit_picker_cursor_up(),
        Action::SubmitPickerConfirm => app.submit_picker_confirm(),
        Action::ExitMode => app.cancel_submit_action_picker(),
        Action::Quit => app.should_quit = true,
        Action::QuitHint => app.set_message(QUIT_HINT_MESSAGE),
        _ => {}
    }
}

/// Handle actions in the final submit confirmation modal.
pub fn handle_submit_confirm_action(app: &mut App, action: Action) {
    match action {
        Action::ConfirmYes => app.confirm_submit(),
        Action::ConfirmNo => app.cancel_submit(),
        // Only meaningful when the stale-head warning is visible.
        Action::SubmitReloadPr
            if app.submit_head_is_stale()
                && matches!(app.diff_source, app::DiffSource::PullRequest(_)) =>
        {
            app.cancel_submit();
            if let Err(e) = app.spawn_pr_reload() {
                app.set_error(format!("Reload failed: {e}"));
            }
        }
        Action::Quit => app.should_quit = true,
        _ => {}
    }
}

#[cfg(test)]
mod command_tests {
    use super::{CommandKind, GitOpCommand, command_spec_for, parse_git_op_command};

    #[test]
    fn parses_branch_tag_merge_and_rebase_commands() {
        assert_eq!(
            parse_git_op_command("merge feature/x"),
            Some(GitOpCommand::Merge("feature/x".into()))
        );
        assert_eq!(parse_git_op_command("merge"), None);
        assert_eq!(
            parse_git_op_command("rebase main"),
            Some(GitOpCommand::Rebase("main".into()))
        );
        assert_eq!(
            parse_git_op_command("branch -D old"),
            Some(GitOpCommand::DeleteBranch("old".into()))
        );
        assert_eq!(
            parse_git_op_command("branch -m new-name"),
            Some(GitOpCommand::RenameBranch(None, "new-name".into()))
        );
        assert_eq!(
            parse_git_op_command("branch -m a b"),
            Some(GitOpCommand::RenameBranch(Some("a".into()), "b".into()))
        );
        assert_eq!(
            parse_git_op_command("tag -d v1"),
            Some(GitOpCommand::DeleteTag("v1".into()))
        );
        // Creating a branch or tag is not a git-op command.
        assert_eq!(parse_git_op_command("branch topic"), None);
        assert_eq!(parse_git_op_command("tag v2"), None);
    }

    #[test]
    fn parses_relative_line_number_commands() {
        assert_eq!(
            command_spec_for("set relativenumber").map(|spec| spec.kind),
            Some(CommandKind::SetRelativeLineNumbers(true))
        );
        assert_eq!(
            command_spec_for("set norelativenumber").map(|spec| spec.kind),
            Some(CommandKind::SetRelativeLineNumbers(false))
        );
        assert_eq!(
            command_spec_for("set relativenumber!").map(|spec| spec.kind),
            Some(CommandKind::ToggleRelativeLineNumbers)
        );
    }

    #[test]
    fn parses_copy_url_command() {
        assert_eq!(
            command_spec_for("copy-url").map(|spec| spec.kind),
            Some(CommandKind::CopyUrl)
        );
    }

    #[test]
    fn parses_every_reviewed_visibility_command_form() {
        // Mirrors `:set commits` / `:set nocommits` / `:set commits!`, plus a
        // bare toggle alias in the shape of `:wrap`.
        assert_eq!(
            command_spec_for("set reviewed").map(|spec| spec.kind),
            Some(CommandKind::SetShowReviewed(true))
        );
        assert_eq!(
            command_spec_for("set noreviewed").map(|spec| spec.kind),
            Some(CommandKind::SetShowReviewed(false))
        );
        assert_eq!(
            command_spec_for("set reviewed!").map(|spec| spec.kind),
            Some(CommandKind::ToggleShowReviewed)
        );
        assert_eq!(
            command_spec_for("reviewed").map(|spec| spec.kind),
            Some(CommandKind::ToggleShowReviewed)
        );
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn branch_matches_rank_prefixes_before_substrings_in_list_order() {
        let names: Vec<String> = [
            "main",
            "master",
            "dev",
            "origin/main",
            "origin/master",
            "feature/mail",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            super::rank_branch_matches(&names, "ma"),
            [
                "main",
                "master",
                "origin/main",
                "origin/master",
                "feature/mail"
            ]
        );
        assert_eq!(super::rank_branch_matches(&names, "MAIL"), ["feature/mail"]);
        assert!(super::rank_branch_matches(&names, "zzz").is_empty());
    }
}
