//! The repo switcher modal: the repositories tuicr was opened in, most
//! recent first, filtered as the user types. Each row shows the repository's
//! name, its path with `~` for home, and how long ago it was opened. While
//! a fetch-all runs, and until the picker closes after one, each row also
//! says where that repository stands in it.

use chrono::DateTime;
use ratatui::{
    Frame,
    layout::Alignment,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, RepoFetchState, RepoRow};
use crate::theme::Theme;
use crate::ui::commit_row::format_relative_short;
use crate::ui::diff_view::truncate_path_smart;
use crate::ui::status_bar;
use crate::ui::styles;
use crate::ui::submit_modals::{centered_rect, modal_anchor};

/// Widest a repository name is drawn before it is cut with `…`.
const MAX_NAME_WIDTH: usize = 32;
const MISSING_SUFFIX: &str = " (missing)";
/// Columns a fetch state keeps when the row is too narrow for it and the
/// whole path: enough for `failed: origin: fatal: ` and a few words more.
const MIN_FETCH_STATE_WIDTH: usize = 32;
/// Columns a row's path keeps however long the fetch state is.
const MIN_PATH_WIDTH: usize = 12;

pub(super) fn render_repo_picker(frame: &mut Frame, app: &App) {
    let theme = &app.theme;
    let Some(picker) = app.repo_picker.as_ref() else {
        return;
    };
    let area = centered_rect(70, 70, modal_anchor(app, frame.area()));
    frame.render_widget(Clear, area);

    let title = match app.fetch_all_progress() {
        Some((done, total)) => format!(" Repositories \u{00b7} fetching {done}/{total} "),
        None => " Repositories ".to_string(),
    };
    let block = Block::default()
        .title(title)
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .style(styles::popup_style(theme))
        .border_style(styles::border_style(theme, true));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let width = usize::from(inner.width);
    let visible = picker.visible();
    // Rows available for repositories: the filter line and the hint line
    // sit at the bottom, with a blank line between.
    let rows = usize::from(inner.height).saturating_sub(3);
    let offset = if rows == 0 {
        0
    } else {
        picker.cursor.saturating_sub(rows - 1)
    };
    let shown: Vec<(usize, &RepoRow)> = visible
        .iter()
        .enumerate()
        .skip(offset)
        .take(rows)
        .map(|(row, &idx)| (row, &picker.rows[idx]))
        .collect();
    let name_width = shown
        .iter()
        .map(|(_, repo)| repo.name.width())
        .max()
        .unwrap_or(0)
        .min(MAX_NAME_WIDTH);
    let dim = Style::default().fg(theme.fg_dim);

    let mut lines: Vec<Line> = Vec::with_capacity(rows + 3);
    for (row, repo) in shown {
        let is_cursor = row == picker.cursor;
        let name_style = if is_cursor {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else if !repo.exists {
            dim
        } else if repo.is_current {
            styles::branch_style(theme).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let marker = if is_cursor {
            ">"
        } else if repo.is_current {
            "*"
        } else {
            " "
        };
        let name = fit_name(&repo.name, name_width);
        let age = DateTime::from_timestamp(repo.last_opened, 0)
            .map(|time| format_relative_short(&time))
            .unwrap_or_default();
        let age = format!("  {age} ");
        let left_width = 2 + name_width + 2;
        let suffix = if repo.exists { "" } else { MISSING_SUFFIX };
        // Where this repository stands in the fetch-all, between the path
        // and the age. The two share what the name and the age leave: each
        // is drawn whole when both fit, otherwise the state takes what the
        // path does not need, and never less than its own minimum while
        // the path keeps its.
        let fetch = app
            .fetch_all
            .as_ref()
            .and_then(|job| job.state_for(&repo.path))
            .map(|state| fetch_state_label(state, theme));
        let shared = width.saturating_sub(left_width + age.width() + suffix.width());
        let fetch_room = shared
            .saturating_sub(repo.display_path.width() + 2)
            .max(MIN_FETCH_STATE_WIDTH)
            .min(shared.saturating_sub(MIN_PATH_WIDTH + 2));
        let (fetch_text, fetch_style) = match fetch {
            Some((text, style)) if fetch_room > 0 => {
                (format!("  {}", fit_text(&text, fetch_room)), style)
            }
            _ => (String::new(), dim),
        };
        let path_room = shared.saturating_sub(fetch_text.width());
        let path = if path_room == 0 {
            String::new()
        } else {
            format!(
                "{}{suffix}",
                truncate_path_smart(&repo.display_path, path_room)
            )
        };
        let pad =
            width.saturating_sub(left_width + path.width() + fetch_text.width() + age.width());
        lines.push(Line::from(vec![
            Span::styled(format!("{marker} "), name_style),
            Span::styled(name, name_style),
            Span::raw("  "),
            Span::styled(path, dim),
            Span::raw(" ".repeat(pad)),
            Span::styled(fetch_text, fetch_style),
            Span::styled(age, dim),
        ]));
    }
    if picker.rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no repositories yet: \":repo <path>\" opens one",
            dim,
        )));
    } else if visible.is_empty() {
        lines.push(Line::from(Span::styled("  no repositories match", dim)));
    }
    while lines.len() < rows {
        lines.push(Line::from(""));
    }

    lines.push(Line::from(vec![
        Span::styled("  / ", Style::default().fg(theme.fg_secondary)),
        Span::styled(
            picker.filter.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled("\u{2588}", Style::default().fg(theme.fg_secondary)),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        status_bar::fit_hints(&status_bar::mode_hints(app), width),
        Style::default().fg(theme.fg_secondary),
    )));

    frame.render_widget(
        Paragraph::new(lines).style(styles::popup_style(theme)),
        inner,
    );
}

