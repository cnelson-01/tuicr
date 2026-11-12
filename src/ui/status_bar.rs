use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, DiffSource, FocusedPanel, InputMode, Message, MessageType};
use crate::theme::Theme;
use crate::ui::commit_row::CURSOR_GLYPH;
use crate::ui::styles;
use crate::vcs::traits::VcsType;

/// Maximum visible completion candidates in the command prompt popup.
const COMMAND_COMPLETION_MAX_ROWS: usize = 7;

pub fn build_message_span(message: Option<&Message>, theme: &Theme) -> (Span<'static>, usize) {
    if let Some(msg) = message {
        let (fg, bg) = match msg.message_type {
            MessageType::Info => (theme.message_info_fg, theme.message_info_bg),
            MessageType::Warning => (theme.message_warning_fg, theme.message_warning_bg),
            MessageType::Error => (theme.message_error_fg, theme.message_error_bg),
        };
        let detail = msg.content.replace(['\n', '\r'], " ");
        let content = if msg.message_type == MessageType::Error {
            format!(" [:messages] {detail} ")
        } else {
            format!(" {detail} ")
        };
        let width = content.width();
        (
            Span::styled(
                content,
                Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
            ),
            width,
        )
    } else {
        (Span::raw(""), 0)
    }
}

pub fn build_right_aligned_spans<'a>(
    mut left_spans: Vec<Span<'a>>,
    message_span: Span<'a>,
    message_width: usize,
    total_width: usize,
) -> Vec<Span<'a>> {
    let left_width: usize = left_spans.iter().map(|s| s.content.width()).sum();
    let padding_width = total_width.saturating_sub(left_width + message_width);
    let padding = Span::raw(" ".repeat(padding_width));

    left_spans.push(padding);
    if message_width > 0 {
        left_spans.push(message_span);
    }
    left_spans
}

pub fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let in_pr_mode = matches!(app.diff_source, DiffSource::PullRequest(_));

    let brand = Span::styled(
        " tuicr ",
        Style::default()
            .fg(theme.fg_primary)
            .add_modifier(Modifier::BOLD),
    );

    // Right-cluster: source/context chunks, bullet-separated. PR mode leads
    // with a `PR Mode` tag; otherwise we show `<vcs>:<branch> · <source>`.
    let mut chunks: Vec<String> = Vec::new();
    if in_pr_mode {
        chunks.push("PR Mode".to_string());
    } else {
        let vcs_type = &app.vcs_info.vcs_type;
        let branch = app.vcs_info.branch_name.as_deref().unwrap_or("detached");
        // Outgoing / incoming counts against the upstream, and the
        // operation in progress, like an IDE's branch widget.
        let mut chip = format!("{vcs_type}:{branch}");
        if let Some(status) = app.repo_status.as_ref() {
            if status.ahead > 0 {
                chip.push_str(&format!(" \u{2191}{}", status.ahead));
            }
            if status.behind > 0 {
                chip.push_str(&format!(" \u{2193}{}", status.behind));
            }
            if let Some(op) = status.operation {
                chip.push_str(&format!(" \u{00b7} {} IN PROGRESS", op.to_uppercase()));
            }
        }
        chunks.push(chip);
    }
    if let Some(source) = header_source_chunk(app) {
        chunks.push(source);
    }
    if app.is_single_file_view {
        chunks.push("FOCUS".to_string());
    }
    if let Some(slug) = app.session_slug() {
        chunks.push(slug);
    }
    if app.is_pristine_mode {
        // The pristine session key has shape `pristine:<head_or_none>:<hash>`,
        // so the middle segment is the short SHA of the HEAD we're reviewing.
        // "none" renders as `uncommitted` so empty repos read sensibly. A
        // missing prefix falls back to `?` rather than crashing the chip.
        let head_label = app
            .vcs_info
            .head_commit
            .strip_prefix("pristine:")
            .and_then(|rest| rest.split(':').next())
            .map(|raw| if raw == "none" { "uncommitted" } else { raw })
            .unwrap_or("?");
        chunks.push(format!(
            "PRISTINE \u{00b7} {} \u{00b7} {} files",
            head_label,
            app.diff_files.len()
        ));
    }
    let source_text = if chunks.is_empty() {
        String::new()
    } else {
        format!(" {} ", chunks.join(" \u{00b7} "))
    };
    let source_width = source_text.chars().count();
    let source_span = Span::styled(source_text, Style::default().fg(theme.fg_secondary));

    let (update_span, update_width) = match app.update_info.as_ref() {
        Some(info) if info.update_available => {
            let text = format!(" v{} available ", info.latest_version);
            let width = text.chars().count();
            (
                Span::styled(
                    text,
                    Style::default()
                        .fg(theme.update_badge_fg)
                        .bg(theme.update_badge_bg)
                        .add_modifier(Modifier::BOLD),
                ),
                width,
            )
        }
        Some(info) if info.is_ahead => {
            let text = format!(" unreleased v{} ", info.current_version);
            let width = text.chars().count();
            (
                Span::styled(
                    text,
                    Style::default()
                        .fg(theme.update_badge_fg)
                        .bg(theme.update_badge_bg)
                        .add_modifier(Modifier::BOLD),
                ),
                width,
            )
        }
        _ => (Span::raw(""), 0),
    };

    let total_width = area.width as usize;
    let brand_width = brand.content.chars().count();

    // When the diff is the only pane it has no frame/title row, so fold the
    // current file name (left, after the brand) and its diff stats (right,
    // after the source cluster) into this single header line.
    let sole = app.is_diff_sole_pane();
    let (stat_spans, stat_width): (Vec<Span>, usize) = if sole {
        let line = crate::ui::diff_view::diff_stat_title(app);
        let w = line
            .spans
            .iter()
            .map(|s| s.content.chars().count())
            .sum::<usize>();
        (line.spans, w)
    } else {
        (Vec::new(), 0)
    };

    let right_width = source_width + stat_width + update_width;

    let (file_span, file_width) = if sole {
        let label = if app.is_cursor_in_overview() || app.current_file_path().is_none() {
            "Overview".to_string()
        } else {
            app.current_file_path()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        };
        // Leave a two-column minimum gap between the file name and the right
        // cluster; truncate the path (keeping the basename) to whatever fits.
        let avail = total_width.saturating_sub(brand_width + right_width + 2);
        let label = crate::ui::diff_view::truncate_path_smart(&label, avail);
        let text = format!(" {label} ");
        let width = text.chars().count();
        (
            Some(Span::styled(
                text,
                Style::default()
                    .fg(theme.fg_secondary)
                    .add_modifier(Modifier::BOLD),
            )),
            width,
        )
    } else {
        (None, 0)
    };

    let pad_width = total_width.saturating_sub(brand_width + file_width + right_width);

    let mut spans = vec![brand];
    if let Some(file_span) = file_span {
        spans.push(file_span);
    }
    spans.push(Span::raw(" ".repeat(pad_width)));
    spans.push(source_span);
    spans.extend(stat_spans);
    if update_width > 0 {
        spans.push(update_span);
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(styles::status_bar_style(theme)),
        area,
    );
}

