//! Shows that tuicr is working while its main thread is blocked.
//!
//! History operations (cherry-pick, rebase, merge, checkout, ...) and large
//! diff loads run on the main thread, which also draws the screen, so
//! nothing could tell the user that a key had landed until the operation
//! came back. A watchdog thread fills that gap. The main loop marks when it
//! starts handling an event ([`BusyIndicator::begin`]) and when it goes
//! back to waiting ([`BusyIndicator::end`]); if a handler is still running
//! after [`SHOW_AFTER`], the watchdog paints a spinner over the bottom row,
//! with the git command in flight when there is one, and keeps it turning
//! until the main loop takes the screen back.
//!
//! The watchdog writes to the terminal directly, outside ratatui. That is
//! safe because the main thread draws only between `end` and the next
//! `begin`, and both take the lock the watchdog paints under. `end` reports
//! whether anything was painted so the caller can repaint every cell:
//! ratatui's diff does not know the bottom row changed.

use std::io::Write;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::{JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use crossterm::{
    cursor::{MoveTo, RestorePosition, SavePosition},
    queue,
    style::{
        Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
    },
};
use unicode_width::UnicodeWidthStr;

/// How long a handler may run before the indicator appears. Shorter work
/// reads as instant, and painting for it would only flicker.
pub const SHOW_AFTER: Duration = Duration::from_millis(250);
/// Time between spinner frames, and how often the watchdog looks.
const FRAME: Duration = Duration::from_millis(80);
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// The thread whose git commands label the indicator: the one that called
/// [`BusyIndicator::start`].
static MAIN_THREAD: OnceLock<ThreadId> = OnceLock::new();
/// The git command the main thread is waiting on, as the console shows it.
static GIT_IN_FLIGHT: Mutex<Option<String>> = Mutex::new(None);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Held while a git command runs. On the main thread it names the command
/// for the indicator; on any other thread it does nothing, since a worker's
/// command is not what the main thread is waiting on.
pub struct GitCommandGuard {
    labelled: bool,
}

/// Note that `git <args>` is starting on this thread.
pub fn git_command(args: &[String]) -> GitCommandGuard {
    let labelled = MAIN_THREAD.get() == Some(&std::thread::current().id());
    if labelled {
        *lock(&GIT_IN_FLIGHT) = Some(command_label(args));
    }
    GitCommandGuard { labelled }
}

/// The command as the console shows it, with full object ids cut to the
/// eight characters a person reads.
fn command_label(args: &[String]) -> String {
    let short: Vec<String> = args
        .iter()
        .map(|arg| {
            if arg.len() >= 20 && arg.bytes().all(|b| b.is_ascii_hexdigit()) {
                arg[..8].to_string()
            } else {
                arg.clone()
            }
        })
        .collect();
    crate::git_console::display_args(&short)
}

impl Drop for GitCommandGuard {
    fn drop(&mut self) {
        if self.labelled {
            *lock(&GIT_IN_FLIGHT) = None;
        }
    }
}

struct State {
    /// When the main thread began the work it is still doing.
    since: Option<Instant>,
    /// Whether the indicator is on screen.
    shown: bool,
    fg: Color,
    bg: Color,
    quit: bool,
}

/// The watchdog and the main loop's handle on it.
pub struct BusyIndicator {
    state: Arc<Mutex<State>>,
    watchdog: Option<JoinHandle<()>>,
}

impl BusyIndicator {
    /// Start the watchdog, painting to `writer` (the terminal the TUI is
    /// on). Call from the thread that runs the event loop.
    pub fn start<W: Write + Send + 'static>(writer: W) -> Self {
        let _ = MAIN_THREAD.set(std::thread::current().id());
        Self::start_with(writer, SHOW_AFTER, || crossterm::terminal::size().ok())
    }

    fn start_with<W, S>(mut writer: W, show_after: Duration, size: S) -> Self
    where
        W: Write + Send + 'static,
        S: Fn() -> Option<(u16, u16)> + Send + 'static,
    {
        let state = Arc::new(Mutex::new(State {
            since: None,
            shown: false,
            fg: Color::Black,
            bg: Color::Yellow,
            quit: false,
        }));
        let shared = Arc::clone(&state);
        let watchdog = std::thread::spawn(move || {
            let mut frame = 0usize;
            loop {
                std::thread::sleep(FRAME);
                // The lock is held across the write, so `end` cannot return
                // while a frame is half on the wire.
                let mut state = lock(&shared);
                if state.quit {
                    break;
                }
                let Some(since) = state.since else {
                    continue;
                };
                let elapsed = since.elapsed();
                if elapsed < show_after {
                    continue;
                }
                let Some((cols, rows)) = size() else {
                    continue;
                };
                let label = lock(&GIT_IN_FLIGHT).clone();
                let text =
                    indicator_text(SPINNER[frame % SPINNER.len()], label.as_deref(), elapsed);
                if paint(&mut writer, &text, cols, rows, state.fg, state.bg).is_ok() {
                    state.shown = true;
                }
                frame += 1;
            }
        });
        Self {
            state,
            watchdog: Some(watchdog),
        }
    }

    /// The main thread is starting work: handling an event, or installing
    /// what a worker sent back. Repeated calls keep the first start time.
    pub fn begin(&self) {
        let mut state = lock(&self.state);
        if state.since.is_none() {
            state.since = Some(Instant::now());
        }
    }

    /// The main thread is done and about to draw or wait for input. Returns
    /// whether the indicator was painted, in which case the caller must
    /// repaint the whole screen. Also call this before handing the terminal
    /// to another program (`$EDITOR`).
    pub fn end(&self) -> bool {
        let mut state = lock(&self.state);
        state.since = None;
        std::mem::take(&mut state.shown)
    }

    /// Colours for the indicator, normally the theme's warning message
    /// pair, so it reads like the status bar's own "Fetching…".
    pub fn set_colors(&self, fg: ratatui::style::Color, bg: ratatui::style::Color) {
        let mut state = lock(&self.state);
        state.fg = terminal_color(fg);
        state.bg = terminal_color(bg);
    }
}

