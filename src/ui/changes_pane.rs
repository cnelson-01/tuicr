//! The Changes pane, an IDE's Commit tool window drawn in the file tree's
//! column: every local change in one tree (merge conflicts, staged,
//! unstaged, unversioned), each entry with a checkbox and a status glyph,
//! submodules as expandable entries, and the diff pane previewing the row
//! under the cursor.
//!
//! Rows come from `App::changes_rows()` (built by `build_changes_rows`), so
//! the renderer, the key handlers and the mouse hit-test read one list. The
//! columns the hit-test asks about (`ChangesRow::expander_cell`,
//! `checkbox_cell`, `glyph_cell`) are the columns this file draws into:
//!
//! ```text
//! Group        "  "*depth  ▼|▶  ' '  "{label} ({count})"
//! Directory    "  "*depth  ▼|▶  ' '  "{label}/"
//! Entry        "  "*depth  [▶|▼|' ']  ' '  [▣|▢|' ']  ' '  glyph  ' '  name  ["  " dim dir]  ["  " dim detail]
//! Note         "  "*depth  "  " text
//! Uninit.      "  "*depth  "  - {path}  (not initialized)"
//! ```

use std::path::Path;

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem},
};
use unicode_width::UnicodeWidthStr;

use crate::app::changes::CHANGES_INDENT;
use crate::app::{App, ChangesRow};
use crate::theme::Theme;
use crate::ui::diff_view::{apply_horizontal_scroll, truncate_path_smart};
use crate::ui::styles;
use crate::vcs::{ChangeDiffKey, ChangeEntry, ChangeEntryKind, ChangeGroup, ChangeStatus};

const EXPANDED_GLYPH: &str = "\u{25bc}"; // ▼
const COLLAPSED_GLYPH: &str = "\u{25b6}"; // ▶
const STAGED_BOX: &str = "\u{25a3}"; // ▣
const UNSTAGED_BOX: &str = "\u{25a2}"; // ▢
/// Cells between the name and the dim directory or detail that follows it.
const GAP: &str = "  ";
/// The dim directory is truncated to the cells left after the name and
/// dropped altogether when fewer than this many remain.
const MIN_DIR_CELLS: usize = 6;

/// Draws the Changes view into the file tree's column. Records the inner
/// rectangle for the mouse (`App::file_list_inner_area`, shared with the
/// tree) and the view's viewport fields, clamps `scroll_x`, and renders the
/// list over the view's own `ListState`.
pub(super) fn render_changes_pane(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.changes_focused();
    let block = Block::default()
        .title(pane_title(app, area.width))
        .borders(Borders::ALL)
        .style(styles::panel_style(&app.theme))
        .border_style(styles::border_style(&app.theme, focused));
    let inner = block.inner(area);
    app.file_list_inner_area = Some(inner);

    let grouped = app
        .changes
        .as_ref()
        .is_some_and(|view| view.group_by_directory);
    let rows = app.changes_rows();
    let lines: Vec<Line<'static>> = if rows.is_empty() {
        vec![Line::from(Span::styled(
            "  (no local changes)",
            styles::dim_style(&app.theme),
        ))]
    } else {
        rows.iter()
            .map(|row| row_line(&app.theme, row, grouped, inner.width as usize))
            .collect()
    };
    let max_content_width = lines.iter().map(Line::width).max().unwrap_or(0);

    let theme = &app.theme;
    let Some(view) = app.changes.as_mut() else {
        frame.render_widget(block, area);
        return;
    };
    view.viewport_height = inner.height as usize;
    view.viewport_width = inner.width as usize;
    view.max_content_width = max_content_width;
    let max_scroll_x = max_content_width.saturating_sub(inner.width as usize);
    view.scroll_x = view.scroll_x.min(max_scroll_x);
    let scroll_x = view.scroll_x;

    let items: Vec<ListItem> = lines
        .into_iter()
        .map(|line| ListItem::new(apply_horizontal_scroll(line, scroll_x)))
        .collect();
    let list = List::new(items)
        .style(styles::panel_style(theme))
        .highlight_style(styles::selected_style(theme))
        .block(block);
    frame.render_stateful_widget(list, area, &mut view.list);
}

