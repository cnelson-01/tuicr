//! The Helix-backed editor pane, driven through the `EditorPane` trait the
//! way the review drives it: built by the factory, opened on a file, typed
//! into, drawn, saved, and reopened after the file changed under it.
//!
//! This file holds exactly one test. Helix can be started once per process,
//! and each file under `tests/` is a process of its own.

#![cfg(feature = "helix")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use tuicr::helix::setup::RuntimeDirs;
use tuicr::helix::{CursorShape, EditorPane, HelixPaneOptions, PaneOutcome, pane_factory};

/// Where the pane is drawn: inside a larger frame, off the corner, so that
/// a blit or a cursor that forgets the origin shows.
const AREA: Rect = Rect {
    x: 2,
    y: 1,
    width: 72,
    height: 16,
};

/// Stands in for the user's home: Helix's config, cache and data, tuicr's
/// data directory, a "Helix source tree" to copy runtime files from, and
/// the files being edited.
struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        for (var, name) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
        ] {
            let path = dir.path().join(name);
            std::fs::create_dir_all(&path).expect("sandbox dir");
            // SAFETY: this binary runs one test, and no other thread exists
            // yet to read the environment while it is written.
            unsafe { std::env::set_var(var, &path) };
        }
        // Helix looks for a workspace from the working directory. tuicr is
        // started through a symbolic link here, as from a shell that did
        // `cd work`: `$PWD` keeps the link, the system resolves it.
        let work = dir.path().join("work");
        #[cfg(unix)]
        {
            std::fs::create_dir_all(dir.path().join("real-work")).expect("work dir");
            std::os::unix::fs::symlink(dir.path().join("real-work"), &work).expect("link");
        }
        #[cfg(not(unix))]
        std::fs::create_dir_all(&work).expect("work dir");
        std::env::set_current_dir(&work).expect("chdir");
        // SAFETY: as above.
        unsafe { std::env::set_var("PWD", &work) };

        // What a Helix checkout's `runtime` directory has, in small.
        let bundled = dir.path().join("helix-src").join("runtime");
        std::fs::create_dir_all(bundled.join("queries").join("rust")).expect("queries");
        std::fs::write(
            bundled.join("queries").join("rust").join("highlights.scm"),
            "(line_comment) @comment\n",
        )
        .expect("query");
        std::fs::create_dir_all(bundled.join("themes")).expect("themes");
        std::fs::write(bundled.join("tutor"), "tutor\n").expect("tutor");
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn runtime(&self) -> RuntimeDirs {
        RuntimeDirs {
            installed: self.path("data").join("tuicr").join("helix-runtime"),
            bundled: self.path("helix-src").join("runtime"),
        }
    }

    fn options(&self) -> HelixPaneOptions {
        HelixPaneOptions {
            runtime: self.runtime(),
            config_file: Some(self.path("helix-config.toml")),
            log_file: Some(self.path("logs").join("helix.log")),
            true_color: false,
            dark: None,
        }
    }
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

/// Type `text` one key at a time; `\n` is Enter and `\x1b` is Escape.
fn type_keys(pane: &mut dyn EditorPane, text: &str) -> PaneOutcome {
    let mut total = PaneOutcome::default();
    for c in text.chars() {
        let code = match c {
            '\n' => KeyCode::Enter,
            '\x1b' => KeyCode::Esc,
            c => KeyCode::Char(c),
        };
        let outcome = pane.handle_key(key(code, KeyModifiers::NONE));
        total.redraw |= outcome.redraw;
        total.close_requested |= outcome.close_requested;
        total.saved.extend(outcome.saved);
        total.opened.extend(outcome.opened);
    }
    total
}