/// Short form of the HEAD sha, or `None` when there is no real commit to name.
///
/// File backends and pristine sessions store synthetic keys rather than SHAs.
/// An empty repository has no HEAD at all.
fn head_commit_label(app: &App) -> Option<String> {
    if app.is_pristine_mode || app.vcs_info.vcs_type == VcsType::File {
        return None;
    }
    let head = app.vcs_info.head_commit.as_str();
    (!head.is_empty()).then(|| head[..7.min(head.len())].to_string())
}

/// Suffix a working-tree label with the commit being diffed against.
fn with_head_commit(label: &str, app: &App) -> String {
    match head_commit_label(app) {
        Some(head) => format!("{label} \u{00b7} commit {head}"),
        None => label.to_string(),
    }
}

/// Short, lowercase description of the active review source, including the
/// commit it is diffed against. Returns `None` only when there is nothing to
/// add beyond `vcs:branch`, which now means an empty repository.
fn header_source_chunk(app: &App) -> Option<String> {
    match &app.diff_source {
        // The working-tree family all diff against HEAD but never named it, so
        // the commit under review was only visible via `-r <sha>`. The
        // commit-range arms below already identify their own revision.
        DiffSource::WorkingTree => head_commit_label(app).map(|head| format!("commit {head}")),
        DiffSource::Staged => Some(with_head_commit("staged", app)),
        DiffSource::Unstaged => Some(with_head_commit("unstaged", app)),
        DiffSource::StagedAndUnstaged => Some(with_head_commit("staged + unstaged", app)),
        DiffSource::CommitRange(commits) => {
            if commits.len() == 1 {
                Some(format!("commit {}", &commits[0][..7.min(commits[0].len())]))
            } else {
                Some(
                    app.commit_selection_summary()
                        .unwrap_or_else(|| format!("{} commits", commits.len())),
                )
            }
        }
        DiffSource::StagedUnstagedAndCommits(commits) => {
            if commits.len() == 1 {
                Some(format!(
                    "staged + unstaged + commit {}",
                    &commits[0][..7.min(commits[0].len())]
                ))
            } else {
                Some(format!("staged + unstaged + {} commits", commits.len()))
            }
        }
        DiffSource::Submodule(sub) => Some(format!(
            "{} \u{00b7} {}",
            sub.path.display(),
            if sub.staged { "staged" } else { "unstaged" }
        )),
        DiffSource::PullRequest(pr) => {
            let slug = pr.key.repository.display_name();
            let trimmed_title = if pr.title.chars().count() > 60 {
                let truncated: String = pr.title.chars().take(59).collect();
                format!("{truncated}\u{2026}")
            } else {
                pr.title.clone()
            };
            let mut s = format!(
                "{slug}#{number} \u{00b7} {trimmed_title}",
                number = pr.key.number
            );
            if app.pr_commits.len() > 1
                && let Some(summary) = app.commit_selection_summary()
            {
                s.push_str(&format!(" \u{00b7} {summary}"));
            }
            Some(s)
        }
    }
}

