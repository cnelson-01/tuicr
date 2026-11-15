//! The embedded editor, seen from tuicr.
//!
//! tuicr can show an editor inside its own frame instead of suspending to
//! `$EDITOR`. This module is the seam: [`EditorPane`] is what the rest of
//! tuicr talks to, and [`PaneSlot`] holds the one pane a process may have.
//! The Helix-backed implementation plugs in behind the trait through a
//! [`PaneFactory`]; nothing outside this module names Helix types.
//!
//! With the `helix` feature (on by default) the implementation is here
//! too: [`pane_factory`] builds a pane around the Helix editor running in
//! this process, and [`setup`] looks after the runtime files it reads.
//! Without the feature there is no factory to install, and files open in
//! `$EDITOR` as they did before there was an embedded editor.
//!
//! The state that ties a pane to the review (focus, the mode to return to,
//! the cursor) lives in `app/editor_pane.rs`, and the drawing in
//! `ui/editor_pane.rs`.

#[cfg(feature = "helix")]
mod blit;
#[cfg(feature = "helix")]
mod embedded;
#[cfg(test)]
pub(crate) mod fake;
#[cfg(feature = "helix")]
mod input;
mod pane;
#[cfg(feature = "helix")]
pub mod setup;

#[cfg(feature = "helix")]
pub use embedded::{HelixPaneOptions, pane_factory};

pub use pane::{
    CRASHED, CursorShape, EditorPane, NO_FACTORY, PaneCursor, PaneFactory, PaneOutcome, PaneSlot,
    PaneStartError, PaneState, RESTART_HINT, SHUT_DOWN, is_return_chord, note_contained_panic,
};