/// ` Changes · {n} staged · {m} unstaged ` when the border has room for it,
/// else ` Changes · {n} · {m} `, else ` Changes `; ` Changes · clean ` for an
/// empty set.
fn pane_title(app: &App, width: u16) -> String {
    let Some(set) = app.changes.as_ref().map(|view| &view.set) else {
        return " Changes ".to_string();
    };
    if set.is_empty() {
        return " Changes \u{00b7} clean ".to_string();
    }
    let (staged, unstaged) = (set.staged.len(), set.unstaged.len());
    let room = width.saturating_sub(2) as usize;
    let long = format!(" Changes \u{00b7} {staged} staged \u{00b7} {unstaged} unstaged ");
    if long.width() <= room {
        return long;
    }
    let short = format!(" Changes \u{00b7} {staged} \u{00b7} {unstaged} ");
    if short.width() <= room {
        return short;
    }
    " Changes ".to_string()
}

fn expander_glyph(expanded: bool) -> &'static str {
    if expanded {
        EXPANDED_GLYPH
    } else {
        COLLAPSED_GLYPH
    }
}

/// One row, before horizontal panning. `width` is the pane's inner width,
/// which decides how much of the dim directory survives on an entry row.
fn row_line(theme: &Theme, row: &ChangesRow, grouped: bool, width: usize) -> Line<'static> {
    let indent = " ".repeat(row.depth() * CHANGES_INDENT);
    let dim = styles::dim_style(theme);
    match row {
        ChangesRow::Group {
            group,
            count,
            expanded,
            ..
        } => {
            let label_style = if *group == ChangeGroup::Conflicted {
                styles::change_status_style(theme, ChangeStatus::Conflicted)
            } else {
                Style::default().fg(theme.fg_primary)
            }
            .add_modifier(Modifier::BOLD);
            Line::from(vec![
                Span::raw(indent),
                Span::styled(expander_glyph(*expanded), styles::dir_icon_style(theme)),
                Span::raw(" "),
                Span::styled(format!("{} ({count})", group.label()), label_style),
            ])
        }
        ChangesRow::Directory {
            label, expanded, ..
        } => Line::from(vec![
            Span::raw(indent),
            Span::styled(expander_glyph(*expanded), styles::dir_icon_style(theme)),
            Span::raw(" "),
            Span::styled(format!("{label}/"), Style::default().fg(theme.fg_secondary)),
        ]),
        ChangesRow::Entry {
            key,
            entry,
            expanded,
            ..
        } => entry_line(theme, indent, key, entry, *expanded, grouped, width),
        ChangesRow::Note { text, .. } => Line::from(vec![
            Span::raw(indent),
            Span::styled(format!("{GAP}{text}"), dim),
        ]),
        ChangesRow::Uninitialized { path, .. } => {
            let name = format!("{GAP}- {}", path.display());
            let used = indent.width() + name.width();
            let mut spans = vec![Span::raw(indent), Span::styled(name, dim)];
            if let Some(shown) = fit_detail("(not initialized)", width.saturating_sub(used)) {
                spans.push(Span::styled(format!("{GAP}{shown}"), dim));
            }
            Line::from(spans)
        }
    }
}

fn entry_line(
    theme: &Theme,
    indent: String,
    key: &ChangeDiffKey,
    entry: &ChangeEntry,
    expanded: Option<bool>,
    grouped: bool,
    width: usize,
) -> Line<'static> {
    let dim = styles::dim_style(theme);
    let expander = match expanded {
        Some(expanded) => expander_glyph(expanded),
        None => " ",
    };
    let (checkbox, checkbox_style) = match key.group {
        ChangeGroup::Staged => (STAGED_BOX, Style::default().fg(theme.file_added)),
        ChangeGroup::Unstaged | ChangeGroup::Untracked => (UNSTAGED_BOX, dim),
        ChangeGroup::Conflicted => (" ", Style::default()),
    };
    let (glyph, glyph_style) = if entry.is_submodule() {
        ('S', styles::submodule_glyph_style(theme))
    } else {
        (
            entry.status.glyph(),
            styles::change_status_style(theme, entry.status),
        )
    };
    let name = entry.display_name();
    let name_style = if entry.status == ChangeStatus::Conflicted {
        glyph_style.add_modifier(Modifier::BOLD)
    } else {
        glyph_style
    };
    // indent, expander, space, checkbox, space, glyph, space, name
    let used = indent.width() + 6 + name.width();
    let mut spans = vec![
        Span::raw(indent),
        Span::styled(expander, styles::dir_icon_style(theme)),
        Span::raw(" "),
        Span::styled(checkbox, checkbox_style),
        Span::raw(" "),
        Span::styled(glyph.to_string(), glyph_style),
        Span::raw(" "),
        Span::styled(name, name_style),
    ];
    let mut used = used;
    if !grouped && let Some(dir) = entry_directory(key, entry) {
        let remaining = width.saturating_sub(used);
        if remaining >= MIN_DIR_CELLS {
            let shown = truncate_path_smart(&dir, remaining - GAP.width());
            used += GAP.width() + shown.width();
            spans.push(Span::styled(format!("{GAP}{shown}"), dim));
        }
    }
    if let Some(detail) = entry_detail(entry)
        && let Some(shown) = fit_detail(&detail, width.saturating_sub(used))
    {
        spans.push(Span::styled(format!("{GAP}{shown}"), dim));
    }
    Line::from(spans)
}

