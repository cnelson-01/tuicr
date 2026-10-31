# JetBrains parity: gap analysis (experimental)

Branch: `experimental/jetbrains-parity`, cut from `feature/git-tool-window`
after every phase of [ROADMAP.md](ROADMAP.md) shipped.

The yardstick is the JetBrains Git tool window and diff viewer, the git side
only. Editor, inspections, and run configurations are out of scope. Each item
is a gap measured in a real terminal (160x45 and 140x30), not from the docs.
Items are ticked as they land, each in its own commit.

## 1. Side-by-side diff readability

JetBrains never wraps in its diff viewer. It scrolls both sides together and
highlights the changed words inside a modified line. You can drag the
splitter and the tool-window edges, and hide every tool window to read a diff.

- [x] Lines wrap by default (`wrap_lines: true`). Default to no wrap and keep
      `:set wrap` / `wrap = true` as opt-ins
- [x] Clipped lines give no sign that text continues past the edge. Add
      `‹` / `›` markers on the clipped edge of each column
- [ ] In unified view, wrapped continuation rows start at column 0 under the
      gutter. Indent them to the content column
- [ ] No intra-line (word-level) change highlighting. Paired deletion and
      addition lines should emphasize just the tokens that differ, in both views
- [x] The side-by-side split is fixed at 50/50. Add keys to move it, a
      `:set split N` command, a reset key, and mouse drag on the divider
- [x] The file tree is fixed at 20% width. Add keys to resize it, a
      `:set treewidth N` command, and mouse drag on its border
- [x] No "hide all tool windows" (JetBrains Ctrl-Shift-F12). Add a zen
      toggle that keeps only the diff, and restores the layout when toggled again
- [ ] Ignore-whitespace is config-only. Add a runtime toggle that reloads
      the diff
- [x] Horizontal paging moves only 4 columns at a time. Add half-screen
      pans and jumps to the line start and the end of the longest line
- [ ] Font size: a TUI can't change the terminal font. Document the terminal
      zoom keys, and use the density controls above (split, tree width, zen)

## 2. Self-documenting keys

JetBrains has Find Action (search every action by name, see its shortcut,
run it), and every menu item shows its shortcut. tuicr has a long static `?`
screen, and the status-bar hints for most panes are partial or truncated.

- [ ] Command palette: fuzzy-search every key action and `:` command, with
      its key and a description; Enter runs it
- [ ] Which-key popup: after a prefix key (`<leader>`, `z`, `Z`, `d`), list
      the keys that can follow and what each does
- [ ] `:` completion shows each command's description
- [ ] Status-bar hints for the log pane and branch picker are incomplete or
      clipped. Make them fit the width, ending in `? more`
- [ ] `?` opens at the section for the focused pane

## 3. Git features

### Log

- [ ] The log row glues the ref label onto the subject (`[topic/refactor]flip`)
      and shows one ref per commit. Show every ref, styled by kind: HEAD,
      local branch, remote branch, tag
- [ ] No commit details pane in the live preview (hash, author, date,
      parents, refs, full message). The message pseudo-file appears only for a
      committed single-commit review
- [ ] No ahead/behind (incoming/outgoing) counts for the current branch

### Branches (JetBrains branch popup)

- [ ] The picker can't delete, rename, merge into current, rebase current
      onto, push, or set upstream. It shows no ahead/behind, no last-commit
      subject or age, and no current-branch marker
- [ ] No `:merge <branch>` (`:merge` opens only the conflict view)
- [ ] No `:rebase <branch>`, `:continue`, `:skip`. You can abort an
      in-progress operation, but you can't finish one from the TUI

### Commits

- [ ] No interactive rebase editor (JetBrains "Interactively Rebase from
      Here"): reorder, pick, reword, squash, fixup, drop, then run
- [ ] Tags can be created but not deleted or pushed

### Remote

- [ ] `:push` never shows what it will push. Add a confirm step that lists
      the outgoing commits
- [ ] No force push. Reword, drop, and fixup on pushed commits leave the
      branch unpushable. Add `:push!` as `--force-with-lease`

### Local changes

- [ ] No line-level staging (JetBrains per-line checkboxes). Stage the
      visual-selected lines
- [ ] Stash picker can pop, apply, or drop, but can't preview a stash's diff

### Annotate

- [ ] The blame gutter is a dead end. Add a key that jumps from the line's
      commit to that commit in the log, with its diff loaded

### Transparency

- [ ] No git console (JetBrains Console tab). Keep a log of the git commands
      tuicr ran, their exit status, and their output, viewable in the TUI