impl Drop for BusyIndicator {
    fn drop(&mut self) {
        lock(&self.state).quit = true;
        if let Some(watchdog) = self.watchdog.take() {
            let _ = watchdog.join();
        }
    }
}

/// A theme colour as the escape sequence writer wants it.
fn terminal_color(color: ratatui::style::Color) -> Color {
    use ratatui::style::Color as Theme;
    match color {
        Theme::Reset => Color::Reset,
        Theme::Black => Color::Black,
        Theme::Red => Color::DarkRed,
        Theme::Green => Color::DarkGreen,
        Theme::Yellow => Color::DarkYellow,
        Theme::Blue => Color::DarkBlue,
        Theme::Magenta => Color::DarkMagenta,
        Theme::Cyan => Color::DarkCyan,
        Theme::Gray => Color::Grey,
        Theme::DarkGray => Color::DarkGrey,
        Theme::LightRed => Color::Red,
        Theme::LightGreen => Color::Green,
        Theme::LightYellow => Color::Yellow,
        Theme::LightBlue => Color::Blue,
        Theme::LightMagenta => Color::Magenta,
        Theme::LightCyan => Color::Cyan,
        Theme::White => Color::White,
        Theme::Rgb(r, g, b) => Color::Rgb { r, g, b },
        Theme::Indexed(i) => Color::AnsiValue(i),
    }
}

/// ` ⠹ git cherry-pick 1a2b3c4  3s `: the spinner, what is running, and how
/// long it has been once that is worth saying.
fn indicator_text(spinner: char, git: Option<&str>, elapsed: Duration) -> String {
    let what = git.unwrap_or("Working\u{2026}");
    if elapsed >= Duration::from_secs(1) {
        format!(" {spinner} {what}  {}s ", elapsed.as_secs())
    } else {
        format!(" {spinner} {what} ")
    }
}

/// `text` cut to `width` columns (with `…` when it does not fit) and padded
/// out to exactly `width`.
fn fit_to_width(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    let fits = text.width() <= width;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        // Leave a column for the ellipsis when the text is being cut.
        if used + w > width.saturating_sub(usize::from(!fits)) {
            break;
        }
        out.push(ch);
        used += w;
    }
    if !fits && used < width {
        out.push('\u{2026}');
        used += 1;
    }
    out.extend(std::iter::repeat_n(' ', width - used));
    out
}