/// The dim detail cut to the cells left on the row: whole when it fits,
/// otherwise shortened with an ellipsis so a clipped `modified content`
/// never reads as `modifi`, and dropped when fewer than [`MIN_DIR_CELLS`]
/// remain.
fn fit_detail(detail: &str, remaining: usize) -> Option<String> {
    let room = remaining.checked_sub(GAP.width())?;
    if room < MIN_DIR_CELLS {
        return None;
    }
    if detail.width() <= room {
        return Some(detail.to_string());
    }
    let mut shown = String::new();
    for ch in detail.chars() {
        if shown.width() + ch.to_string().width() + 1 > room {
            break;
        }
        shown.push(ch);
    }
    let shown = shown.trim_end().to_string();
    Some(format!("{shown}\u{2026}"))
}

/// The dim directory shown after a file's name in flat mode: the parent of
/// the entry's path, prefixed by the submodule it lives in. Submodules and
/// embedded repositories show their whole path as the name, so they have
/// none.
fn entry_directory(key: &ChangeDiffKey, entry: &ChangeEntry) -> Option<String> {
    if !matches!(entry.kind, ChangeEntryKind::File) {
        return None;
    }
    let parent = entry.path.parent().unwrap_or(Path::new(""));
    let dir = match &key.submodule {
        Some(sub) if parent == Path::new("") => sub.clone(),
        Some(sub) => sub.join(parent),
        None => parent.to_path_buf(),
    };
    let dir = dir.to_string_lossy().into_owned();
    (!dir.is_empty()).then_some(dir)
}

/// The dim detail after the name: a submodule's state (`+3 commits`,
/// `modified content`, ...), `(embedded repo)`, or the source of a rename
/// or copy (`← old.txt`).
fn entry_detail(entry: &ChangeEntry) -> Option<String> {
    match &entry.kind {
        ChangeEntryKind::Submodule(change) => Some(change.detail()).filter(|d| !d.is_empty()),
        ChangeEntryKind::EmbeddedRepo => Some("(embedded repo)".to_string()),
        ChangeEntryKind::File => entry
            .old_path
            .as_ref()
            .filter(|_| matches!(entry.status, ChangeStatus::Renamed | ChangeStatus::Copied))
            .map(|old| format!("\u{2190} {}", old.display())),
    }
}

#[cfg(test)]
mod tests {
    //! Render checks for the Changes pane through the real `ui::render`.
    //! The app is built directly with a sample change set (no VCS call is
    //! made), so these exercise the renderer alone.
    use std::path::PathBuf;
    use std::time::Instant;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    use crate::app::{
        App, ChangesView, ConfirmAction, DiffSource, DiscardTarget, FocusedPanel, InputMode,
        SubmoduleExpansion,
    };
    use crate::model::{DiffFile, DiffLine, FileStatus, ReviewSession, SessionDiffSource};
    use crate::vcs::traits::{VcsBackend, VcsInfo, VcsType};
    use crate::vcs::{
        ChangeDiffKey, ChangeEntry, ChangeEntryKind, ChangeGroup, ChangeSet, ChangeStatus,
        SubmoduleChange,
    };

    struct StubVcs(VcsInfo);
    impl VcsBackend for StubVcs {
        fn info(&self) -> &VcsInfo {
            &self.0
        }
        fn get_working_tree_diff(
            &self,
            _hl: &crate::syntax::SyntaxHighlighter,
        ) -> crate::error::Result<Vec<DiffFile>> {
            Ok(Vec::new())
        }
        fn fetch_context_lines(
            &self,
            _path: &std::path::Path,
            _status: FileStatus,
            _ref_commit: Option<&str>,
            _start: u32,
            _end: u32,
        ) -> crate::error::Result<Vec<DiffLine>> {
            Ok(Vec::new())
        }
        fn file_line_count(
            &self,
            _path: &std::path::Path,
            _status: FileStatus,
            _ref_commit: Option<&str>,
        ) -> crate::error::Result<u32> {
            Ok(0)
        }
    }

