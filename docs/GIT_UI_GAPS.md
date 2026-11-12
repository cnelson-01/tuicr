# Git UI parity: gap analysis (experimental)

Branch: `experimental/git-ui`, cut from `feature/git-tool-window`
after every phase of [ROADMAP.md](ROADMAP.md) shipped.

The yardstick is a full IDE's Git tool window and diff viewer, the git side
only. Editor, inspections, and run configurations are out of scope. Each item
is a gap measured in a real terminal (160x45 and 140x30), not from the docs.
Items are ticked as they land, each in its own commit.

## 1. Side-by-side diff readability

An IDE diff viewer never wraps. It scrolls both sides together and
highlights the changed words inside a modified line. You can drag the
splitter and the tool-window edges, and hide every tool window to read a diff.

- [x] Lines wrap by default (`wrap_lines: true`). Default to no wrap and keep
      `:set wrap` / `wrap = true` as opt-ins
- [x] Clipped lines give no sign that text continues past the edge. Add
      `‹` / `›` markers on the clipped edge of each column
- [x] In unified view, wrapped continuation rows start at column 0 under the
      gutter. Indent them to the content column
- [x] No intra-line (word-level) change highlighting. Paired deletion and
      addition lines should emphasize just the tokens that differ, in both views
- [x] Nothing links a change across the two columns, and the blank rows that
      pad the shorter side look like blank lines. Draw a connector in the
      divider: a band joining each removed line to its replacement, and a
      rule across the shorter side at the point the rest of the change folds
      into
- [x] The side-by-side split is fixed at 50/50. Add keys to move it, a
      `:set split N` command, a reset key, and mouse drag on the divider
- [x] The file tree is fixed at 20% width. Add keys to resize it, a
      `:set treewidth N` command, and mouse drag on its border
- [x] No "hide all tool windows". Add a zen
      toggle that keeps only the diff, and restores the layout when toggled again
- [x] Ignore-whitespace is config-only. Add a runtime toggle that reloads
      the diff
- [x] Horizontal paging moves only 4 columns at a time. Add half-screen
      pans and jumps to the line start and the end of the longest line
- [x] Font size: a TUI can't change the terminal font. Document the terminal
      zoom keys, and use the density controls above (split, tree width, zen)

## 2. Self-documenting keys

An IDE has Find Action (search every action by name, see its shortcut,
run it), and every menu item shows its shortcut. tuicr has a long static `?`
screen, and the status-bar hints for most panes are partial or truncated.

- [x] Command palette: fuzzy-search every key action and `:` command, with
      its key and a description; Enter runs it
- [x] Which-key popup: after a prefix key (`<leader>`, `z`, `Z`, `d`), list
      the keys that can follow and what each does
- [x] `:` completion shows each command's description
- [x] Status-bar hints for the log pane and branch picker are incomplete or
      clipped. Make them fit the width, ending in `? more`
- [x] `?` opens at the section for the focused pane. Covered by the palette
      instead: it lists the focused pane's actions first, and `?` now opens
      with a Start Here section pointing at it

## 3. Git features

### Log

- [x] The log row glues the ref label onto the subject (`[topic/refactor]flip`)
      and shows one ref per commit. Show every ref, styled by kind: HEAD,
      local branch, remote branch, tag
- [x] The log walks only local branches, so commits that exist only on a
      remote (`origin/feature`) never appear, and remote labels are a dim
      grey. Walk remote-tracking branches too and color them apart (purple,
      as in IDE git logs), in the log and the branch picker; `c` on a
      remote-only tip creates the tracking branch
- [x] No commit details pane in the live preview (hash, author, date,
      parents, refs, full message). The message pseudo-file appears only for a
      committed single-commit review
- [x] No ahead/behind (incoming/outgoing) counts for the current branch
- [x] The log pane starts closed (`show_log = false`), unlike the Git tool
      window. Open it by default once a review is on screen, diff focused

### Branches (IDE branch popup)

- [x] The picker can't delete, rename, merge into current, rebase current
      onto, push, or set upstream. It shows no ahead/behind, no last-commit
      subject or age, and no current-branch marker
- [x] No `:merge <branch>` (`:merge` opens only the conflict view)
- [x] No `:rebase <branch>`, `:continue`, `:skip`. You can abort an
      in-progress operation, but you can't finish one from the TUI
- [x] Uncommitted changes block a switch (any change at all, untracked
      files included) and every rebase. Carry them along, and ask only
      about the files that are actually in the way

### Commits

- [x] No interactive rebase editor (an IDE's "Interactively Rebase from
      Here"): reorder, pick, reword, squash, fixup, drop, then run
- [x] Tags can be created but not deleted or pushed (`:tag -d`,
      `:push --tags`)
- [x] Reset makes you type a mode, defaults to the one that leaves a dirty
      tree, and `hard` cannot be taken back. Ask with one key per mode,
      say what each does, default to `keep`, stash what `hard` discards,
      and let `:undo` put a reset or rebase back

### Remote

- [x] `:push` never shows what it will push. Add a confirm step that lists
      the outgoing commits
- [x] No force push. Reword, drop, and fixup on pushed commits leave the
      branch unpushable. Add `:push!` as `--force-with-lease`

### Local changes

- [x] No line-level staging (an IDE's per-line checkboxes). Stage the
      visual-selected lines
- [x] Stash picker can pop, apply, or drop, but can't preview a stash's diff
- [x] No Commit tool window: the staged / unstaged reviews list files only
      after a diff is loaded, with no view of the whole working tree's
      status. Add a changes window (`<leader>t`) in the file tree's place:
      conflicts, staged, unstaged and untracked groups with checkboxes,
      stage / unstage / discard / commit from the list, the diff following
      the cursor. Submodules show their state (new commits, modified,
      untracked) with their own entries nested under them, every operation
      on a nested row running inside the submodule

### Annotate

- [x] The blame gutter is a dead end. Add a key that jumps from the line's
      commit to that commit in the log, with its diff loaded

### Transparency

- [x] No git console (an IDE's Console tab). Keep a log of the git commands
      tuicr ran, their exit status, and their output, viewable in the TUI