/// What a row says about its repository's fetch, and in which colour.
fn fetch_state_label(state: &RepoFetchState, theme: &Theme) -> (String, Style) {
    let dim = Style::default().fg(theme.fg_dim);
    match state {
        RepoFetchState::Queued => ("queued".to_string(), dim),
        RepoFetchState::Fetching => (
            "fetching\u{2026}".to_string(),
            Style::default().fg(theme.fg_secondary),
        ),
        RepoFetchState::Updated => ("updated".to_string(), Style::default().fg(theme.file_added)),
        RepoFetchState::UpToDate => ("up to date".to_string(), dim),
        RepoFetchState::Skipped(reason) => (format!("skipped: {reason}"), dim),
        RepoFetchState::Failed(reason) => (
            format!("failed: {reason}"),
            Style::default().fg(theme.file_deleted),
        ),
    }
}

/// `name` in exactly `width` columns: padded, or cut with `…`.
fn fit_name(name: &str, width: usize) -> String {
    let cut = fit_text(name, width);
    format!("{cut}{}", " ".repeat(width.saturating_sub(cut.width())))
}

/// `text` in at most `width` columns, cut with `…` when it is longer.
fn fit_text(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    let mut cut = String::new();
    for c in text.chars() {
        let next = format!("{cut}{c}");
        if next.width() + 1 > width {
            break;
        }
        cut = next;
    }
    cut.push('\u{2026}');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{DiffSource, FetchAllJob, FetchAllRow, InputMode, RepoPicker};
    use crate::model::{DiffFile, DiffLine, FileStatus, ReviewSession, SessionDiffSource};
    use crate::vcs::traits::{VcsBackend, VcsInfo, VcsType};
    use chrono::Utc;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;

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

    fn row(name: &str, display_path: &str, opened_ago_secs: i64) -> RepoRow {
        RepoRow {
            path: PathBuf::from(format!("/abs/{name}")),
            name: name.to_string(),
            display_path: display_path.to_string(),
            last_opened: Utc::now().timestamp() - opened_ago_secs,
            exists: true,
            is_current: false,
        }
    }

    fn app_with_picker(rows: Vec<RepoRow>, cursor: usize, filter: &str) -> App {
        let vcs_info = VcsInfo {
            root_path: PathBuf::from("/tmp"),
            head_commit: "head".into(),
            branch_name: Some("main".into()),
            vcs_type: VcsType::Git,
        };
        let session = ReviewSession::new(
            vcs_info.root_path.clone(),
            vcs_info.head_commit.clone(),
            vcs_info.branch_name.clone(),
            SessionDiffSource::WorkingTree,
        );
        let mut app = App::build(
            Box::new(StubVcs(vcs_info.clone())),
            vcs_info,
            crate::theme::Theme::dark(),
            None,
            false,
            Vec::new(),
            session,
            DiffSource::WorkingTree,
            InputMode::Normal,
            Vec::new(),
            None,
            None,
        )
        .expect("build app");
        app.repo_picker = Some(RepoPicker {
            rows,
            filter: filter.to_string(),
            cursor,
            return_mode: InputMode::Normal,
        });
        app.input_mode = InputMode::RepoPicker;
        app
    }

    fn screen(app: &mut App) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
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

    fn line_with<'a>(screen: &'a [String], needle: &str) -> &'a str {
        screen
            .iter()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no line with {needle:?} in:\n{}", screen.join("\n")))
    }

    #[test]
    fn should_show_name_path_age_and_the_markers() {
        // given: the current repository first, the cursor on the next one
        let mut current = row("tuicr", "~/code/tuicr", 30);
        current.is_current = true;
        let previous = row("api-server", "~/work/acme/api-server", 3 * 3600);
        let mut gone = row("old-thing", "/mnt/usb/old-thing", 40 * 86_400);
        gone.exists = false;
        let mut app = app_with_picker(vec![current, previous, gone], 1, "");

        // when
        let screen = screen(&mut app);

        // then
        assert!(screen.iter().any(|line| line.contains(" Repositories ")));
        let current = line_with(&screen, "~/code/tuicr");
        assert!(current.contains("* tuicr"), "{current:?}");
        assert!(current.contains("just now"), "{current:?}");
        let cursor = line_with(&screen, "~/work/acme/api-server");
        assert!(cursor.contains("> api-server"), "{cursor:?}");
        assert!(cursor.contains("  3h "), "{cursor:?}");
        let gone = line_with(&screen, "old-thing");
        assert!(gone.contains("/mnt/usb/old-thing (missing)"), "{gone:?}");
        assert!(gone.contains("1mo"), "{gone:?}");
        // the names share a column, so the paths line up
        assert_eq!(
            current.find("~/code/tuicr"),
            cursor.find("~/work/acme/api-server")
        );
    }

    #[test]
    fn should_show_the_filter_and_the_keys() {
        // given
        let mut app = app_with_picker(
            vec![
                row("tuicr", "~/code/tuicr", 30),
                row("api", "~/work/api", 60),
            ],
            0,
            "tcr",
        );

        // when
        let screen = screen(&mut app);

        // then: only the match is listed, under the filter that found it
        assert!(screen.iter().any(|line| line.contains("> tuicr")));
        assert!(!screen.iter().any(|line| line.contains("~/work/api")));
        assert!(screen.iter().any(|line| line.contains("/ tcr")));
        let hints = line_with(&screen, "esc close");
        assert!(hints.contains("\u{21b5} switch"), "{hints:?}");
        assert!(hints.contains("^x remove"), "{hints:?}");
        assert!(hints.contains("^f fetch all"), "{hints:?}");
        assert!(screen.iter().any(|line| line.contains(" REPOS ")));
    }

    /// A fetch-all run in flight over the picker's rows, each having got
    /// as far as the state given.
    fn fetch_run(app: &App, states: &[RepoFetchState]) -> FetchAllJob {
        let picker = app.repo_picker.as_ref().expect("picker");
        // Every repository starts out queued, which is how the run knows
        // how many it has.
        let rows: Vec<FetchAllRow> = picker
            .rows
            .iter()
            .map(|repo| FetchAllRow {
                path: repo.path.clone(),
                name: repo.name.clone(),
                state: RepoFetchState::Queued,
            })
            .collect();
        let (mut job, _workers) = FetchAllJob::new(rows, Vec::new());
        for (row, state) in job.rows.iter_mut().zip(states) {
            row.state = state.clone();
        }
        job
    }

    #[test]
    fn should_show_where_each_repository_stands_in_a_fetch_all() {
        // given: a run part of the way through
        let mut gone = row("old-thing", "/mnt/usb/old-thing", 40 * 86_400);
        gone.exists = false;
        let mut app = app_with_picker(
            vec![
                row("tuicr", "~/code/tuicr", 30),
                row("api-server", "~/work/acme/api-server", 3 * 3600),
                row("notes", "~/notes", 2 * 86_400),
                row("website", "~/work/website", 5 * 86_400),
                row("scratch", "~/tmp/scratch", 6 * 86_400),
                gone,
            ],
            0,
            "",
        );
        app.fetch_all = Some(fetch_run(
            &app,
            &[
                RepoFetchState::Updated,
                RepoFetchState::Fetching,
                RepoFetchState::Queued,
                RepoFetchState::UpToDate,
                RepoFetchState::Failed("origin: fatal: unable to access".to_string()),
                RepoFetchState::Skipped("missing"),
            ],
        ));

        // when
        let screen = screen(&mut app);

        // then: the title counts, and each row says its own state between
        // the path and the age
        assert!(
            screen
                .iter()
                .any(|line| line.contains(" Repositories \u{00b7} fetching 4/6 ")),
            "{}",
            screen.join("\n")
        );
        let has = |path: &str, state: &str| {
            let line = line_with(&screen, path);
            assert!(line.contains(state), "{line:?}");
            assert!(
                line.find(path) < line.find(state),
                "the state follows the path: {line:?}"
            );
        };
        has("~/code/tuicr", "  updated  just now");
        has("~/work/acme/api-server", "  fetching\u{2026}  3h");
        has("~/notes", "  queued  2d");
        has("~/work/website", "  up to date  5d");
        has("/mnt/usb/old-thing (missing)", "  skipped: missing  1mo");
        // a failure's reason has what the path leaves, and is cut to it
        has(
            "~/tmp/scratch",
            "  failed: origin: fatal: unable t\u{2026}  6d",
        );
        // and the status bar counts too
        assert!(screen.last().unwrap().contains(" Fetching 4/6 "));
    }

    #[test]
    fn should_keep_the_results_on_the_rows_once_the_run_is_over() {
        // given: every repository settled
        let mut app = app_with_picker(
            vec![
                row("tuicr", "~/code/tuicr", 30),
                row("api", "~/work/api", 60),
            ],
            0,
            "",
        );
        let rows = fetch_run(&app, &[RepoFetchState::Updated, RepoFetchState::UpToDate]).rows;
        // With nothing queued, a run is over from the start.
        let (job, _workers) = FetchAllJob::new(rows, Vec::new());
        assert!(!job.running());
        app.fetch_all = Some(job);

        // when
        let screen = screen(&mut app);

        // then: no count in the title, the states still on the rows
        assert!(screen.iter().any(|line| line.contains(" Repositories ")));
        assert!(!screen.iter().any(|line| line.contains("fetching")));
        assert!(line_with(&screen, "~/code/tuicr").contains("  updated  "));
        assert!(line_with(&screen, "~/work/api").contains("  up to date  "));
    }

    #[test]
    fn should_cut_a_long_failure_reason_and_keep_the_path() {
        // given: a reason far longer than the row can hold
        let mut app = app_with_picker(vec![row("tuicr", "~/code/tuicr", 30)], 0, "");
        let reason = "origin: fatal: unable to access 'https://example.invalid/a/very/long/\
                      path/to/a/repository.git/': Could not resolve host: example.invalid";
        app.fetch_all = Some(fetch_run(
            &app,
            &[RepoFetchState::Failed(reason.to_string())],
        ));

        // when
        let screen = screen(&mut app);

        // then: the short path is whole, and the reason has the rest
        let line = line_with(&screen, "~/code/tuicr");
        assert!(
            line.contains("~/code/tuicr  failed: origin: fatal: unable t\u{2026}  just now"),
            "{line:?}"
        );
        assert!(!line.contains("Could not resolve host"), "{line:?}");
    }

    #[test]
    fn should_keep_some_of_a_long_path_beside_a_long_failure_reason() {
        // given: a path and a reason that cannot both fit
        let path = "~/work/clients/acme/services/billing/api-gateway/checkouts/main";
        let mut app = app_with_picker(vec![row("main", path, 30)], 0, "");
        let reason = "origin: fatal: unable to access 'https://example.invalid/acme/billing.git/'";
        app.fetch_all = Some(fetch_run(
            &app,
            &[RepoFetchState::Failed(reason.to_string())],
        ));

        // when
        let screen = screen(&mut app);

        // then: the path is shortened from the front and the reason from
        // the back, and what is the matter can still be read
        let line = line_with(&screen, "failed: ");
        assert!(line.contains("> main  \u{2026}/main "), "{line:?}");
        assert!(
            line.contains("  failed: origin: fatal: unable t\u{2026}  just now"),
            "{line:?}"
        );
    }

    #[test]
    fn should_leave_rows_without_a_state_when_no_fetch_all_was_run() {
        // given
        let mut app = app_with_picker(vec![row("tuicr", "~/code/tuicr", 30)], 0, "");

        // when
        let screen = screen(&mut app);

        // then
        let line = line_with(&screen, "~/code/tuicr");
        for word in ["queued", "fetching", "updated", "up to date", "skipped"] {
            assert!(!line.contains(word), "{line:?}");
        }
    }

    #[test]
    fn should_say_so_when_nothing_matches_or_nothing_is_listed() {
        // given
        let mut unmatched = app_with_picker(vec![row("tuicr", "~/code/tuicr", 30)], 0, "zzz");
        let mut empty = app_with_picker(Vec::new(), 0, "");

        // when
        let unmatched = screen(&mut unmatched);
        let empty = screen(&mut empty);

        // then
        assert!(
            unmatched
                .iter()
                .any(|line| line.contains("no repositories match"))
        );
        assert!(
            empty
                .iter()
                .any(|line| line.contains("no repositories yet: \":repo <path>\" opens one"))
        );
    }

    #[test]
    fn should_keep_the_cursor_row_on_screen_in_a_long_list() {
        // given: more repositories than the modal has rows
        let rows: Vec<RepoRow> = (0..40)
            .map(|i| row(&format!("repo-{i:02}"), &format!("~/w/repo-{i:02}"), i * 60))
            .collect();
        let mut app = app_with_picker(rows, 35, "");

        // when
        let screen = screen(&mut app);

        // then
        assert!(screen.iter().any(|line| line.contains("> repo-35")));
        assert!(!screen.iter().any(|line| line.contains("repo-00")));
    }

    #[test]
    fn should_cut_a_long_name_and_keep_the_columns() {
        assert_eq!(fit_name("abc", 5), "abc  ");
        assert_eq!(fit_name("abcdef", 4), "abc\u{2026}");
        assert_eq!(fit_name("abcdef", 6), "abcdef");
        assert_eq!(fit_name("", 3), "   ");
    }
}