    fn submodule(path: &str, status: ChangeStatus, change: SubmoduleChange) -> ChangeEntry {
        let mut entry = ChangeEntry::file(path, status);
        entry.kind = ChangeEntryKind::Submodule(change);
        entry
    }

    /// The tree of the design mockup: a conflict, three staged entries (one
    /// a submodule ahead by three commits), three unstaged (one a dirty
    /// submodule), an untracked file, an embedded repo, and a submodule
    /// that is not checked out.
    fn sample_set() -> ChangeSet {
        let mut set = ChangeSet::default();
        set.push(
            ChangeGroup::Conflicted,
            ChangeEntry::file("README.md", ChangeStatus::Conflicted),
        );
        set.push(
            ChangeGroup::Staged,
            ChangeEntry::file("src/app/changes.rs", ChangeStatus::Added),
        );
        set.push(
            ChangeGroup::Staged,
            ChangeEntry::file("src/app/mod.rs", ChangeStatus::Modified),
        );
        set.push(
            ChangeGroup::Staged,
            submodule(
                "lib",
                ChangeStatus::Modified,
                SubmoduleChange {
                    head_id: Some("806ed11f3c2a9d1e0b7c4a6f8e2d1c0b9a8f7e6d".into()),
                    index_id: Some("40c7ed9a1b2c3d4e5f60718293a4b5c6d7e8f9a0".into()),
                    workdir_id: Some("40c7ed9a1b2c3d4e5f60718293a4b5c6d7e8f9a0".into()),
                    ahead: 3,
                    registered: true,
                    ..Default::default()
                },
            ),
        );
        set.push(
            ChangeGroup::Unstaged,
            ChangeEntry::file("src/app/tree.rs", ChangeStatus::Modified),
        );
        set.push(
            ChangeGroup::Unstaged,
            ChangeEntry::file("src/app/mod.rs", ChangeStatus::Modified),
        );
        set.push(
            ChangeGroup::Unstaged,
            submodule(
                "vendor/x",
                ChangeStatus::Modified,
                SubmoduleChange {
                    index_id: Some("1111111111111111111111111111111111111111".into()),
                    workdir_id: Some("1111111111111111111111111111111111111111".into()),
                    modified_content: true,
                    registered: true,
                    ..Default::default()
                },
            ),
        );
        set.push(
            ChangeGroup::Untracked,
            ChangeEntry::file("docs/notes.md", ChangeStatus::Untracked),
        );
        let mut nested = ChangeEntry::file("nested", ChangeStatus::Untracked);
        nested.kind = ChangeEntryKind::EmbeddedRepo;
        set.push(ChangeGroup::Untracked, nested);
        set.uninitialized_submodules.push(PathBuf::from("tools"));
        set.sort();
        set
    }

    fn app_with_set(set: ChangeSet) -> App {
        let vcs_info = VcsInfo {
            root_path: PathBuf::from("/tmp"),
            head_commit: "abcdef0123456789".into(),
            branch_name: Some("main".into()),
            vcs_type: VcsType::Git,
        };
        let session = ReviewSession::new(
            vcs_info.root_path.clone(),
            vcs_info.head_commit.clone(),
            vcs_info.branch_name.clone(),
            SessionDiffSource::StagedAndUnstaged,
        );
        let mut app = App::build(
            Box::new(StubVcs(vcs_info.clone())),
            vcs_info,
            crate::theme::Theme::dark(),
            None,
            false,
            Vec::new(),
            session,
            DiffSource::Changes(None),
            InputMode::Normal,
            Vec::new(),
            None,
            None,
        )
        .expect("build app");
        let mut view = ChangesView {
            set,
            ..Default::default()
        };
        view.rebuild_rows();
        app.changes = Some(view);
        app.show_file_list = true;
        app.focused_panel = FocusedPanel::FileList;
        app
    }

    fn sample_app() -> App {
        app_with_set(sample_set())
    }

