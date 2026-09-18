# Git tool window roadmap

Goal: make tuicr's history and branch workflow feel like the JetBrains Git tool
window. Phases land in order, each as its own commit(s). Tick items as they ship.

## Phase 1: land the feature branch on current main

- [x] Merge `feature/merge-view-and-commit-graph` onto main; re-home the
      `src/app.rs` changes into the `src/app/*` submodules
- [x] Build, clippy, and tests green
- [x] KEYBINDINGS / AGENTS docs merged

## Phase 2: persistent log pane

- [x] Commit graph renders in the inline commit pane beside the diff, not only in
      the full-screen selector
- [x] Pane height is adjustable and `<leader>s` still toggles it
- [x] Highlighting a commit previews its diff live (no Enter needed)
- [x] Changed-file list follows the highlighted commit

## Phase 3: branch operations

- [x] Switch branch (`:checkout <name>` and a picker); checkout a commit (detached)
- [x] Create a branch at the cursor commit
- [x] Dirty-tree guard: stash-and-switch or refuse with a clear message
- [x] Abort an in-progress cherry-pick / rebase / merge / revert
- [x] Graph shows remote-tracking branches, tags, and a HEAD marker

## Phase 4: log filter

- [x] Filter local commits by message text, author, and path
- [x] Filter prompt in the log pane with a match count

## Phase 5: local changes

- [x] Stage / unstage per file
- [x] Stage / unstage per hunk
- [x] Discard (rollback) a hunk or a file
- [x] Commit with a message; amend
- [x] Stash push / pop / list

## Phase 6: find in files

- [ ] `rg`-backed search over the working tree with a results pane
- [ ] Enter jumps to the diff line when loaded, otherwise opens `$EDITOR` there

## Phase 7: annotate and history

- [ ] Blame gutter toggle
- [ ] Per-file history view