pub fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;

    // Right-aligned slot priority: active message > pr-flow spinners
    // (submit/reload/range) > remote-comments loading hint > modified
    // indicator. Surfaces the most important transient state without
    // crowding the hints on the left.
    let (right_span, right_width) = if app.message.is_some() {
        build_message_span(app.message.as_ref(), theme)
    } else if let Some(submit) = app.pr_submit_state.as_ref() {
        use crate::forge::submit::SubmitEvent;
        let glyph = crate::ui::selector::pr_open_spinner_glyph(submit.started_at.elapsed());
        let label = match submit.event {
            SubmitEvent::Draft => "Pushing pending review…",
            _ => "Submitting review…",
        };
        let content = format!(" {glyph} {label} ");
        let width = content.chars().count();
        (
            Span::styled(
                content,
                Style::default()
                    .fg(theme.message_info_fg)
                    .bg(theme.message_info_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            width,
        )
    } else if let Some(reload) = app.pr_reload_state.as_ref() {
        let glyph = crate::ui::selector::pr_open_spinner_glyph(reload.started_at.elapsed());
        let content = format!(" {glyph} Reloading PR… ");
        let width = content.chars().count();
        (
            Span::styled(
                content,
                Style::default()
                    .fg(theme.message_info_fg)
                    .bg(theme.message_info_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            width,
        )
    } else if let Some(range) = app.pr_range_reload_state.as_ref() {
        let glyph = crate::ui::selector::pr_open_spinner_glyph(range.started_at.elapsed());
        let content = format!(" {glyph} Loading range diff… ");
        let width = content.chars().count();
        (
            Span::styled(
                content,
                Style::default()
                    .fg(theme.message_info_fg)
                    .bg(theme.message_info_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            width,
        )
    } else if app.forge_review_threads_loading {
        let content = " loading remote comments\u{2026} ".to_string();
        let width = content.chars().count();
        (
            Span::styled(content, Style::default().fg(theme.fg_dim)),
            width,
        )
    } else if app.dirty {
        let content = " \u{2022} modified ".to_string();
        let width = content.chars().count();
        (
            Span::styled(content, Style::default().fg(theme.pending)),
            width,
        )
    } else {
        (Span::raw(""), 0)
    };
    // In command/search mode, show the input on the left (vim-style)
    let total_width = area.width as usize;
    let left_spans = if matches!(app.input_mode, InputMode::Command | InputMode::Search) {
        let prefix = if app.input_mode == InputMode::Command {
            ":"
        } else {
            "/"
        };
        let buffer = if app.input_mode == InputMode::Command {
            &app.command_buffer
        } else {
            &app.search_buffer
        };
        let command_text = format!("{prefix}{buffer}");
        vec![Span::styled(
            command_text,
            Style::default().fg(theme.fg_primary),
        )]
    } else if let Some(draft) = app.commit_filter_draft.as_ref() {
        // The log pane's filter prompt lives here, like `/` search does.
        vec![Span::styled(
            format!("/{draft}"),
            Style::default().fg(theme.fg_primary),
        )]
    } else {
        let mode_str = match app.input_mode {
            InputMode::Normal => {
                if let Some(count) = app.pending_count {
                    format!(" NORMAL {count} ")
                } else {
                    " NORMAL ".to_string()
                }
            }
            InputMode::Command => " COMMAND ".to_string(),
            InputMode::Search => " SEARCH ".to_string(),
            InputMode::Comment => " COMMENT ".to_string(),
            InputMode::Help => " HELP ".to_string(),
            InputMode::MessageDetails => " ERROR ".to_string(),
            InputMode::Summary => " SUMMARY ".to_string(),
            InputMode::Confirm => " CONFIRM ".to_string(),
            InputMode::CommitSelect => " SELECT ".to_string(),
            InputMode::VisualSelect => {
                if let Some((range, _)) = app.visual_selection_line_range() {
                    if range.is_single() {
                        format!(" VISUAL L{} ", range.start)
                    } else {
                        format!(" VISUAL L{}-L{} ", range.start, range.end)
                    }
                } else {
                    " VISUAL ".to_string()
                }
            }
            InputMode::SubmitResolver => " RESOLVE ".to_string(),
            InputMode::SubmitConfirm => " SUBMIT ".to_string(),
            InputMode::SubmitActionPicker => " SUBMIT ".to_string(),
            InputMode::BranchPicker => "BRANCHES".to_string(),
            InputMode::StashPicker => "STASHES".to_string(),
            InputMode::FindResults => " FIND ".to_string(),
            InputMode::MergeConflict => " MERGE ".to_string(),
            InputMode::Palette => " ACTIONS ".to_string(),
            InputMode::Console => " CONSOLE ".to_string(),
            InputMode::RebaseEditor => " REBASE ".to_string(),
            InputMode::ThemePicker => " THEME ".to_string(),
        };

        let mode_width = mode_str.width();
        let mode_span = Span::styled(mode_str, styles::mode_style(theme));

        let hints: String = if app.message.is_some() {
            String::new()
        } else {
            let room = total_width
                .saturating_sub(mode_width)
                .saturating_sub(right_width)
                .saturating_sub(4);
            format!("   {}", fit_hints(&mode_hints(app), room))
        };
        let hints_span = Span::styled(hints, Style::default().fg(theme.fg_secondary));

        let mut spans = vec![mode_span, hints_span];
        if app.input_mode == InputMode::Normal
            && app.message.is_none()
            && let Some((current, total)) = app.search_match_position()
        {
            spans.push(Span::styled(
                format!("   [{current}/{total}]"),
                Style::default().fg(theme.fg_secondary),
            ));
        }
        spans
    };

    let spans = build_right_aligned_spans(left_spans, right_span, right_width, total_width);

    let line = Line::from(spans);

    let status = Paragraph::new(line)
        .style(styles::status_bar_style(theme))
        .block(Block::default());

    frame.render_widget(status, area);
}

/// One status-bar hint; `pinned` hints survive when the bar is too narrow
/// for everything.
pub(crate) struct Hint {
    text: String,
    pinned: bool,
}

pub(crate) fn hint(text: impl Into<String>) -> Hint {
    Hint {
        text: text.into(),
        pinned: false,
    }
}

pub(crate) fn pinned(text: impl Into<String>) -> Hint {
    Hint {
        text: text.into(),
        pinned: true,
    }
}

/// The keys worth knowing in the current mode and pane, most useful first.
/// Normal-mode panes end with the palette and help keys, so every key is
/// one search away even when the bar can't list it.
fn mode_hints(app: &App) -> Vec<Hint> {
    let leader = crate::input::catalog::display_leader(app.leader_key);
    let actions = || pinned("^p actions");
    if app.file_tree_prompt_editing() || app.commit_filter_editing() {
        // File-tree prompts are a sub-state of Normal, so the mode chip
        // still reads NORMAL; the hint is what tells the user Enter/Esc
        // are the way out.
        return vec![pinned("\u{21b5} apply"), pinned("esc cancel")];
    }
    if app.theme_picker_filtering() {
        return vec![pinned("\u{21b5} apply filter"), pinned("esc cancel filter")];
    }
    match app.input_mode {
        InputMode::Normal if app.log_pane_focused() => vec![
            hint("j/k move"),
            hint("\u{21b5} load"),
            hint("space pick"),
            hint("c checkout"),
            hint("n branch"),
            hint("p cherry-pick"),
            hint("R reword"),
            hint("f fixup"),
            hint("D drop"),
            hint("v revert"),
            hint("x reset"),
            hint("t tag"),
            hint("F fetch"),
            hint("S branch"),
            hint("/ filter"),
            hint("esc diff"),
            actions(),
        ],
        InputMode::Normal if app.focused_panel == FocusedPanel::Changes => vec![
            hint("j/k move"),
            hint("\u{2423} stage/unstage"),
            hint("\u{21b5} open"),
            hint("a/A all"),
            hint("X discard"),
            hint("c commit"),
            hint("h/l fold"),
            hint("r refresh"),
            hint("esc diff"),
            actions(),
            pinned("? help"),
        ],
        InputMode::Normal if app.focused_panel == FocusedPanel::FileList => vec![
            hint("j/k move"),
            hint("\u{21b5} open"),
            hint("i/e filter"),
            hint("I/E clear"),
            hint("/ find"),
            hint("r reviewed"),
            hint("s/u stage"),
            hint(format!("{leader}[ {leader}] width")),
            actions(),
            pinned("? help"),
        ],
        InputMode::Normal => vec![
            hint("j/k move"),
            hint("n/N change"),
            hint("]/[ hunk"),
            hint("}/{ file"),
            hint("h/l pan"),
            hint("c comment"),
            hint("r reviewed"),
            hint("s/S stage"),
            hint(format!("{leader}g log")),
            hint(format!("{leader}b branches")),
            hint(format!("{leader} more")),
            actions(),
            pinned("? help"),
        ],
        InputMode::Command => vec![
            pinned("tab complete"),
            pinned("\u{21b5} execute"),
            pinned("esc cancel"),
        ],
        InputMode::Search => vec![pinned("\u{21b5} search"), pinned("esc cancel")],
        InputMode::Comment if !app.comment_vim_enabled => vec![
            hint("\u{2191}/\u{2193} row"),
            pinned("ctrl-s save"),
            pinned("esc cancel"),
        ],
        InputMode::Comment => vec![pinned("ctrl-s save"), pinned("esc cancel")],
        InputMode::Help => vec![hint("/ search"), hint("n/N match"), pinned("q/?/esc close")],
        InputMode::MessageDetails => vec![hint("j/k scroll"), pinned("q/esc close")],
        InputMode::Summary => vec![
            hint("j/k select"),
            hint("dd delete"),
            hint("\u{21b5} jump"),
            pinned("q/esc close"),
        ],
        InputMode::Confirm if app.pending_choice.is_some() => app
            .pending_choice
            .iter()
            .flat_map(|choice| choice.options.iter())
            .map(|option| pinned(format!("{} {}", option.key, option.label)))
            .chain([pinned("\u{21b5} default"), pinned("esc cancel")])
            .collect(),
        InputMode::Confirm => vec![pinned("y yes"), pinned("n no")],
        InputMode::FindResults => vec![
            hint("j/k move"),
            hint("\u{21b5} open"),
            hint("e editor"),
            hint("/ new search"),
            pinned("esc close"),
        ],
        InputMode::StashPicker => vec![
            hint("j/k move"),
            hint("\u{21b5} pop"),
            hint("a apply"),
            hint("v view"),
            hint("d drop"),
            pinned("esc close"),
        ],
        InputMode::BranchPicker => vec![
            hint("type to filter"),
            hint("\u{21b5} checkout"),
            hint("^s stash+switch"),
            hint("^g merge"),
            hint("^b rebase onto"),
            hint("^x delete"),
            hint("^r rename"),
            hint("^t track"),
            hint("tab pick"),
            hint("^d compare"),
            hint("^o log"),
            hint("^f fetch"),
            pinned("esc close"),
        ],
        InputMode::CommitSelect => vec![
            hint("j/k navigate"),
            hint("space select"),
            hint("\u{21b5} confirm"),
            pinned("esc back"),
        ],
        InputMode::VisualSelect => vec![
            hint("j/k extend"),
            hint("c/\u{21b5} comment"),
            hint("s suggest"),
            hint("S/U stage lines"),
            hint("y yank"),
            pinned("esc/V cancel"),
        ],
        InputMode::SubmitResolver => vec![
            hint("j/k move"),
            hint("\u{21b5} toggle"),
            hint("s submit"),
            pinned("esc cancel"),
        ],
        InputMode::SubmitConfirm => vec![pinned("y submit"), pinned("n cancel")],
        InputMode::SubmitActionPicker => vec![
            hint("j/k move"),
            hint("\u{21b5} submit"),
            pinned("esc cancel"),
        ],
        InputMode::MergeConflict => vec![
            hint("n/N conflict"),
            hint("o/t/b take"),
            hint("u undo"),
            hint("w write"),
            pinned("esc back"),
        ],
        InputMode::RebaseEditor => vec![
            hint("j/k move"),
            hint("p/r/s/f/d action"),
            hint("J/K reorder"),
            hint("u reset"),
            pinned("\u{21b5} run"),
            pinned("esc cancel"),
        ],
        InputMode::Console => vec![
            hint("j/k scroll"),
            hint("m reads too"),
            hint("c clear"),
            pinned("esc close"),
        ],
        InputMode::Palette => vec![
            hint("type to search"),
            hint("\u{2191}/\u{2193} move"),
            pinned("\u{21b5} run"),
            pinned("esc close"),
        ],
        InputMode::ThemePicker => vec![
            hint("j/k move"),
            hint("/ filter"),
            pinned("\u{21b5} apply"),
            pinned("esc cancel"),
        ],
    }
}

/// Join hints with ` · `, dropping unpinned ones from the end until the
/// line fits in `width` columns.
pub(crate) fn fit_hints(hints: &[Hint], width: usize) -> String {
    let join = |keep: &[&Hint]| {
        keep.iter()
            .map(|h| h.text.as_str())
            .collect::<Vec<_>>()
            .join(" \u{00b7} ")
    };
    let mut keep: Vec<&Hint> = hints.iter().collect();
    loop {
        let line = join(&keep);
        if line.width() <= width {
            return line;
        }
        let Some(pos) = keep.iter().rposition(|h| !h.pinned) else {
            return line;
        };
        keep.remove(pos);
    }
}

pub fn render_command_completion_popup(frame: &mut Frame, app: &App, status_area: Rect) {
    if app.input_mode != InputMode::Command {
        return;
    }
    let Some(completion) = app.command_completion.as_ref() else {
        return;
    };
    if completion.matches.is_empty() || status_area.y < 3 || status_area.width < 4 {
        return;
    }

    let available_rows = status_area.y.saturating_sub(2) as usize;
    let visible_rows = completion
        .matches
        .len()
        .min(COMMAND_COMPLETION_MAX_ROWS)
        .min(available_rows);
    if visible_rows == 0 {
        return;
    }

    let selected = completion
        .selected
        .min(completion.matches.len().saturating_sub(1));
    let start = completion_window_start(selected, completion.matches.len(), visible_rows);
    let end = start + visible_rows;
    let visible_matches = &completion.matches[start..end];
    let name_width = visible_matches
        .iter()
        .map(|command| command.chars().count())
        .max()
        .unwrap_or(0);
    // Each command's description from the action catalog, when it has one.
    let descriptions: Vec<Option<&str>> = visible_matches
        .iter()
        .map(|command| crate::input::catalog::command_description(command))
        .collect();
    let desc_width = descriptions
        .iter()
        .flatten()
        .map(|d| d.width() + 3)
        .max()
        .unwrap_or(0);
    let content_width = name_width + 2 + desc_width;
    let popup_width = (content_width as u16 + 2).min(status_area.width);
    if popup_width == 0 {
        return;
    }

    let popup_height = visible_rows as u16 + 2;
    let popup_area = Rect {
        x: status_area.x,
        y: status_area.y.saturating_sub(popup_height),
        width: popup_width,
        height: popup_height,
    };

    let rows: Vec<Line<'_>> = visible_matches
        .iter()
        .enumerate()
        .map(|(offset, command)| {
            let idx = start + offset;
            let marker = if idx == selected { CURSOR_GLYPH } else { " " };
            let style = if idx == selected {
                styles::selected_style(&app.theme)
            } else {
                Style::default().fg(app.theme.fg_secondary)
            };
            let mut spans = vec![
                Span::styled(marker, styles::selection_marker_style(&app.theme, style)),
                Span::styled(format!(" {command:<name_width$}"), style),
            ];
            if let Some(desc) = descriptions[offset] {
                spans.push(Span::styled(
                    format!("   {desc}"),
                    style.fg(app.theme.fg_dim),
                ));
            }
            Line::from(spans)
        })
        .collect();

    frame.render_widget(Clear, popup_area);
    frame.render_widget(
        Paragraph::new(rows)
            .style(styles::panel_style(&app.theme))
            .block(Block::default().borders(Borders::ALL)),
        popup_area,
    );
}

fn completion_window_start(selected: usize, match_count: usize, visible_rows: usize) -> usize {
    if visible_rows >= match_count {
        return 0;
    }
    let half = visible_rows / 2;
    selected
        .saturating_sub(half)
        .min(match_count.saturating_sub(visible_rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_hints_drops_unpinned_hints_from_the_end() {
        let hints = vec![
            hint("aaaa"),
            hint("bbbb"),
            hint("cccc"),
            pinned("^p actions"),
        ];
        assert_eq!(fit_hints(&hints, 100), "aaaa · bbbb · cccc · ^p actions");
        assert_eq!(fit_hints(&hints, 24), "aaaa · bbbb · ^p actions");
        assert_eq!(fit_hints(&hints, 5), "^p actions");
    }

    fn test_message(message_type: MessageType) -> Message {
        Message {
            content: "hello".to_string(),
            message_type,
            expires_at: None,
        }
    }

    #[test]
    fn should_style_info_message_using_theme_fields() {
        let theme = Theme::dark();
        let (span, width) = build_message_span(Some(&test_message(MessageType::Info)), &theme);
        assert_eq!(span.style.fg, Some(theme.message_info_fg));
        assert_eq!(span.style.bg, Some(theme.message_info_bg));
        assert_eq!(width, " hello ".len());
    }

    #[test]
    fn should_return_empty_span_when_message_is_none() {
        let theme = Theme::dark();
        let (span, width) = build_message_span(None, &theme);
        assert_eq!(span.content.as_ref(), "");
        assert_eq!(width, 0);
    }

    #[test]
    fn should_center_completion_window_when_possible() {
        assert_eq!(completion_window_start(5, 12, 7), 2);
    }

    #[test]
    fn should_pin_completion_window_to_top_near_start() {
        assert_eq!(completion_window_start(1, 12, 7), 0);
    }

    #[test]
    fn should_pin_completion_window_to_bottom_near_end() {
        assert_eq!(completion_window_start(10, 12, 7), 5);
    }

    #[test]
    fn should_show_all_completion_rows_when_they_fit() {
        assert_eq!(completion_window_start(3, 5, 7), 0);
    }

    #[test]
    fn should_style_warning_message_using_theme_fields() {
        let theme = Theme::dark();
        let (span, _) = build_message_span(Some(&test_message(MessageType::Warning)), &theme);
        assert_eq!(span.style.fg, Some(theme.message_warning_fg));
        assert_eq!(span.style.bg, Some(theme.message_warning_bg));
    }

    #[test]
    fn should_style_error_message_using_theme_fields() {
        let theme = Theme::dark();
        let (span, _) = build_message_span(Some(&test_message(MessageType::Error)), &theme);
        assert_eq!(span.style.fg, Some(theme.message_error_fg));
        assert_eq!(span.style.bg, Some(theme.message_error_bg));
    }
}

#[cfg(test)]
mod header_snapshot_tests {
    //! Render-snapshot coverage for the status bar header in PR mode.
    //! Drives the full `render_header` against ratatui's `TestBackend`
    //! and asserts on the produced character grid.

    use crate::app::{App, DiffSource, InputMode, PullRequestDiffSource};
    use crate::error::Result as TuicrResult;
    use crate::error::TuicrError;
    use crate::forge::traits::{ForgeRepository, PrSessionKey};
    use crate::model::{DiffFile, DiffLine, FileStatus, ReviewSession, SessionDiffSource};
    use crate::syntax::SyntaxHighlighter;
    use crate::theme::Theme;
    use crate::vcs::traits::{VcsBackend, VcsInfo, VcsType};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use std::path::{Path, PathBuf};

    struct NoopVcs {
        info: VcsInfo,
    }
    impl VcsBackend for NoopVcs {
        fn info(&self) -> &VcsInfo {
            &self.info
        }
        fn get_working_tree_diff(
            &self,
            _highlighter: &SyntaxHighlighter,
        ) -> TuicrResult<Vec<DiffFile>> {
            Err(TuicrError::NoChanges)
        }
        fn fetch_context_lines(
            &self,
            _file_path: &Path,
            _file_status: FileStatus,
            _ref_commit: Option<&str>,
            _start_line: u32,
            _end_line: u32,
        ) -> TuicrResult<Vec<DiffLine>> {
            Ok(Vec::new())
        }
        fn file_line_count(
            &self,
            _file_path: &Path,
            _file_status: FileStatus,
            _ref_commit: Option<&str>,
        ) -> TuicrResult<u32> {
            Ok(0)
        }
    }

    fn pr_source(closed: bool, merged: bool) -> PullRequestDiffSource {
        PullRequestDiffSource {
            key: PrSessionKey::new(
                ForgeRepository::github("github.com", "agavra", "tuicr"),
                125,
                "abcdef0123456789".to_string(),
            ),
            base_sha: "1234567890abcdef".to_string(),
            title: "Add forge-backed PR review".to_string(),
            url: "https://github.com/agavra/tuicr/pull/125".to_string(),
            head_ref_name: "reviews".to_string(),
            base_ref_name: "main".to_string(),
            state: if closed { "CLOSED" } else { "OPEN" }.to_string(),
            closed,
            merged,
        }
    }

    fn build_pr_app(pr: PullRequestDiffSource) -> App {
        let vcs_info = VcsInfo {
            root_path: PathBuf::from("forge:github.com/agavra/tuicr"),
            head_commit: pr.key.head_sha.clone(),
            branch_name: Some(pr.head_ref_name.clone()),
            vcs_type: VcsType::File,
        };
        let mut session = ReviewSession::new(
            vcs_info.root_path.clone(),
            pr.key.head_sha.clone(),
            Some(pr.head_ref_name.clone()),
            SessionDiffSource::PullRequest,
        );
        session.pr_session_key = Some(pr.key.clone());
        App::build(
            Box::new(NoopVcs {
                info: vcs_info.clone(),
            }),
            vcs_info,
            Theme::dark(),
            None,
            false,
            Vec::new(),
            session,
            DiffSource::PullRequest(Box::new(pr)),
            InputMode::Normal,
            Vec::new(),
            None,
            None,
        )
        .expect("build pr app")
    }

    fn draw_header(app: &App) -> Buffer {
        let backend = TestBackend::new(140, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                super::render_header(frame, app, area);
            })
            .expect("draw frame");
        terminal.backend().buffer().clone()
    }

    fn draw_app(app: &mut App, width: u16, height: u16) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, app))
            .expect("draw frame");
        terminal.backend().buffer().clone()
    }

    fn row_text(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    }

    #[test]
    fn should_make_full_long_error_accessible_from_status_bar() {
        let mut app = build_pr_app(pr_source(false, false));
        app.set_error(
            "Submit failed: GitHub command failed: gh: Unprocessable Entity (HTTP 422)\n\
             {\"message\":\"Unprocessable Entity\",\"errors\":[\"Review Can not approve your own pull request\"]}",
        );

        let status = draw_app(&mut app, 60, 12);
        assert!(
            row_text(&status, 11).contains("[:messages]"),
            "status bar should advertise full error details"
        );

        app.open_message_details();
        let buffer = draw_app(&mut app, 60, 12);
        let rendered = (0..buffer.area.height)
            .map(|y| row_text(&buffer, y))
            .collect::<Vec<_>>()
            .join("\n");
        let compact = rendered
            .replace('│', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");

        assert!(
            compact.contains("Submit failed: GitHub command failed: gh: Unprocessable Entity"),
            "error prefix should remain visible, got:\n{rendered}"
        );

        app.help_scroll_to_bottom();
        let buffer = draw_app(&mut app, 60, 12);
        let rendered = (0..buffer.area.height)
            .map(|y| row_text(&buffer, y))
            .collect::<Vec<_>>()
            .join("\n");
        let compact = rendered
            .replace('│', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            compact.contains("Review Can not approve your own pull request"),
            "full error detail should remain reachable, got:\n{rendered}"
        );
    }

    #[test]
    fn should_scroll_to_end_of_oversized_error_details() {
        let mut app = build_pr_app(pr_source(false, false));
        app.set_error(format!(
            "Submit failed: {}UNIQUE_END",
            "long error ".repeat(200)
        ));
        app.open_message_details();

        let _ = draw_app(&mut app, 60, 12);
        app.help_scroll_to_bottom();
        let buffer = draw_app(&mut app, 60, 12);
        let rendered = (0..buffer.area.height)
            .map(|y| row_text(&buffer, y))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            rendered.contains("UNIQUE_END"),
            "last error detail should be reachable, got:\n{rendered}"
        );
    }

    #[test]
    fn should_scroll_error_details_with_mouse_wheel() {
        let mut app = build_pr_app(pr_source(false, false));
        app.set_error(format!("failed: {}", "long error ".repeat(200)));
        app.open_message_details();
        let _ = draw_app(&mut app, 60, 12);

        crate::handler::handle_mouse_event(
            &mut app,
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::ScrollDown,
                column: 0,
                row: 0,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        );

        assert!(app.help_state.scroll_offset > 0);
    }

    #[test]
    fn should_open_error_details_with_messages_command_and_close_to_normal_mode() {
        let mut app = build_pr_app(pr_source(false, false));
        app.input_mode = InputMode::Command;
        app.command_buffer = "messages".to_string();
        app.set_error("failed to load target");

        crate::handler::handle_command_action(&mut app, crate::input::Action::SubmitInput);
        assert_eq!(app.input_mode, InputMode::MessageDetails);
        app.toggle_help();

        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn should_reset_scroll_when_another_error_replaces_the_open_message() {
        let mut app = build_pr_app(pr_source(false, false));
        app.set_error(format!("failed: {}", "long error ".repeat(200)));
        app.open_message_details();
        let _ = draw_app(&mut app, 60, 12);
        app.help_scroll_to_bottom();
        assert!(app.help_state.scroll_offset > 0);

        app.set_error("replacement error");

        assert_eq!(app.input_mode, InputMode::MessageDetails);
        assert_eq!(app.help_state.scroll_offset, 0);
    }

    #[test]
    fn should_report_when_messages_command_has_no_current_error() {
        let mut app = build_pr_app(pr_source(false, false));
        app.input_mode = InputMode::Command;
        app.command_buffer = "messages".to_string();

        crate::handler::handle_command_action(&mut app, crate::input::Action::SubmitInput);

        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(
            app.message.as_ref().map(|message| message.content.as_str()),
            Some("No current error")
        );
    }

    #[test]
    fn should_close_error_details_when_a_non_error_replaces_the_message() {
        let mut app = build_pr_app(pr_source(false, false));
        app.set_error("request failed");
        app.open_message_details();

        app.set_message("request recovered");

        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn should_render_pr_mode_header_with_slug_number_and_title() {
        // given a PR-mode app for agavra/tuicr#125
        let app = build_pr_app(pr_source(false, false));
        // when
        let buffer = draw_header(&app);
        // then — brand on the left, then bullet-separated PR Mode tag,
        // slug#number, and title in the right cluster.
        let line = row_text(&buffer, 0);
        assert!(line.contains("tuicr"), "got: {line:?}");
        assert!(line.contains("PR Mode"), "got: {line:?}");
        assert!(line.contains("agavra/tuicr#125"), "got: {line:?}");
        assert!(line.contains("Add forge-backed PR review"), "got: {line:?}");
    }

    // Read-only badges are no longer shown in the header: the `PR Mode`
    // tag itself signals the user is on a forge-managed review; whether
    // the PR is open/closed/merged is left to the submit flow to surface
    // (the submit action errors with a clear message). These tests now
    // assert the simpler invariant: closed/merged still show `PR Mode`
    // but don't add a `read only` chip.

    #[test]
    fn should_not_show_read_only_badge_for_closed_pr() {
        // given a closed PR
        let app = build_pr_app(pr_source(true, false));
        // when
        let buffer = draw_header(&app);
        // then
        let line = row_text(&buffer, 0);
        assert!(line.contains("PR Mode"), "got: {line:?}");
        assert!(!line.contains("read only"), "got: {line:?}");
    }

    #[test]
    fn should_fold_file_and_stats_into_header_when_diff_is_sole_pane() {
        // given a PR app with no other panes (file list hidden, no commit pane)
        let mut app = build_pr_app(pr_source(false, false));
        app.show_file_list = false;
        assert!(app.is_diff_sole_pane());
        // when
        let buffer = draw_header(&app);
        // then: the diff title row is gone; its file label + stats fold into
        // this header line (no diff files -> "Overview" and +0/-0).
        let line = row_text(&buffer, 0);
        assert!(line.contains("PR Mode"), "got: {line:?}");
        assert!(line.contains("Overview"), "got: {line:?}");
        assert!(line.contains("+0") && line.contains("-0"), "got: {line:?}");
    }

    #[test]
    fn should_not_fold_file_into_header_when_other_panes_visible() {
        // given the file list is visible -> diff keeps its own framed title
        let mut app = build_pr_app(pr_source(false, false));
        app.show_file_list = true;
        assert!(!app.is_diff_sole_pane());
        // when
        let buffer = draw_header(&app);
        // then the header does not carry the file label
        let line = row_text(&buffer, 0);
        assert!(!line.contains("Overview"), "got: {line:?}");
    }

    #[test]
    fn should_not_show_read_only_badge_for_merged_pr() {
        // given a merged PR
        let app = build_pr_app(pr_source(false, true));
        // when
        let buffer = draw_header(&app);
        // then
        let line = row_text(&buffer, 0);
        assert!(line.contains("PR Mode"), "got: {line:?}");
        assert!(!line.contains("read only"), "got: {line:?}");
    }

    #[test]
    fn should_omit_read_only_badge_for_open_pr() {
        // given
        let app = build_pr_app(pr_source(false, false));
        // when
        let buffer = draw_header(&app);
        // then
        let line = row_text(&buffer, 0);
        assert!(!line.contains("read only"), "got: {line:?}");
    }

    fn fake_pr_commit(oid: &str, summary: &str) -> crate::forge::traits::PullRequestCommit {
        crate::forge::traits::PullRequestCommit {
            oid: oid.to_string(),
            short_oid: oid[..7.min(oid.len())].to_string(),
            summary: summary.to_string(),
            author: "Alice".to_string(),
            timestamp: None,
        }
    }

    #[test]
    fn should_render_n_of_m_commits_when_subset_selected() {
        // given a 3-commit PR with a two-commit subrange selected
        let mut app = build_pr_app(pr_source(false, false));
        app.pr_commits = vec![
            fake_pr_commit("aaaaaaa1", "third"),
            fake_pr_commit("bbbbbbb2", "second"),
            fake_pr_commit("ccccccc3", "first"),
        ];
        app.review_commits = app
            .pr_commits
            .iter()
            .map(crate::app::pr_commit_to_commit_info)
            .collect();
        app.commit_selection_range = Some((0, 1));
        // when
        let buffer = draw_header(&app);
        // then
        let line = row_text(&buffer, 0);
        assert!(line.contains("2 of 3 commits"), "got: {line:?}");
    }

    #[test]
    fn should_render_commit_position_when_single_commit_selected() {
        // given a 3-commit PR with a single commit selected, the header shows
        // its position (not "1 of 3") so it changes as ( / ) cycle.
        let mut app = build_pr_app(pr_source(false, false));
        app.pr_commits = vec![
            fake_pr_commit("aaaaaaa1", "third"),
            fake_pr_commit("bbbbbbb2", "second"),
            fake_pr_commit("ccccccc3", "first"),
        ];
        app.review_commits = app
            .pr_commits
            .iter()
            .map(crate::app::pr_commit_to_commit_info)
            .collect();
        app.commit_selection_range = Some((1, 1));
        // when
        let buffer = draw_header(&app);
        // then
        let line = row_text(&buffer, 0);
        assert!(line.contains("commit 2/3"), "got: {line:?}");
    }

    #[test]
    fn should_omit_commits_label_when_full_range_selected() {
        // given a 3-commit PR with all commits selected
        let mut app = build_pr_app(pr_source(false, false));
        app.pr_commits = vec![
            fake_pr_commit("a", "third"),
            fake_pr_commit("b", "second"),
            fake_pr_commit("c", "first"),
        ];
        app.commit_selection_range = Some((0, 2));
        // when
        let buffer = draw_header(&app);
        // then — no `N of M commits` subset label since the full range is selected
        let line = row_text(&buffer, 0);
        assert!(!line.contains(" of 3 commits"), "got: {line:?}");
    }

    #[test]
    fn should_omit_commits_label_for_single_commit_pr() {
        // given a single-commit PR — the selector is hidden, no label.
        let mut app = build_pr_app(pr_source(false, false));
        app.pr_commits = vec![fake_pr_commit("a", "only commit")];
        app.commit_selection_range = Some((0, 0));
        // when
        let buffer = draw_header(&app);
        // then
        let line = row_text(&buffer, 0);
        assert!(!line.contains(" of "), "got: {line:?}");
    }

    #[test]
    fn should_not_panic_on_long_multibyte_title() {
        // regression: a >60-char title whose truncation boundary falls inside a
        // multibyte character used to panic when the title was sliced by byte index.
        let mut pr = pr_source(false, false);
        pr.title =
            "プルリクエストのタイトルが非常に長くて六十文字を超える場合でも表示が壊れずに正しく切り詰められることを確認するためのテストです"
                .to_string();
        let app = build_pr_app(pr);
        // when — must render without panicking on a non-char-boundary byte slice
        let buffer = draw_header(&app);
        // then — the header still renders the PR identifier (reaching this
        // assertion at all proves the truncation no longer panics)
        let line = row_text(&buffer, 0);
        assert!(line.contains("agavra/tuicr#125"), "got: {line:?}");
    }
    fn build_local_app(diff_source: DiffSource, head_commit: &str) -> App {
        let vcs_info = VcsInfo {
            root_path: PathBuf::from("/repo"),
            head_commit: head_commit.to_string(),
            branch_name: Some("main".to_string()),
            vcs_type: VcsType::Git,
        };
        let session = ReviewSession::new(
            vcs_info.root_path.clone(),
            vcs_info.head_commit.clone(),
            vcs_info.branch_name.clone(),
            SessionDiffSource::WorkingTree,
        );
        App::build(
            Box::new(NoopVcs {
                info: vcs_info.clone(),
            }),
            vcs_info,
            Theme::dark(),
            None,
            false,
            Vec::new(),
            session,
            diff_source,
            InputMode::Normal,
            Vec::new(),
            None,
            None,
        )
        .expect("build local app")
    }

    #[test]
    fn should_show_head_commit_when_reviewing_the_working_tree() {
        let app = build_local_app(DiffSource::WorkingTree, "abcdef0123456789");
        assert_eq!(
            super::header_source_chunk(&app),
            Some("commit abcdef0".to_string())
        );
    }

    #[test]
    fn should_omit_synthetic_file_commits_from_the_header() {
        for head_commit in ["abcdef0123456789", "file"] {
            let mut app = build_local_app(DiffSource::WorkingTree, head_commit);
            app.vcs_info.vcs_type = VcsType::File;
            assert_eq!(app.vcs_info.head_commit, head_commit);

            let buffer = draw_header(&app);
            let line = row_text(&buffer, 0);

            assert!(line.contains("file:main"), "got: {line:?}");
            assert!(!line.contains("commit "), "got: {line:?}");
        }
    }

    #[test]
    fn should_show_head_commit_alongside_staged_labels() {
        let staged = build_local_app(DiffSource::Staged, "abcdef0123456789");
        assert_eq!(
            super::header_source_chunk(&staged),
            Some("staged \u{00b7} commit abcdef0".to_string())
        );

        let both = build_local_app(DiffSource::StagedAndUnstaged, "abcdef0123456789");
        assert_eq!(
            super::header_source_chunk(&both),
            Some("staged + unstaged \u{00b7} commit abcdef0".to_string())
        );
    }

    #[test]
    fn should_omit_head_commit_when_repository_has_no_head() {
        let app = build_local_app(DiffSource::WorkingTree, "");
        assert_eq!(super::header_source_chunk(&app), None);

        let staged = build_local_app(DiffSource::Staged, "");
        assert_eq!(
            super::header_source_chunk(&staged),
            Some("staged".to_string())
        );
    }

    #[test]
    fn should_not_duplicate_commit_for_a_revision_review() {
        // `-r <sha>` already names its own revision; HEAD must not be appended.
        let app = build_local_app(
            DiffSource::CommitRange(vec!["fedcba9876543210".to_string()]),
            "abcdef0123456789",
        );
        assert_eq!(
            super::header_source_chunk(&app),
            Some("commit fedcba9".to_string())
        );
    }

    #[test]
    fn should_render_head_commit_in_the_header() {
        let app = build_local_app(DiffSource::WorkingTree, "abcdef0123456789");
        let buffer = draw_header(&app);
        let line = row_text(&buffer, 0);
        assert!(line.contains("commit abcdef0"), "got: {line:?}");
    }
}
