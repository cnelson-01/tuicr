# Git tool window roadmap

Goal: make tuicr's history and branch workflow feel like the JetBrains Git tool
window. Phases land in order, each as its own commit(s). Tick items as they ship.

## Phase 1: land the feature branch on current main

- [ ] Merge `feature/merge-view-and-commit-graph` onto main; re-home the
      `src/app.rs` changes into the `src/app/*` submodules
- [ ] Build, clippy, and tests green
- [ ] KEYBINDINGS / AGENTS docs merged

## Phase 2: persistent log pane

- [ ] Commit graph renders in the inline commit pane beside the diff, not only in
      the full-screen selector
- [ ] Pane height is adjustable and `<leader>s` still toggles it
- [ ] Highlighting a commit previews its diff live (no Enter needed)
- [ ] Changed-file list follows the highlighted commit

## Phase 3: branch operations

- [ ] Switch branch (`:checkout <name>` and a picker); checkout a commit (detached)
- [ ] Create a branch at the cursor commit
- [ ] Dirty-tree guard: stash-and-switch or refuse with a clear message
- [ ] Abort an in-progress cherry-pick / rebase / merge / revert
- [ ] Graph shows remote-tracking branches, tags, and a HEAD marker

## Phase 4: log filter

- [ ] Filter local commits by message text, author, and path
- [ ] Filter prompt in the log pane with a match count

## Phase 5: local changes

- [ ] Stage / unstage per file
- [ ] Stage / unstage per hunk
- [ ] Discard (rollback) a hunk or a file
- [ ] Commit with a message; amend
- [ ] Stash push / pop / list

## Phase 6: find in files

- [ ] `rg`-backed search over the working tree with a results pane
- [ ] Enter jumps to the diff line when loaded, otherwise opens `$EDITOR` there

## Phase 7: annotate and history

- [ ] Blame gutter toggle
- [ ] Per-file history view