/// Draw the pane into a fresh frame and return the frame's rows as text
/// with the cursor it asked for.
fn draw(pane: &mut dyn EditorPane) -> (Vec<String>, Option<tuicr::helix::PaneCursor>) {
    let mut buf = Buffer::empty(Rect::new(0, 0, AREA.right() + 3, AREA.bottom() + 2));
    let cursor = pane.render(AREA, &mut buf);
    let rows = (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect();
    (rows, cursor)
}

fn screen(pane: &mut dyn EditorPane) -> String {
    draw(pane).0.join("\n")
}

/// Pump as the event loop does until `done`, for at most ten seconds.
fn pump_until(
    pane: &mut dyn EditorPane,
    what: &str,
    mut done: impl FnMut(&PaneOutcome) -> bool,
) -> PaneOutcome {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let outcome = pane.pump();
        if done(&outcome) {
            return outcome;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Write `text` as another program would some seconds later: with a
/// modification time past any the editor has seen, whatever the
/// granularity of the file system's clock.
fn write_later(path: &Path, text: &str, seconds: u64) {
    std::fs::write(path, text).expect("write");
    let file = std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open");
    file.set_modified(SystemTime::now() + Duration::from_secs(seconds))
        .expect("set mtime");
}

#[test]
fn should_edit_a_file_through_the_real_helix_pane() {
    let sandbox = Sandbox::new();
    let runtime = sandbox.runtime();
    let size = Size::new(AREA.width, AREA.height);
    let mut factory = pane_factory(sandbox.options());

    // --- Starting -------------------------------------------------------
    // A Helix config that does not parse stops the start before anything
    // is built: the user can fix the file and press `e` again.
    std::fs::write(sandbox.path("helix-config.toml"), "[editor\n").expect("config");
    let err = factory(size).err().expect("a broken config is an error");
    assert!(err.can_retry(), "nothing was built yet: {err}");
    assert!(err.to_string().contains("helix-config.toml"), "{err}");

    // The fixed config asks for a bar cursor while inserting, which shows
    // below that it is this file the editor reads.
    std::fs::write(
        sandbox.path("helix-config.toml"),
        "[editor.cursor-shape]\ninsert = \"bar\"\n",
    )
    .expect("config");
    let mut boxed = factory(size).unwrap_or_else(|err| panic!("the pane should start: {err}"));
    let pane = boxed.as_mut();

    // The runtime files were installed into tuicr's own directory on the
    // way, without being asked, and the editor logs where it was told to.
    assert!(runtime.is_current());
    assert!(
        runtime
            .installed
            .join("queries/rust/highlights.scm")
            .exists()
    );
    assert!(sandbox.path("logs").join("helix.log").exists());

    // --- Opening --------------------------------------------------------
    let text: String = (1..=40).map(|n| format!("line {n}\n")).collect();
    std::fs::write(sandbox.path("notes.txt"), &text).expect("file");
    // tuicr's spelling of the path is not the editor's (which resolves
    // the `..`), and it is tuicr's that has to come back: the review
    // compares the paths it is told with the ones it handed over.
    let path = sandbox.dir.path().join("work").join("..").join("notes.txt");
    assert_ne!(path, sandbox.path("notes.txt"));

    let outcome = pane.open(&path, Some(30)).expect("open");
    assert!(outcome.redraw);
    assert!(!outcome.close_requested);
    assert_eq!(outcome.opened, [path.as_path()]);
    assert!(outcome.saved.is_empty());
    let state = pane.state();
    assert_eq!(state.path.as_deref(), Some(path.as_path()));
    assert_eq!((state.line, state.col), (30, 1));
    assert_eq!(state.mode, "NOR");
    assert!(state.modified.is_empty());
    assert_eq!(state.pending_writes, 0);

    // --- Drawing --------------------------------------------------------
    let (rows, cursor) = draw(pane);
    assert_eq!(rows[0].trim(), "", "the row above the pane is not its own");
    assert!(
        rows.iter().all(|row| row.starts_with("  ")),
        "nothing is drawn left of the pane:\n{}",
        rows.join("\n")
    );
    let row_of = |rows: &[String], needle: &str| -> usize {
        rows.iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("`{needle}` is not on screen:\n{}", rows.join("\n")))
    };
    // The line asked for is in view, centred, with its neighbours.
    let row = row_of(&rows, "line 30");
    assert!((7..=9).contains(&row), "line 30 is on row {row}");
    row_of(&rows, "line 27");
    row_of(&rows, "line 33");
    // Helix's own status line is part of the frame: the mode, and the
    // cursor's line and column.
    let status = &rows[usize::from(AREA.bottom()) - 2];
    assert!(
        status.contains("NOR") && status.contains("30:1"),
        "{}",
        rows.join("\n")
    );
    // No grammar was built for this runtime directory, and the pane says
    // how to get one where the user is looking: in the editor.
    assert!(
        rows[usize::from(AREA.bottom()) - 1].contains("tuicr --helix-setup"),
        "{}",
        rows.join("\n")
    );
    // In normal mode Helix paints its block cursor into the cells.
    assert_eq!(cursor, None);

    // --- Keys -----------------------------------------------------------
    let outcome = type_keys(pane, "iXY");
    assert!(outcome.redraw);
    assert_eq!(pane.state().mode, "INS");
    let (rows, cursor) = draw(pane);
    let row = row_of(&rows, "XYline 30");
    // The bar the config asked for, in screen coordinates: on the text row,
    // right of the two characters typed.
    let cursor = cursor.expect("a bar cursor while inserting");
    assert_eq!(cursor.shape, CursorShape::Bar);
    assert_eq!(usize::from(cursor.y), row);
    assert_eq!(
        usize::from(cursor.x),
        rows[row].find("XYline 30").expect("column") + 2
    );
    type_keys(pane, "\x1b");
    assert_eq!(pane.state().mode, "NOR");
    assert_eq!(pane.state().modified, [path.as_path()]);

    // tuicr's own chord and key releases never reach the editor.
    let before = pane.state();
    let ignored = pane.handle_key(key(KeyCode::Char('g'), KeyModifiers::CONTROL));
    assert_eq!(ignored, PaneOutcome::default());
    let released = pane.handle_key(KeyEvent::new_with_kind(
        KeyCode::Char('x'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
    ));
    assert_eq!(released, PaneOutcome::default());
    assert_eq!(pane.state(), before);

    // --- Saving ---------------------------------------------------------
    // `:w` only asks for the save. The pane counts it at once, which is
    // what lets the review wait for it before it reloads the diff.
    type_keys(pane, ":w\n");
    assert_eq!(pane.state().pending_writes, 1);
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        text,
        "nothing is written before the pane is pumped"
    );
    let outcome = pump_until(pane, "the save", |outcome| !outcome.saved.is_empty());
    assert_eq!(outcome.saved, [path.as_path()]);
    assert_eq!(pane.state().pending_writes, 0);
    assert!(pane.state().modified.is_empty());
    let saved = std::fs::read_to_string(&path).expect("read");
    assert_eq!(saved, text.replace("line 30\n", "XYline 30\n"));

    // --- Mouse and paste -------------------------------------------------
    let (rows, _) = draw(pane);
    let row = row_of(&rows, "line 27");
    let column = rows[row].find("line 27").expect("column") + 3;
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        pane.handle_mouse(
            MouseEvent {
                kind,
                column: column as u16,
                row: row as u16,
                modifiers: KeyModifiers::NONE,
            },
            AREA,
        );
    }
    let state = pane.state();
    assert_eq!((state.line, state.col), (27, 4));
    let outcome = pane.handle_paste("PASTE ");
    assert!(outcome.redraw);
    assert!(screen(pane).contains("linPASTE e 27"), "{}", screen(pane));
    type_keys(pane, "u");
    assert!(pane.state().modified.is_empty());

    // --- The file changes under a buffer without unsaved changes --------
    // The review rewrites files between visits; the merge view does so
    // just before it hands its file over. Opening shows the file as it is
    // now, and does not report it as opened a second time.
    let rewritten = format!("written by the review\n{saved}");
    write_later(&path, &rewritten, 10);
    let outcome = pane.open(&path, Some(1)).expect("reopen");
    assert!(outcome.opened.is_empty(), "{:?}", outcome.opened);
    let rows = draw(pane).0;
    row_of(&rows, "written by the review");
    assert_eq!(pane.state().line, 1);
    assert!(pane.state().modified.is_empty());

    // --- The file changes under unsaved changes --------------------------
    type_keys(pane, "iMINE \x1b");
    assert_eq!(pane.state().modified, [path.as_path()]);
    write_later(&path, "someone else's\n", 20);
    let outcome = pane.open(&path, None).expect("reopen with unsaved changes");
    assert!(outcome.redraw);
    let rows = draw(pane).0;
    // The buffer is kept, the editor says what happened, and the disk is
    // left as the other program wrote it.
    row_of(&rows, "MINE written by the review");
    assert!(
        rows[usize::from(AREA.bottom()) - 1].contains("notes.txt changed on disk"),
        "{}",
        rows.join("\n")
    );
    assert_eq!(pane.state().modified, [path.as_path()]);
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "someone else's\n"
    );

    // --- Focus, and a pane too small to lay the editor out in ------------
    pane.focus_changed(true);
    pane.focus_changed(false);
    let draw_in = |pane: &mut dyn EditorPane, width: u16, height: u16| -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        pane.render(area, &mut buf);
        (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .map(|at| buf[at].symbol())
            .collect()
    };
    let small_text = draw_in(pane, 12, 4);
    assert!(small_text.contains("Too small"), "{small_text}");
    // Back at a usable size the editor draws again.
    row_of(&draw(pane).0, "MINE written by the review");

    // Splits need more than the smallest pane does: three views stacked
    // take two rows each and the command line. In a pane of six rows,
    // which is otherwise enough, Helix was left with a view that had no
    // row of text, and drawing that view ended tuicr.
    for _ in 0..2 {
        pane.handle_key(key(KeyCode::Char('w'), KeyModifiers::CONTROL));
        type_keys(pane, "s");
    }
    let too_low = draw_in(pane, 72, 6);
    assert!(too_low.contains("Too small for the editor"), "{too_low}");
    assert!(too_low.contains("need 20x7"), "{too_low}");
    // The editor is still there and still takes keys behind the notice.
    type_keys(pane, "j");
    let fits = draw_in(pane, 72, 7);
    assert!(!fits.contains("Too small"), "{fits}");
    assert_eq!(
        fits.matches(" sel ").count(),
        3,
        "three status lines: {fits}"
    );
    // One view again, and the smallest pane is the usual one.
    pane.handle_key(key(KeyCode::Char('w'), KeyModifiers::CONTROL));
    type_keys(pane, "o");
    assert!(!draw_in(pane, 72, 6).contains("Too small"));
    row_of(&draw(pane).0, "MINE written by the review");

    // --- A file opened from inside the editor ------------------------------
    // The review names files under the repository root as git reports it:
    // resolved. A relative path typed in the editor means the same file,
    // and has to reach the same buffer, although tuicr was started through
    // a link (the sandbox's `work`) that `$PWD` still names.
    #[cfg(unix)]
    {
        let resolved = std::env::current_dir().expect("cwd");
        assert!(
            resolved.ends_with("real-work"),
            "the system resolves the link: {}",
            resolved.display()
        );
        let inside = resolved.join("inside.txt");
        std::fs::write(&inside, "inside the project\n").expect("file");
        let outcome = pane.open(&inside, Some(1)).expect("open");
        assert_eq!(outcome.opened, [inside.as_path()]);
        // Shown by its name: the editor knows the file is in its directory.
        let status = draw(pane).0[usize::from(AREA.bottom()) - 2].clone();
        assert!(status.contains(" inside.txt "), "{status}");
        type_keys(pane, "iHERE \x1b");
        let outcome = type_keys(pane, ":open inside.txt\n");
        assert!(
            outcome.opened.is_empty(),
            "no second buffer for the one file: {:?}",
            outcome.opened
        );
        assert_eq!(pane.state().path.as_deref(), Some(inside.as_path()));
        row_of(&draw(pane).0, "HERE inside the project");
        type_keys(pane, "u:buffer-close\n");
        assert_eq!(pane.state().modified, [path.as_path()]);
    }

    // --- Closing ----------------------------------------------------------
    // `:q!` on the last view asks the host to go back to the review, and
    // gives the unsaved buffer up.
    let outcome = type_keys(pane, ":q!\n");
    assert!(outcome.close_requested);
    assert!(pane.state().modified.is_empty());
    assert_eq!(pane.state().path, None);

    // Opened again, the file is read from disk and reported as opened.
    let outcome = pane.open(&path, None).expect("open after close");
    assert_eq!(outcome.opened, [path.as_path()]);
    row_of(&draw(pane).0, "someone else's");

    // --- Ending -------------------------------------------------------------
    // A save asked for and not yet pumped is carried out by the shutdown.
    type_keys(pane, "iLAST \x1b:w!\n");
    let errors = boxed.shutdown();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "LAST someone else's\n"
    );
}