    fn draw(app: &mut App, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, app))
            .expect("draw frame");
        terminal.backend().buffer().clone()
    }

    fn buffer_text(buffer: &Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn row_text(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    }

    /// The rendered text of the pane row that shows `needle`.
    fn pane_row_containing(text: &str, needle: &str) -> String {
        text.lines()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no row containing {needle:?} in:\n{text}"))
            .to_string()
    }

    fn row_index(app: &App, name: &str) -> usize {
        app.changes_rows()
            .iter()
            .position(|row| {
                row.entry_key()
                    .is_some_and(|key| key.path.to_string_lossy().ends_with(name))
            })
            .unwrap_or_else(|| panic!("no entry row for {name}"))
    }

    #[test]
    fn renders_group_headers_with_counts_and_checkboxes_at_120x24() {
        let mut app = sample_app();
        let text = buffer_text(&draw(&mut app, 120, 24));
        for expected in [
            "\u{25bc} Merge conflicts (1)",
            "\u{25bc} Staged (3)",
            "    \u{25a3} A changes.rs",
            "    \u{25a3} M mod.rs",
            // Detail text is cut to the row with an ellipsis, never mid-word
            // at the pane edge.
            "    \u{25a3} S lib  +3 commi\u{2026}",
            "\u{25bc} Unstaged (3)",
            "    \u{25a2} M tree.rs",
            "  \u{25b6} \u{25a2} S vendor/x",
            "\u{25bc} Unversioned files (2",
            "    \u{25a2} ? notes.md",
            "? nested  (embe\u{2026}",
            "- tools  (not initi\u{2026}",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
        assert!(!text.contains("+3 commit"), "clipped mid-word:\n{text}");
    }

    #[test]
    fn checkbox_and_expander_cells_match_the_rendered_columns() {
        let mut app = sample_app();
        let buffer = draw(&mut app, 120, 24);
        let inner = app.file_list_inner_area.expect("pane inner area recorded");

        let staged = row_index(&app, "changes.rs");
        let row = &app.changes_rows()[staged];
        let cell = row.checkbox_cell().expect("staged entries have a checkbox");
        let y = inner.y + staged as u16;
        assert_eq!(buffer[(inner.x + cell as u16, y)].symbol(), "\u{25a3}");
        assert_eq!(buffer[(inner.x + row.glyph_cell() as u16, y)].symbol(), "A");

        let dirty = row_index(&app, "vendor/x");
        let row = &app.changes_rows()[dirty];
        let cell = row
            .expander_cell()
            .expect("dirty submodules have an expander");
        let y = inner.y + dirty as u16;
        assert_eq!(buffer[(inner.x + cell as u16, y)].symbol(), "\u{25b6}");
        assert_eq!(
            buffer[(inner.x + row.checkbox_cell().unwrap() as u16, y)].symbol(),
            "\u{25a2}"
        );
    }

    #[test]
    fn flat_rows_show_the_dim_directory_at_160x45() {
        let mut app = sample_app();
        app.file_list_pct = 25;
        let buffer = draw(&mut app, 160, 45);
        let text = buffer_text(&buffer);
        let row = pane_row_containing(&text, "M tree.rs");
        assert!(row.contains("M tree.rs  src/app"), "{row}");
        let row = pane_row_containing(&text, "? notes.md");
        assert!(row.contains("? notes.md  docs"), "{row}");
        // The directory is dim, unlike the name.
        let inner = app.file_list_inner_area.unwrap();
        let idx = row_index(&app, "tree.rs");
        let y = inner.y + idx as u16;
        let line = row_text(&buffer, y);
        let dir_col = line.find("src/app").unwrap() as u16;
        assert_eq!(buffer[(dir_col, y)].fg, app.theme.fg_dim);
    }

    #[test]
    fn flat_rows_truncate_the_directory_before_dropping_it() {
        let mut app = sample_app();
        // 24 columns for the pane, 22 inside.
        let text = buffer_text(&draw(&mut app, 120, 24));
        assert_eq!(app.file_list_inner_area.unwrap().width, 22);
        // `    ▢ M tree.rs` uses 15 cells: 7 remain, so the directory is
        // truncated to the 5 after the gap.
        let row = pane_row_containing(&text, "M tree.rs");
        assert!(row.contains("M tree.rs  \u{2026}/app"), "{row}");
        // `    ▣ M mod.rs` leaves 8: still `…/app`.
        let row = pane_row_containing(&text, "\u{25a3} M mod.rs");
        assert!(row.contains("M mod.rs  \u{2026}/app"), "{row}");
        // `    ▣ A changes.rs` leaves 4, fewer than the minimum: dropped.
        let row = pane_row_containing(&text, "A changes.rs");
        assert!(!row.contains("\u{2026}") && !row.contains("app"), "{row}");
    }

    #[test]
    fn directory_grouping_renders_folder_rows() {
        let mut app = sample_app();
        app.changes.as_mut().unwrap().group_by_directory = true;
        app.changes.as_mut().unwrap().rebuild_rows();
        let text = buffer_text(&draw(&mut app, 160, 45));
        let src = pane_row_containing(&text, "\u{25bc} src/");
        let app_dir = pane_row_containing(&text, "\u{25bc} app/");
        let tree = pane_row_containing(&text, "M tree.rs");
        let indent = |row: &str| row.find(|c: char| c != '\u{2502}' && c != ' ').unwrap();
        assert!(indent(&src) < indent(&app_dir), "{src}\n{app_dir}");
        assert!(indent(&app_dir) < indent(&tree), "{app_dir}\n{tree}");
        assert!(!tree.contains("src/app"), "{tree}");
    }

    #[test]
    fn dirty_submodule_expansion_renders_nested_groups() {
        let mut app = sample_app();
        let mut inner = ChangeSet::default();
        inner.push(
            ChangeGroup::Unstaged,
            ChangeEntry::file("a.txt", ChangeStatus::Modified),
        );
        let view = app.changes.as_mut().unwrap();
        view.submodules
            .insert(PathBuf::from("vendor/x"), SubmoduleExpansion::Loaded(inner));
        view.rebuild_rows();
        let text = buffer_text(&draw(&mut app, 160, 45));
        let lines: Vec<&str> = text.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.contains("  \u{25bc} \u{25a2} S vendor/x"))
            .unwrap_or_else(|| panic!("no expanded submodule row in:\n{text}"));
        assert!(
            lines[at + 1].contains("    \u{25bc} Unstaged (1)"),
            "{}",
            lines[at + 1]
        );
        assert!(
            lines[at + 2].contains("\u{25a2} M a.txt"),
            "{}",
            lines[at + 2]
        );
        // Inside a submodule the dim directory names the submodule.
        assert!(
            lines[at + 2].contains("a.txt  vendor/x"),
            "{}",
            lines[at + 2]
        );
    }

    #[test]
    fn failed_submodule_expansion_renders_a_note() {
        let mut app = sample_app();
        let view = app.changes.as_mut().unwrap();
        view.submodules.insert(
            PathBuf::from("vendor/x"),
            SubmoduleExpansion::Failed("not a git repository".into()),
        );
        view.rebuild_rows();
        // Wide enough for the whole note; the default 20% clips it.
        app.file_list_pct = 40;
        let text = buffer_text(&draw(&mut app, 160, 45));
        let note = pane_row_containing(&text, "could not read submodule");
        assert!(note.contains("not a git repository"), "{note}");
        // The note sits right under the submodule it belongs to.
        let lines: Vec<&str> = text.lines().collect();
        let at = lines.iter().position(|l| l.contains("S vendor/x")).unwrap();
        assert!(
            lines[at + 1].contains("could not read submodule"),
            "{}",
            lines[at + 1]
        );
    }

    #[test]
    fn probe_error_renders_a_note_under_the_entry() {
        let mut set = ChangeSet::default();
        set.push(
            ChangeGroup::Unstaged,
            submodule(
                "lib",
                ChangeStatus::Modified,
                SubmoduleChange {
                    probe_error: Some("gitlink missing".into()),
                    registered: true,
                    ..Default::default()
                },
            ),
        );
        let mut app = app_with_set(set);
        app.file_list_pct = 40;
        let buffer = draw(&mut app, 160, 45);
        let text = buffer_text(&buffer);
        let lines: Vec<&str> = text.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.contains("S lib"))
            .unwrap_or_else(|| panic!("no submodule row in:\n{text}"));
        assert!(
            lines[at + 1].contains("could not read submodule: gitlink missing"),
            "{}",
            lines[at + 1]
        );
        // The note is dim and not selectable.
        let inner = app.file_list_inner_area.unwrap();
        let y = inner.y + 2;
        let col = row_text(&buffer, y).find("could").unwrap() as u16;
        assert_eq!(buffer[(col, y)].fg, app.theme.fg_dim);
        assert!(!app.changes_rows()[2].selectable());
    }

    #[test]
    fn conflicted_group_comes_first_and_uses_the_conflict_color() {
        let mut app = sample_app();
        let buffer = draw(&mut app, 120, 24);
        let inner = app.file_list_inner_area.unwrap();
        assert!(row_text(&buffer, inner.y).contains("Merge conflicts (1)"));
        let idx = row_index(&app, "README.md");
        let row = &app.changes_rows()[idx];
        assert_eq!(row.checkbox_cell(), None);
        let glyph = buffer[(inner.x + row.glyph_cell() as u16, inner.y + idx as u16)].clone();
        assert_eq!(glyph.symbol(), "!");
        assert_eq!(glyph.fg, app.theme.diff_del);
    }

    #[test]
    fn selected_row_uses_the_selected_style() {
        let mut app = sample_app();
        app.changes.as_mut().unwrap().select(2);
        let buffer = draw(&mut app, 120, 24);
        let inner = app.file_list_inner_area.unwrap();
        assert!(row_text(&buffer, inner.y + 2).contains("Staged (3)"));
        assert_eq!(buffer[(inner.x, inner.y + 2)].bg, app.theme.bg_highlight);
        assert_ne!(buffer[(inner.x, inner.y + 1)].bg, app.theme.bg_highlight);
    }

    #[test]
    fn horizontal_scroll_shifts_rows() {
        let mut app = sample_app();
        app.changes.as_mut().unwrap().scroll_x = 2;
        let buffer = draw(&mut app, 120, 24);
        let inner = app.file_list_inner_area.unwrap();
        let row = row_text(&buffer, inner.y + 2);
        // Entry details are cut to the pane, so only the 23-cell
        // "Unversioned files (2)" header overflows the 22-cell viewport:
        // the pan is clamped to one cell and the group rows shift by it.
        assert!(row.starts_with("\u{2502} Staged (3)"), "{row}");
        assert_eq!(app.changes.as_ref().unwrap().scroll_x, 1);
        assert_eq!(app.changes.as_ref().unwrap().viewport_width, 22);
        assert_eq!(app.changes.as_ref().unwrap().viewport_height, 20);
    }

    #[test]
    fn horizontal_scroll_is_clamped_to_the_widest_row() {
        let mut app = sample_app();
        app.changes.as_mut().unwrap().scroll_x = usize::MAX;
        draw(&mut app, 120, 24);
        let view = app.changes.as_ref().unwrap();
        assert_eq!(
            view.scroll_x,
            view.max_content_width.saturating_sub(view.viewport_width)
        );
    }

    #[test]
    fn title_counts_staged_and_unstaged_and_shrinks() {
        let mut app = sample_app();
        app.file_list_pct = 25;
        let text = buffer_text(&draw(&mut app, 160, 45));
        assert!(
            text.contains(" Changes \u{00b7} 3 staged \u{00b7} 3 unstaged "),
            "{text}"
        );
        app.file_list_pct = crate::app::DEFAULT_FILE_LIST_PCT;
        let text = buffer_text(&draw(&mut app, 120, 24));
        assert!(text.contains(" Changes \u{00b7} 3 \u{00b7} 3 "), "{text}");
        assert!(!text.contains("3 staged"), "{text}");
        // 14 columns: room for ` Changes ` but not for ` Changes · 3 · 3 `.
        app.file_list_pct = 12;
        let text = buffer_text(&draw(&mut app, 120, 24));
        assert!(text.contains("\u{250c} Changes \u{2500}"), "{text}");
        assert!(!text.contains("\u{00b7} 3"), "{text}");
    }

    #[test]
    fn empty_view_says_no_local_changes() {
        let mut app = app_with_set(ChangeSet::default());
        let text = buffer_text(&draw(&mut app, 120, 24));
        assert!(text.contains(" Changes \u{00b7} clean "), "{text}");
        assert!(text.contains("(no local changes)"), "{text}");
    }

    #[test]
    fn inactive_view_leaves_the_file_tree_in_place() {
        let mut app = sample_app();
        app.diff_source = DiffSource::WorkingTree;
        let text = buffer_text(&draw(&mut app, 120, 24));
        assert!(text.contains(" Files "), "{text}");
        assert!(!text.contains("Staged (3)"), "{text}");
    }

    #[test]
    fn discard_prompt_with_detail_renders_the_detail_lines() {
        let mut app = sample_app();
        let entry = app
            .changes
            .as_ref()
            .unwrap()
            .set
            .find(ChangeGroup::Staged, std::path::Path::new("lib"))
            .cloned()
            .unwrap();
        app.pending_discard = Some(DiscardTarget::Entry {
            key: ChangeDiffKey::new(ChangeGroup::Staged, "lib"),
            entry,
        });
        app.enter_confirm_mode(ConfirmAction::DiscardChanges);
        let text = buffer_text(&draw(&mut app, 120, 24));
        assert!(text.contains("pointer 40c7ed9 \u{2192} 806ed11"), "{text}");
        assert!(text.contains("inside lib are kept"), "{text}");
        assert!(text.contains("[Y]es") && text.contains("[N]o"), "{text}");
    }

    #[test]
    fn discard_prompt_without_detail_uses_the_plain_dialog() {
        let mut app = sample_app();
        app.pending_discard = Some(DiscardTarget::Entry {
            key: ChangeDiffKey::new(ChangeGroup::Untracked, "docs/notes.md"),
            entry: ChangeEntry::file("docs/notes.md", ChangeStatus::Untracked),
        });
        app.enter_confirm_mode(ConfirmAction::DiscardChanges);
        let text = buffer_text(&draw(&mut app, 120, 24));
        assert!(
            text.contains("Delete the untracked file docs/notes.md?"),
            "{text}"
        );
        assert!(!text.contains("pointer"), "{text}");
    }

    #[test]
    fn long_plain_discard_prompt_wraps_instead_of_clipping() {
        let mut app = sample_app();
        let mut entry = ChangeEntry::file("renamed.txt", ChangeStatus::Renamed);
        entry.old_path = Some(PathBuf::from("plain.txt"));
        app.pending_discard = Some(DiscardTarget::Entry {
            key: ChangeDiffKey::new(ChangeGroup::Staged, "renamed.txt"),
            entry,
        });
        app.enter_confirm_mode(ConfirmAction::DiscardChanges);
        let text = buffer_text(&draw(&mut app, 120, 24));
        assert!(text.contains("Undo the rename plain.txt"), "{text}");
        // The tail of the sentence wraps onto a second row instead of being
        // cut at the box's edge.
        assert!(
            text.contains("restored from HEAD. This cannot be"),
            "{text}"
        );
        assert!(text.contains("undone."), "{text}");
    }

    #[test]
    fn renamed_rows_show_their_source() {
        let mut set = ChangeSet::default();
        let mut entry = ChangeEntry::file("renamed.txt", ChangeStatus::Renamed);
        entry.old_path = Some(PathBuf::from("plain.txt"));
        set.push(ChangeGroup::Staged, entry);
        let mut app = app_with_set(set);
        app.file_list_pct = 40;
        let text = buffer_text(&draw(&mut app, 160, 24));
        assert!(text.contains("R renamed.txt  \u{2190} plain.txt"), "{text}");
        // In the default 22-cell pane the source does not fit and is
        // dropped rather than clipped mid-word.
        let text = buffer_text(&draw(&mut app_with_set_default(), 120, 24));
        assert!(text.contains("R renamed.txt   "), "{text}");
        assert!(!text.contains("\u{2190}"), "{text}");
    }

    fn app_with_set_default() -> App {
        let mut set = ChangeSet::default();
        let mut entry = ChangeEntry::file("renamed.txt", ChangeStatus::Renamed);
        entry.old_path = Some(PathBuf::from("plain.txt"));
        set.push(ChangeGroup::Staged, entry);
        app_with_set(set)
    }

    /// Median frame time for a pane of 500 untracked entries at 180x50,
    /// printed; no budget is asserted (see `render_perf_tests.rs`).
    ///
    /// `cargo test --release render_perf_changes_pane -- --ignored --nocapture`
    #[test]
    #[ignore = "timing measurement, run explicitly"]
    fn render_perf_changes_pane() {
        let mut set = ChangeSet::default();
        for i in 0..500 {
            set.push(
                ChangeGroup::Untracked,
                ChangeEntry::file(
                    format!("src/module_{}/file_{i}.rs", i % 20),
                    ChangeStatus::Untracked,
                ),
            );
        }
        set.sort();
        let mut app = app_with_set(set);
        let mut terminal = Terminal::new(TestBackend::new(180, 50)).unwrap();
        let mut samples = Vec::new();
        for _ in 0..21 {
            let start = Instant::now();
            terminal
                .draw(|frame| crate::ui::render(frame, &mut app))
                .expect("draw frame");
            samples.push(start.elapsed().as_micros());
        }
        samples.sort_unstable();
        println!(
            "500 untracked entries at 180x50: {:>8.2} ms/frame",
            samples[samples.len() / 2] as f64 / 1000.0
        );
    }
}