/// Paint `text` across the bottom row, leaving the cursor where it was.
fn paint<W: Write>(
    writer: &mut W,
    text: &str,
    cols: u16,
    rows: u16,
    fg: Color,
    bg: Color,
) -> std::io::Result<()> {
    if cols == 0 || rows == 0 {
        return Ok(());
    }
    queue!(
        writer,
        SavePosition,
        MoveTo(0, rows - 1),
        SetForegroundColor(fg),
        SetBackgroundColor(bg),
        SetAttribute(Attribute::Bold),
        Print(fit_to_width(text, usize::from(cols))),
        SetAttribute(Attribute::Reset),
        ResetColor,
        RestorePosition,
    )?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer the test can read back while the watchdog owns the other end.
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            lock(&self.0).extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Shared {
        fn text(&self) -> String {
            String::from_utf8_lossy(&lock(&self.0)).into_owned()
        }
    }

    fn indicator(out: &Shared, show_after: Duration) -> BusyIndicator {
        BusyIndicator::start_with(out.clone(), show_after, || Some((40, 10)))
    }

    #[test]
    fn should_paint_only_after_the_work_has_run_long_enough() {
        // given a handler that has been running past the threshold
        let out = Shared::default();
        let busy = indicator(&out, Duration::from_millis(120));
        busy.begin();
        std::thread::sleep(Duration::from_millis(60));
        // then nothing is on screen yet
        assert!(out.text().is_empty(), "painted before the threshold");

        // when it keeps running
        std::thread::sleep(Duration::from_millis(400));
        // then the indicator is on the bottom row and `end` says to repaint
        let painted = out.text();
        assert!(painted.contains("Working"), "{painted:?}");
        assert!(painted.contains("\u{1b}[10;1H"), "bottom row: {painted:?}");
        assert!(busy.end(), "end reports the paint");
        // and reports it once
        assert!(!busy.end());
    }

    #[test]
    fn should_stay_quiet_for_quick_work_and_while_idle() {
        // given work that ends before the threshold
        let out = Shared::default();
        let busy = indicator(&out, Duration::from_millis(200));
        busy.begin();
        std::thread::sleep(Duration::from_millis(40));
        assert!(!busy.end());
        // when the loop then sits idle well past it
        std::thread::sleep(Duration::from_millis(400));
        // then nothing was ever painted
        assert!(out.text().is_empty(), "{:?}", out.text());
    }

    #[test]
    fn should_stop_painting_once_the_main_loop_takes_the_screen_back() {
        // given an indicator that is on screen
        let out = Shared::default();
        let busy = indicator(&out, Duration::from_millis(50));
        busy.begin();
        std::thread::sleep(Duration::from_millis(300));
        assert!(busy.end());
        let after_end = out.text().len();
        // when the main loop is drawing or waiting
        std::thread::sleep(Duration::from_millis(300));
        // then the watchdog writes nothing more
        assert_eq!(out.text().len(), after_end);
    }

    #[test]
    fn should_keep_the_first_start_time_across_repeated_begins() {
        let out = Shared::default();
        let busy = indicator(&out, Duration::from_millis(200));
        busy.begin();
        std::thread::sleep(Duration::from_millis(150));
        // a second event in the same burst does not restart the clock
        busy.begin();
        std::thread::sleep(Duration::from_millis(250));
        assert!(busy.end(), "the burst as a whole ran long enough");
    }

    #[test]
    fn should_name_the_git_command_and_the_time_it_has_taken() {
        assert_eq!(
            indicator_text('⠋', None, Duration::from_millis(300)),
            " ⠋ Working\u{2026} "
        );
        assert_eq!(
            indicator_text(
                '⠙',
                Some("git cherry-pick 1a2b3c4"),
                Duration::from_millis(900)
            ),
            " ⠙ git cherry-pick 1a2b3c4 "
        );
        assert_eq!(
            indicator_text('⠹', Some("git rebase main"), Duration::from_millis(3200)),
            " ⠹ git rebase main  3s "
        );
    }

    #[test]
    fn should_shorten_object_ids_in_the_label() {
        let args: Vec<String> = [
            "-c",
            "commit.gpgsign=false",
            "cherry-pick",
            "0ce42cd27d47a3463e64e0d150424ff68e4dd686",
            "deadbeef",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(command_label(&args), "git cherry-pick 0ce42cd2 deadbeef");
    }

    #[test]
    fn should_fit_the_text_to_the_row() {
        // padded out to the full width
        assert_eq!(fit_to_width(" ⠋ ok ", 10), " ⠋ ok     ");
        // cut with an ellipsis, never wider than the row
        let cut = fit_to_width(" ⠋ git cherry-pick 1a2b3c4 ", 12);
        assert_eq!(cut.width(), 12);
        assert!(cut.ends_with('\u{2026}'), "{cut:?}");
        // a wide character that would straddle the edge is dropped
        assert_eq!(fit_to_width("ab漢字", 5).width(), 5);
        assert_eq!(fit_to_width("", 3), "   ");
    }

    #[test]
    fn should_label_git_commands_from_the_main_thread_only() {
        // This test's thread is not the one that started an indicator, so
        // its commands must not label one.
        let other = std::thread::spawn(|| {
            let _guard = git_command(&["status".to_string()]);
            lock(&GIT_IN_FLIGHT).clone()
        })
        .join()
        .expect("thread");
        assert_eq!(other, None);
    }
}
