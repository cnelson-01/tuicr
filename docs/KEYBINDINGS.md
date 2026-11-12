# Keybindings

Full reference. Press `?` inside tuicr for an in-app version of this list.

## Discovering keys

| Key | Action |
|-----|--------|
| `Ctrl-p` | Command palette (also `<leader>p`, `:palette`, `:actions`). Type words from an action's name, group, or keys (`stage hunk`, `push`, `zen`); every word must match. The focused pane's actions rank first. `↑` / `↓` (or `Ctrl-n` / `Ctrl-p`, `Ctrl-j` / `Ctrl-k`) move, `Enter` runs, `Esc` clears the query, then closes |
| `<leader>`, `z`, `Z`, `d` | Prefix keys. Press one and a popup lists every key that can follow it and what each does |

The palette runs an action by focusing the pane it belongs to (log, file tree,
diff) and replaying its keys, so a palette run behaves exactly like typing them.
Actions that take an argument (`:tag NAME`, `:set split N`) open the command
line pre-filled. `:` completions show each command's description. The status
bar lists the focused pane's keys, dropping the least important to fit the
width, and always ends with `^p actions`.

`<leader>` defaults to `;`. Override it with `leader = ","` in `~/.config/tuicr/config.toml`.

## Navigation

| Key | Action |
|-----|--------|
| `j` / `↓` | Scroll down |
| `k` / `↑` | Scroll up |
| `h` / `←` | Scroll left |
| `l` / `→` | Scroll right |
| `zH` / `zL` | Pan half a column left / right |
| `zs` / `ze` | Pan to the line start / to the end of the longest line on screen |
| `Ctrl-d` / `Ctrl-u` | Half page down / up |
| `Ctrl-f` / `Ctrl-b` | Full page down / up |
| `g` / `G` | Go to first / last file |
| `{N}G` | Go to source line N in current file |
| `{N}{motion}` | Vim-style count prefix — repeats `j` / `k` / `h` / `l` / `{` / `}` / `[` / `]` / `n` / `N` `N` times |
| `{` / `}` | Jump to previous / next file |
| `[` / `]` | Jump to previous / next hunk |
| `n` / `N` | Jump to next / previous change (a run of added or deleted lines; context between two edits splits a hunk into two changes). In single-file view it crosses into the next / previous file. While a search is highlighted, `n` / `N` step search matches instead |
| `m` / `M` | Jump to next / previous comment |
| `/` | Search within diff (case-insensitive regex); matches on diff content are highlighted and the status bar shows the `[current/total]` position (headers, comments, and PR info are searchable but not highlighted) |
| `n` / `N` (search highlighted) | Next / previous search match (wraps around) |
| `Esc` | Clear search-match highlighting; `n` / `N` go back to stepping changes |
| `f` | Find in files: opens the command line with `find ` (also in the file tree and changes window; the log pane keeps `f` for fixup) |
| `Enter` | Expand or collapse hidden context between hunks |
| `zt` | Scroll cursor to top of screen |
| `zz` | Center cursor on screen |
| `zb` | Scroll cursor to bottom of screen |
| `Ctrl-l` | Clear and redraw the whole screen |

`Ctrl-l` works in every mode, including while typing a comment, a `:` command or
a search. Use it when the terminal leaves stale text on screen, for example after
a font size change in a window that wasn't visible. tuicr also redraws in full
whenever the terminal reports a resize.

## Diff search

Diff `/` search uses the Rust `regex` syntax, case-insensitively by default,
like the file-tree include/exclude filters. For example, `foo|bar` matches either
word, `^fn\s+` finds function declarations, and `foo\.bar` matches a literal dot.
Plain-text searches keep their existing Unicode case folding. The same compiled
pattern drives navigation and highlighting in both diff layouts; side-by-side
columns are matched independently. Zero-width matches (such as `^`) are navigable
but have no characters to highlight. An invalid expression reports an error
without moving the cursor or replacing the previous valid search; it clears the
highlighting, so `n` / `N` step changes until the next search. File-tree and
help `/` searches remain text searches.

## Reading a diff comfortably

Long lines don't wrap by default. Both side-by-side columns pan together
(`h` / `l`, `zH` / `zL`, `zs` / `ze`, or a horizontal trackpad swipe). A `›` at
a column's right edge means the line continues past it, and a `‹` in the
gutter means text has scrolled off the left. File and hunk headers stay put.

In side-by-side view the divider joins each change across the two columns. A
removed line and the line that replaced it share a band, the old side's colour
meeting the new side's at the centre. When one side of a change has more lines
than the other, the shorter side is padded with blank rows. The first of them
is where the change folds: the longer side's band steps across the divider and
runs on as a thin rule over the blank row. The rule sits between the two lines
on that side where the extra lines were removed or will be added. On the rows
below it the band stays on its own side.

To give the code more room:

- Drag the `│` divider, or use `<leader><` / `<leader>>`, to give the side
  you're reading more width (`:set split 65`; `split = 65` in config)
- Drag the file tree's right border, or use `<leader>[` / `<leader>]`
  (`:set treewidth 12`)
- `<leader>z` hides every panel but the diff; press it again to restore them
- `<leader>w` ignores whitespace-only changes

A terminal app can't change its own font size; use your terminal's zoom
instead (`Cmd =` / `Cmd -` in Ghostty, iTerm2, and Terminal.app; `Ctrl =` /
`Ctrl -` on most Linux terminals). tuicr reflows on every resize.

## Help

Press `?` to open help.

| Key | Action |
|-----|--------|
| `/` | Search within help (case-insensitive) |
| `n` / `N` | Next / previous help search match |
| `j` / `k` | Scroll down / up |
| `q` / `?` / `Esc` | Close help |

## File tree

| Key | Action |
|-----|--------|
| `Space` | Toggle expand directory |
| `Enter` | Expand directory / jump to file in diff |
| `o` | Expand all directories |
| `O` | Collapse all directories |
| `i` | Filter to files matching a regex (include) |
| `e` | Filter out files matching a regex (exclude) |
| `I` | Clear the include filter |
| `E` | Clear the exclude filter |
| `/` | Search file paths (substring) |
| `n` / `N` | Next / previous file-path match |

These keys are only active while the file tree is focused — in the diff, `i` still
edits the comment at the cursor and `/` still searches the diff.

### Filtering

`i` and `e` take case-insensitive regexes matched against each file's **full
relative path** (`^src/`, `\.rs$`, `test|spec`). Both can be active at once:
include runs first, then exclude removes from what's left.

A filter is not just a tree view — hidden files also disappear from the diff
pane, from `{`/`}` file navigation, `[`/`]` hunk navigation, and from the file
and `+/-` counts in the header. The tree title reports how much is hidden
(`Files · 2/12 · 12 of 58`), and the active patterns show in its bottom border.

Enter applies, `Esc` cancels, `Ctrl-u` clears the line. Reopening a prompt
pre-fills the pattern already applied, so submitting an emptied buffer is the
same as `I` / `E`. An invalid regex reports the error and leaves the prompt open
so it can be fixed. Comments on hidden files are not deleted and still export —
filters are a view, not an edit.

Filters are session-local: they are not written to the review session and reset
when tuicr restarts, but they survive a `:e` reload.

### Hiding reviewed files

`:set noreviewed` / `:set reviewed` / `:set reviewed!` (or the bare `:reviewed`)
hide or show files already marked reviewed with `r`. There is deliberately no
single-key binding: `H` is a vim motion, and this is a command-only feature. Like
`i` / `e`, hidden files leave the tree, the diff pane, `{`/`}` and `[`/`]`
navigation, `/` search, and the `+/-` counts in the header — so the header reports
the diff still left to review.

Two things stay deliberately unaffected. The tree title keeps counting reviewed
files in its `reviewed/total` fraction, since scoping it to the visible rows would
collapse it to `0/n` exactly when progress matters most; the bottom border carries
a `reviewed hidden` cue instead. And a file whose hunks are individually marked
with `R` is not hidden — only the file-level `r` flag counts.

While hiding, `r` becomes a burn-down loop: marking the file you are reading moves
you to the next unreviewed file, wrapping at the end. A hidden file cannot be
un-reviewed, because `r` can no longer reach it — `:set reviewed` brings it back.
Start a session with them hidden via `show_reviewed = false` in `config.toml`.

### Search

`/` moves only the tree selection to the next matching path, expanding collapsed
parents as needed, and leaves the diff viewport where it was — press `Enter` to
jump the diff there. `n` / `N` step matches and wrap around. Search only ever
considers files that pass the active filters.

## Panel focus

| Key | Action |
|-----|--------|
| `Tab` / `Shift-Tab` | Cycle focus forward / backward between file list (or the changes window in its place), comment navigator, diff, and commit selector |
| `<leader>h` | Move focus one panel left (side-by-side: new side → old side → file list) |
| `<leader>l` | Move focus one panel right (side-by-side: file list → diff → old side → new side) |
| `<leader>k` | Move focus up (comments to files, or diff/files to commit selector when visible) |
| `<leader>j` | Move focus down (files to comments when visible, otherwise diff) |
| `<leader>e` | Toggle file list visibility |
| `<leader>s` | Toggle commit selector visibility (also `:set commits!`) |
| `<leader>g` | Go to the log pane: opens it (focused) when hidden, focuses it when open, hides it when it already has focus. `:log` / `:set log!` toggle visibility only |
| `<leader>t` | Go to the changes window (working tree status, in the file tree's place): opens it focused, focuses it, or hides it when it already has focus. `:changes` / `:status` do the same; `:set changes` / `:set nochanges` set visibility |
| `<leader>+` / `<leader>-` | Grow / shrink the log pane (also `:set logheight N`) |
| `<leader>b` | Open the branch picker (also `:branches`) |
| `<leader>F` | Find in files: opens the command line with `find ` from any pane (`f` does the same in the diff, file tree and changes window) |
| `<leader>a` | Toggle the blame gutter (also `:blame`, `:set blame!`) |
| `<leader>A` | Load the commit that last changed the cursor line as the review (also `:blame-commit`); works with the gutter off |
| `<leader><` / `<leader>>` | Move the side-by-side divider left / right (also `<leader>,` / `<leader>.`, `:set split N`, or drag the `│`) |
| `<leader>[` / `<leader>]` | Narrow / widen the file tree (also `:set treewidth N`, or drag its border) |
| `<leader>0` | Reset the divider to 50/50 and the file tree to 20% |
| `<leader>z` | Zen mode: hide the file tree and the log pane; again restores them (also `:zen`) |
| `<leader>w` | Toggle ignore-whitespace and reload the diff (also `:ignorews`, `:set ignorews` / `:set noignorews`) |
| `<leader>n` | Toggle relative line numbers (also `:set relativenumber!`) |
| `Enter` | Select file (when file list is focused) |

## Comment navigator

Shown below the file tree when local comments or visible remote PR threads exist.

| Key | Action |
|-----|--------|
| `j` / `k` | Move selection |
| `h` / `l` | Scroll rows left / right |
| `Enter` | Jump to selected comment |

## Review actions

| Key | Action |
|-----|--------|
| `r` | Toggle file reviewed |
| `R` | Toggle hunk reviewed |
| `c` | Add line comment (or file comment if not on a diff line) |
| `C` | Add file comment |
| `<leader>c` | Add review comment |
| `v` / `V` | Enter visual mode for range comments |
| `dd` | Delete comment at cursor |
| `i` | Edit comment at cursor (vim: text cursor at start) |
| `A` | Edit comment at cursor with text cursor at end (vim mode only) |
| `e` | Open focused file in `$EDITOR` |
| `y` | Copy review to clipboard |
| `Y` | Copy the comment at cursor to clipboard |

`e` opens the file at the cursor's line. Terminal editors (`vim`, `nvim`, `nano`, …)
take over the screen and tuicr reloads the diff once they exit. Windowed editors
(`code`, `cursor`, `zed`, `subl`, …) open in their own window while tuicr stays on
screen; reload with `:e` after editing. Adding `--wait` to the editor command opts a
windowed editor back into the blocking behaviour. Set the `editor` config key to
override `$EDITOR`.

Lines you change in the editor are highlighted in the diff, like an IDE's local
changes: a blue (violet in themes whose additions are already blue) row tint and a
`▌` in the sign column, with `▴` on the line that follows deleted lines. They are
measured against the file as it was just before tuicr first opened it in the
editor this session, so they keep accumulating across edits until `:clearedits`.
The status bar counts them, e.g. `Opened src/x.rs and reloaded 3 files · 4 lines
edited`. They show wherever the diff's new side is the working tree; a
commit-range review shows the commit instead, so `:local` switches it to the same
commits plus your working tree (and back). Theme keys `diff_edit` and
`diff_edit_bg` set the colours.

In PR review `e` opens the revision under review, not whatever the checkout
happens to hold. When the local file *is* that revision you get the real file and
your edits land in it; otherwise — wrong branch checked out, file added or deleted
by the PR, no checkout at all — tuicr writes a read-only copy of the PR's content
to a temp directory and opens that, so the text and the line the cursor sits on
match the diff. The status bar names which one you got, e.g.
`Opened src/main.rs @ 1a2b3c4 (read-only PR copy)`. Binary files are only opened
from the checkout, never copied.

## Visual mode

| Key | Action |
|-----|--------|
| `j` / `k` | Extend selection down / up |
| `c` / `Enter` | Create comment for selected range |
| `S` | Stage only the selected changed lines (working-tree and unstaged views); side-by-side rows stage both their deletion and addition |
| `U` | Unstage only the selected lines (staged view) |
| `s` | Start an editable suggestion prefilled with the selected code lines |
| `Esc` / `v` / `V` | Cancel selection |

Suggestions replace whole lines on the new side of a single diff hunk. Selections
across gaps (including deleted-only rows) or expanded context are rejected.
Unchanged context between added code blocks is included in the suggestion.
GitLab suggestions can include at most 100 lines above the final selected line.

## Comment mode

| Key | Action |
|-----|--------|
| `Tab` / `Shift-Tab` | Cycle comment type forward / backward (per `comment_types` order) |
| `Enter` / `Ctrl-Enter` / `Ctrl-s` | Save comment |
| `Shift-Enter` / `Ctrl-j` | Insert newline |
| `←` / `→` | Move cursor left / right |
| `↑` / `↓` | Move cursor up / down one displayed row (default editor) |
| `Ctrl-w` / `Alt-Backspace` / `Cmd-Backspace` | Delete word |
| `Ctrl-u` | Clear line |
| `Esc` / `Ctrl-c` | Cancel |

In the default editor, Up/Down move through wrapped rows and explicit newlines.
The cursor keeps its column where possible and restores it after crossing a
shorter row. Horizontal movement or editing sets the column for subsequent
vertical movement. Up/Down stop at the first or last row.

With `comment_vim = true` the box uses [`edtui`](https://github.com/preiter93/edtui)
modal editing (Normal/Insert/Visual: `hjkl`, `w`/`b`/`e`, `dd`/`D`/`ciw`/`x`,
`u`/`Ctrl-r`, visual `v`+`y`/`d`/`p`). From Normal mode `:w` (or `Enter` twice)
saves and `:q` (or `Esc`/`q` twice) cancels — the first press arms the action
and the header shows a confirm hint. `Alt-Enter` (Option+Enter) accepts and
`Alt-Esc` discards directly (no double-press) — Alt is the one modified
`Enter`/`Esc` that reaches the app across terminals, including browser/web
terminals like zellij web. `Tab` cycles the comment type in Normal
mode and inserts `comment_tab_width` spaces (default 4) in Insert mode; `Ctrl-s`
also saves.

## Commands

In command mode, `Tab` and `Shift-Tab` complete or cycle command names. After `:compare`,
`:checkout`, or `:checkout!`, they (and `↓` / `↑`, `Ctrl-n` / `Ctrl-p`) complete the branch name
being typed: prefix matches first, then names containing the text, local branches before
remote-tracking ones. `:compare ma`, `↓`, `↓`, `Enter` picks the second match.

| Command | Action |
|---------|--------|
| `:{N}` | Jump to new-side line N in current file |
| `:o{N}` | Jump to old-side line N in current file (matches deletions) |
| `:w` (`:write`) | Save session |
| `:e` (`:reload`) | Reload diff files |
| `:edit` | Open focused file in `$EDITOR` |
| `:local` | Compare the commit-range review with local: the same commits plus the working tree, so `$EDITOR` edits show; again goes back |
| `:clearedits` | Forget the lines edited in `$EDITOR` this session (drops their highlight) |
| `:clip` (`:export`) | Copy review to clipboard |
| `:copy-url` | Copy the open PR URL to clipboard (PR mode) |
| `:summary` | Show all pending local-draft comments; `j`/`k` select, `dd` deletes, and `Enter` jumps |
| `:diff` | Toggle diff view (unified / side-by-side) |
| `:theme` | Open the runtime theme picker (live preview; `/` filters, `Enter` applies and saves, `Esc` reverts) |
| `:theme <name>` | Apply and save `<name>` directly, without opening the picker |
| `:focus` (`:f`) | Toggle single-file view |
| `:stage` | Stage reviewed files (unstaged diffs only) |
| `:vim` (`:set vim!`) / `:novim` (`:set novim`) / `:set vim` | Toggle / disable / enable vim modal editing in the comment box (overrides `comment_vim`) |
| `:commits` (`:targets`) | Select commits to review |
| `:prs` | Open the review target selector on Pull Requests |
| `:sessions` (`:reviews`) | Resume a saved review for this checkout |
| `:merge` (`:conflicts`) | Open the side-by-side merge conflict view |
| `:submit` | Open submit picker (Comment / Approve / Request changes / Draft) |
| `:submit comment` | Submit a Comment review |
| `:submit approve` | Submit an Approve review |
| `:submit request-changes` | Submit a Request-changes review |
| `:submit draft` | Submit a Draft (pending) review |
| `:comments unresolved` | Show unresolved remote comments (PR mode, default) |
| `:comments all` | Show all remote comments including resolved and outdated |
| `:comments hide` | Hide remote comments in PR mode |
| `:set wrap` | Enable line wrap in diff view (off by default: long lines pan, and `‹` / `›` mark the clipped edges) |
| `:set wrap!` (`:wrap`) | Toggle line wrap in diff view |
| `:set split N` | Old side's share of the side-by-side width (15–85%); bare `:set split` resets to 50 |
| `:set treewidth N` | File tree width (8–60%) |
| `:zen` | Toggle zen mode (diff only) |
| `:set mouse!` | Toggle mouse capture for this session: off hands the mouse to the terminal, so a plain drag selects text; `:set mouse` / `:set nomouse` set it. Config `mouse` sets the start-up value |
| `:ignorews` / `:set ignorews!` | Toggle ignoring whitespace in the diff; `:set ignorews` / `:set noignorews` set it |
| `:set relativenumber` / `:set norelativenumber` | Enable / disable relative rendered-row numbers |
| `:set relativenumber!` | Toggle relative rendered-row numbers |
| `:set commits` | Show inline commit selector |
| `:set nocommits` | Hide inline commit selector |
| `:set commits!` | Toggle inline commit selector |
| `:log` / `:set log!` | Toggle the persistent log pane |
| `:set log` / `:set nolog` | Show / hide the persistent log pane |
| `:set logheight N` | Set the log pane height to N rows (borders included) |
| `:branches` / `:branch` | Open the branch picker |
| `:branch <name>` | Create `<name>` and switch to it (at the commit `n` was pressed on, else HEAD) |
| `:checkout <target>` | Check out a branch or commit; refuses when the working tree has changes |
| `:checkout! <target>` | Stash (untracked included), check out, and pop the stash |
| `:abort` | Abort the in-progress merge / cherry-pick / rebase / revert |
| `:revert`, `:reword`, `:drop`, `:fixup` | The log-cursor commit actions above, from the command line |
| `:reset [soft\|mixed\|hard] [commit]` | Reset the branch (default `mixed`; `hard` asks first); the commit defaults to the log cursor |
| `:undo` | Undo the last commit, keeping its changes staged (`reset --soft HEAD~1`) |
| `:tag <name> [commit]` | Lightweight tag at the commit (default: the one `t` was pressed on, else HEAD) |
| `:fetch` | Fetch every remote one by one (pruning, no submodules, never prompting) in the background; the status bar sums up what moved (`origin/main +3`) and names any remote that failed, such as one that wants credentials |
| `:pull` / `:pull!` | Pull the upstream: fast-forward only, or rebase onto it with `!` |
| `:push` | Show the commits that would be pushed and where, then push on `y` (the upstream, set on `origin` when missing). Refuses when the branch is behind its upstream. Never prompts for credentials |
| `:push!` | Force-push with `--force-with-lease` after the same preview, for history you rewrote (reword, drop, fixup); the lease refuses if the remote moved since your last fetch |
| `:merge <branch>` | Merge `<branch>` into the current branch (`--no-edit`). On conflicts, `:merge` opens the resolver, `:continue` finishes, `:abort` backs out |
| `:rebase <branch>` | Rebase the current branch onto `<branch>` |
| `:rebase -i [rev]` | Interactive rebase editor from `rev` (default: the log cursor) up to HEAD; `i` in the log pane does the same. See below |
| `:continue` / `:skip` | Finish the merge / rebase / cherry-pick / revert in progress once conflicts are staged; skip the commit a rebase stopped on |
| `:branch -d <name>` | Delete a branch; an unmerged one asks before `-D` |
| `:branch -m [old] <new>` | Rename a branch (the current one when `old` is left out) |
| `:upstream <remote/branch>` | Make the current branch track a remote branch |
| `:tag -d <name>` | Delete a local tag (asks first) |
| `:push --tags` / `:push-tags` | Push every tag to the default remote |
| `:console` | Git console: every git command tuicr ran, with exit status, time, and output. `m` toggles reads, `c` clears |
| `:commit` / `:commit <message>` | Commit the index; without a message the comment box opens as the message editor |
| `:commit!` / `:amend` / `:commit! <message>` | Amend HEAD (the box is pre-filled with its message) |
| `:unstage` | Unstage the file under the cursor |
| `:discard` | Discard every change to the file under the cursor (asks first) |
| `:stash` / `:stash <message>` | Stash the working tree, untracked files included |
| `:stash pop` | Restore the newest stash |
| `:stashes` / `:stash list` | Open the stash picker (`Enter` pops, `a` applies, `v` shows the stash's diff as the review, `d` drops) |
| `:find <text>` / `:grep <text>` | Search the working tree and show the hits in place of the diff |
| `:find` | Bring the last results back (or prompt when there are none) |
| `:compare <base> <other>` (`:select`) | Load `base...other` as the review (what `other` has since the merge base) and narrow the log pane to those two refs. When `other` has nothing beyond `base` the reverse is loaded and the status bar says so. `:compare <other>` compares the current branch; `:compare A..B` is used as typed; bare `:compare` opens the branch picker |
| `:blame` / `:annotate` / `:set blame!` | Toggle the blame gutter; `:set blame` / `:set noblame` set it |
| `:history` | Show the commits touching the file under the cursor in the log pane |
| `:set reviewed` | Show files already marked reviewed |
| `:set noreviewed` | Hide files already marked reviewed |
| `:set reviewed!` / `:reviewed` | Toggle files already marked reviewed |
| `:clear` | Clear all comments |
| `:clearc` | Clear comments without clearing reviewed marks |
| `:help` (`:h`) | Open the help screen |
| `:messages` | Open full details for the current error |
| `:version` | Show tuicr version |
| `:update` | Check for updates |
| `:q` (`:quit`) | Quit (warns on unsaved comments; discards review-only state) |
| `:q!` (`:quit!`) | Force quit |
| `:x` / `:wq` | Save and quit (prompts to copy if comments exist) |
| `ZZ` | Save and quit |
| `ZQ` | Quit without saving |
| `?` | Toggle help |

Pressing bare `q` no longer quits by default; it prints a reminder to use `:q` instead. Set
`q_quits = true` to restore `q` as a quit key in review modes.

The summary replaces the diff while leaving the file sidebar visible when it is open. The first
pending comment is selected when the summary opens. Use `j`/`k` to select the next
or previous comment; the view scrolls automatically to keep the selection visible. `Enter` returns
to the continuous diff and jumps to the selected comment, leaving single-file view if necessary,
while `Esc` returns without jumping. Reviewed files and hunks are revealed for the jump without
losing their reviewed state.
Press `dd` to delete the selected local draft, including comments whose original file or line
is absent from the current diff. Other comments are kept. Return with `Esc` and use `:w` to
save the deletion, following the existing diff-view deletion workflow.

The theme picker lists every bundled theme plus any local `*.toml` themes in the theme directory.
`j`/`k` move the selection and immediately repaint the UI with that theme (live preview) without
saving anything. `/` opens a filter prompt — type to narrow the list by substring, `Enter` applies
the filter and returns to navigation, `Esc` discards the filter edit without changing the list.
`Enter` on the picker itself keeps the currently previewed theme for the rest of the session. Like
other `:` toggles, the choice is not written to `config.toml`; set `theme = "<name>"` there to keep
it. `Esc` on the picker reverts the preview to whatever theme was active before it opened.

Not every forge supports every event:

| Event | GitHub | GitLab | Gitea | Bitbucket | Azure DevOps | Gerrit |
|---|---|---|---|---|---|---|
| `comment` | yes | yes | yes | yes | yes | yes |
| `approve` | yes | yes | yes | yes | yes (vote +10) | yes (vote +2) |
| `request-changes` | yes | yes | yes | no | yes (vote -10) | yes (vote -1) |
| `draft` | yes | yes | yes | no | yes (plain comment) | yes |

Bitbucket rejects `request-changes` and `draft` up front rather than silently downgrading them.
Azure DevOps has no pending-review primitive, so `draft` posts as a plain comment with no vote.
Gitea requires a review summary for `request-changes` and `draft`, and for `comment` when there
are no inline comments; inline comments alone do not satisfy it. GitLab `draft` creates draft
notes that the author publishes from GitLab's own "Submit review" UI. Gerrit `draft` stores draft
comments that the author publishes from Gerrit's Reply UI.

## Blame and history

`:blame` (or `<leader>a`) widens the line-number gutter and prefixes every new-side line
with `sha author age` for the commit that last touched it, like Annotate in an IDE. Blame
is read one file at a time starting with the file under the cursor, so a big diff fills in
over a moment; lines that are not committed yet stay blank. Commit-range reviews blame at the
range's newest commit, working-tree reviews at the working tree. Not available for PR reviews.

`:history` opens the log pane filtered to the commits that touched the file under the
cursor (`path:` filter); resting the cursor on one previews it, and `Esc` in the pane clears
the filter again.

## Find in files

`:find <text>` (or `f` / `<leader>F`, which pre-fill the command line) searches the working tree
with `rg`, falling back to `git grep` when ripgrep is not installed. The text is matched
literally, case-insensitively unless it contains an uppercase letter. Hits replace the diff:

| Key | Action |
|-----|--------|
| `j` / `k`, `g` / `G`, `Ctrl-d` / `Ctrl-u` | Move the selection |
| `Enter` | Jump to the hit when its file is in the loaded diff, otherwise open `$EDITOR` there |
| `e` | Open `$EDITOR` at the hit regardless |
| `/` | Start a new search |
| `Esc` / `q` | Back to the diff; `:find` brings the results back |

Results are capped at 2000 hits and the title says when that cap was reached.

## Local changes

In a working-tree review (the `Staged changes` / `Unstaged changes` rows, `-w`, or both), the
file tree and the diff act like a commit tool window:

| Key | Action |
|-----|--------|
| `s` / `u` | Stage / unstage the file under the cursor (the tree's selection when the tree is focused) |
| `S` | Stage the hunk under the cursor |
| `U` | Unstage the hunk under the cursor (from the staged view, where its line numbers are meaningful) |
| `X` | Discard the hunk under the cursor, after a `y`/`n` prompt |
| `:discard` | Discard every change to the file under the cursor, after a prompt (a new file is deleted) |
| `:commit`, `:commit!` | Commit / amend: the comment box becomes the message editor (`Enter` or `Ctrl-s` commits, `Esc` cancels) |
| `:stash`, `:stash pop`, `:stashes` | Stash, restore the newest stash, or open the stash picker (`Enter` pop, `a` apply, `d` drop) |

Hunk operations hand git the hunk's `@@` coordinates and the diff it came from, so they apply
exactly what is on screen. Untracked files can only be staged or discarded whole. The diff
reloads after every operation; when the review empties out (everything staged away from the
unstaged view) the pane goes blank until `:e` or the next change.

## Changes window

`<leader>t` (or `:changes`, `:status`) docks the working tree's status where the file tree
sits, like an IDE's Commit tool window: **Conflicts**, **Staged**, **Unstaged** and
**Untracked** groups, each entry with a checkbox (`▣` staged, `▢` not), git's status letter
and its path. Resting the cursor on an entry loads the review it belongs to (staged or
unstaged) and puts the diff cursor on the file, so the hunk and line keys (`S`, `U`, `V`
then `S`) keep working from the diff. `show_changes = true` in `config.toml` opens it at
startup; `<leader>e` closes it and brings the tree back.

Submodules are rows of their own, marked `S`, with their state after the name: `↑N` commits
the submodule has beyond the recorded one (`↓N` the other way), `modified` for changed
tracked files inside it, `untracked` for new files inside it. A dirty submodule's own
entries hang under its row, indented, with the same checkbox meaning inside that repo, and
every key on one of those rows runs git inside the submodule. Three levels of nesting are
walked.

| Key | Action |
|-----|--------|
| `j` / `k`, `g` / `G`, `Ctrl-d` / `Ctrl-u` | Move; the diff follows the entry after a short pause |
| `Enter` | Open the entry's diff and focus it; on a heading or a submodule with nested rows, fold / unfold; on a conflict, open the merge view |
| `Space` | The checkbox: stage an unstaged or untracked entry, unstage a staged one; fold a heading |
| `s` / `u` | Stage / unstage the entry (in its own worktree) |
| `a` / `A` | Stage everything (untracked included) / unstage everything in the cursor's worktree: the root from a root row or heading, a submodule from its rows |
| `X` | Discard the entry after a `y`/`n` prompt: a tracked file goes back to HEAD, a new or untracked file is deleted, a submodule row checks the submodule out on the commit the superproject records (its new commits stay in its reflog) |
| `c` / `C` | Commit / amend the cursor's worktree through the message box; from a submodule's rows the commit lands inside the submodule, after which the superproject shows the moved pointer to stage |
| `h` / `l` | Fold / unfold the heading or submodule under the cursor; `h` on a plain entry jumps to its heading or submodule row |
| `o` / `O` | Unfold / fold every group |
| `r` | Re-read the status (it also refreshes after every operation, history change and diff-watch tick) |
| `e` | Open the entry's file in `$EDITOR` |
| `Esc` | Return focus to the diff; `<leader>t` again hides the window |
| Click / wheel | Move the cursor (and open the entry) / scroll |

A nested file's diff is read through a backend opened on the submodule (the header names
it: `lib/sub · unstaged`); `s` / `u` in that diff stage whole files inside the submodule, and
hunk staging is not available there.

## Log pane

The log pane docks the commit graph above the diff and keeps it there while you review,
like an IDE's Git tool window. It opens on its own once a review is on screen (not for PR reviews,
plain-file reviews or `--all-files`), with the diff keeping focus; `<leader>g` (or `:log`)
focuses it, hides it, or brings it back. It replaces the inline commit selector while
open. Resting the cursor on a commit for a moment loads that commit's diff below; rows
whose diff is on screen carry the `▌` bar. Set `show_log = false` in `config.toml` to
start with it closed. It takes a third of the window (at most 12 rows) until you size it
with `<leader>+` / `<leader>-`, `:set logheight N`, or `log_height = N`.

| Key | Action |
|-----|--------|
| `j` / `k`, `g` / `G`, `Ctrl-d` / `Ctrl-u` | Move the cursor; the highlighted commit is previewed after a short pause |
| `Space` | Toggle the cursor commit in the pick set (non-contiguous picks allowed); on the overflow row, load more history |
| `Enter` | Load the picks (or the cursor commit) as the review; the picks must be one unbroken run of history, though rows of other branches may sit between them. Opens the merge view from the conflicts row |
| `p` | Cherry-pick the picks (or cursor commit) onto the current branch |
| `b` | Rebase the current branch onto the cursor commit |
| `s` | Squash the picks into one commit |
| `c` | Check out the cursor commit's branch (a remote-only tip `origin/fix` becomes a new local `fix` tracking it), or the commit itself (detached) |
| `n` | New branch at the cursor commit: opens `:branch ` for the name |
| `a` | Abort the in-progress merge / cherry-pick / rebase / revert |
| `B` | Open the branch picker |
| `v` | Revert the cursor commit (a new commit; conflicts open the merge view) |
| `R` | Reword the cursor commit: the message box opens pre-filled (amends HEAD, otherwise replays the later commits) |
| `D` | Drop the cursor commit, after a prompt |
| `f` | Fix up: squash the cursor commit into its parent, keeping the parent's message |
| `x` | Reset the branch to the cursor commit: opens `:reset ` for `soft`, `mixed` (default), or `hard` (asks first) |
| `t` | Tag the cursor commit: opens `:tag ` for the name |
| `y` | Copy the cursor commit's full hash |
| `F` | Fetch every remote (as `:fetch`), then refresh the log so the new remote-tracking commits show |
| `S` | Show one branch: opens the branch picker, and `Enter` narrows the log to the highlighted branch (a `ref:` filter; `Tab` first picks a second branch to show both). `Esc` in the log clears it |
| `/` | Filter the log (see below); `Esc` clears an active filter |
| `Tab` / `Shift-Tab`, `<leader>k` / `<leader>j` | Move focus between the pane and the other panels |
| `Esc` | Return focus to the diff |
| Click / wheel | Move the cursor (previews) / scroll |

### Filtering the log

`/` opens a one-line prompt in the log pane and on the selector's Local tab. Free words
match the commit message (all of them, case-insensitively), `author:NAME` matches the
author, `path:PATH` keeps commits that touch that path, `branch:NAME` walks only the
branches (local and remote-tracking) whose name contains NAME (repeatable), and `ref:NAME`
walks exactly that ref (repeatable; what the branch picker's `Ctrl-o` produces):

```
/fix retry author:kim path:src/net branch:release
```

The rows follow the prompt as you type: each pause of about 200ms searches in the
background and the title shows `…` while a search is running. `Enter` keeps the filter,
`Esc` while typing puts the rows back to the last kept filter, and `Esc` afterwards clears
it before its usual meaning. The title (or the selector footer) shows the filter and a
match count, and the overflow row keeps paging through the filtered history. Free words do
not match branch names; use `branch:` for that, and read the label chips on the rows.
Staged, unstaged, and conflicts rows stay visible while filtering.

## Branch picker

`<leader>b`, `B` in a selector, or `:branches` lists local branches (current marked `*`) and
remote-tracking branches, with each local branch's upstream and ahead/behind counts. Log rows
carry every ref as a chip: `[HEAD → main]` for the checked-out branch (`[HEAD]` when detached),
`[main]` local, `[origin/main]` remote in its own color (purple in most themes, also in the
picker), and `<v1.0>` tag. The header shows the current
branch's outgoing / incoming counts against its upstream (`git:main ↑2 ↓1`) and any merge,
rebase, cherry-pick, or revert in progress.

| Key | Action |
|-----|--------|
| any printable key | Filter branches by substring (case-insensitive) |
| `↑` / `↓`, `Ctrl-p` / `Ctrl-n` | Move the cursor |
| `Tab` | Pick / unpick the highlighted branch (`◆1`, `◆2`); a third pick replaces the oldest |
| `Ctrl-d` | Compare: two picks load `first...second` (what the second has that the first does not, or the reverse when that is empty) and narrow the log to the pair; one pick or none compares the current branch with it (or the cursor row) |
| `Ctrl-o` | Show the picked branches (or the cursor row) in the log pane, as a `ref:` filter |
| `Ctrl-f` | Fetch all remotes and refresh the list (ahead/behind counts update) |
| `Enter` | Check out the highlighted branch (a remote branch creates its local tracking branch) |
| `Ctrl-s` | Stash the working tree, check out, then pop the stash |
| `Ctrl-g` | Merge the highlighted branch into the current one |
| `Ctrl-b` | Rebase the current branch onto the highlighted one |
| `Ctrl-x` | Delete the highlighted local branch (an unmerged one asks first) |
| `Ctrl-r` | Rename the highlighted local branch (opens `:branch -m <name> `) |
| `Ctrl-t` | Make the current branch track the highlighted remote branch |
| `Backspace` / `Ctrl-u` | Edit / clear the filter |
| `Esc` | Clear the filter, then close |

A checkout refuses to run over a dirty working tree; `Ctrl-s` here or `:checkout! <target>`
stash first (untracked files included) and restore afterwards. If the pop conflicts, the stash
is kept and the status bar says so.

## Interactive rebase

`i` on a log commit (or `:rebase -i <rev>`) lists that commit and everything
above it on the current branch, oldest first, the order git applies them.

| Key | Action |
|-----|--------|
| `j` / `k` | Move the cursor |
| `J` / `K` (`Alt-↓` / `Alt-↑`) | Move the commit later / earlier |
| `p` | Pick |
| `r` | Reword: edit the subject in place (`Enter` keeps it, `Esc` cancels, `Ctrl-u` clears); the body is kept |
| `s` / `f` | Squash / fix up into the commit above it |
| `d` | Drop |
| `u` | Back to all picks in the original order |
| `Enter` | Run the rebase |
| `Esc` | Close without changing anything |

The footer says what a run would do (`reordered, 1 reworded, 1 dropped`) and
refuses a plan whose first kept commit squashes into nothing. Ranges containing
merges are refused. A conflict stops the rebase with `REBASE IN PROGRESS` in the
header: resolve with `:merge`, then `:continue`, `:skip`, or `:abort`.

## Commit selection / review target selector

The Local tab draws a commit graph across all local and remote-tracking branches, so commits
that so far exist only on `origin/…` show up too: each branch gets a colored track, with branch
chips on tips. `main` (or `master`) always takes the leftmost track, in the same colour, and
every other branch hangs off its right side; commits ahead of it sit one track over until
they meet it. The track follows first parents from the branch tip, and starts at
`origin/main` instead when that is ahead of the local branch, so freshly fetched commits
stay on it. When the two have diverged (commits not pushed yet and commits not pulled
yet), the local branch is drawn on top of `origin/main` in that same track: its own
commits are listed above the remote ones whatever their dates, and a side line from the
oldest of them runs down to the commit they were made on, so the gap you would rebase or
merge across is visible. A repository with neither branch lays its tracks out left to
right as they appear. The log pane draws the same graph. History loads as you move down past the last row, so older branches are a scroll
away rather than behind repeated "show more" presses. When the repository has unresolved
merge conflicts, a `· conflicts ·` row appears at the top — `Enter` on it opens
the merge conflict view.

History operations run on the current branch: `p` cherry-picks the selected
commits (or the cursor commit) onto it, `b` rebases it onto the cursor commit,
and `s` squashes a space-selected range into one commit (the range must be a
contiguous run ending at HEAD; messages are combined oldest-first). If an
operation stops on conflicts the merge conflict view opens automatically;
resolve and write, then run `git cherry-pick --continue` /
`git rebase --continue` from your shell.

The Local tab lists commits in date order (`git log --date-order`): newest
first, with children always above their parents so the graph stays coherent.

| Key | Action |
|-----|--------|
| `Tab` / `Shift-Tab` | Switch between Local, Pull Requests, and Sessions tabs |
| `j` / `k` | Move selection |
| `Space` | Toggle the commit under the cursor in the pick set (non-contiguous picks allowed — pull in single commits from other branches; consecutive presses still sweep a run) |
| `Enter` | Review the picked commits (must be one unbroken run of history: each pick the first parent of the one above it; rows of other branches listed in between are not gaps), open PR, load more PRs, resume a saved review, or open merge conflicts |
| `p` | Cherry-pick the picked (or cursor) commits onto the current branch — works with non-contiguous picks |
| `b` | Rebase the current branch onto the cursor commit |
| `s` | Squash the selected range (must end at HEAD) into one commit |
| `c` | Check out the cursor commit's branch (a remote-only tip `origin/fix` becomes a new local `fix` tracking it), or the commit itself (detached) |
| `n` | New branch at the cursor commit: opens `:branch ` for the name |
| `a` | Abort the in-progress merge / cherry-pick / rebase / revert |
| `B` | Open the branch picker |
| `v` / `R` / `D` / `f` / `x` / `t` / `y` | Revert / reword / drop / fixup / reset to / tag / copy hash of the cursor commit (as in the log pane) |
| `F` | Local tab: fetch every remote (as `:fetch`) and refresh the list |
| `S` | Local tab: show one branch, picked from the branch list (as in the log pane) |
| `/` | Local tab: filter the log by message / `author:` / `path:` (see the log pane section); Pull Requests tab: filter the loaded PR rows |
| `r` | In Pull Requests tab, toggle all open PRs / PRs requesting your review |
| `a` | In Sessions tab, show or hide saved reviews holding no progress |
| `dd` | In Sessions tab, delete the saved review under the cursor (asks to confirm) |
| `Esc` | Return to the diff |
| `:q` | Quit |

The Sessions tab lists saved reviews for the current checkout, so you can pick one
instead of retyping the commit range it was opened with. Rows show the review target,
comment count, reviewed files, and age. Reviews with no comments and no reviewed files
are omitted; press `a` to show them. Selecting a saved PR review re-fetches it from the
forge, the same as opening it from the Pull Requests tab.

Press `dd` to delete the saved review under the cursor. tuicr asks to confirm first, and
refuses to delete a review that is open in another tuicr, since that process would write
it straight back. Combine `a` with `dd` to clear out empty sessions left behind by a
crash, or by `tuicr review delete`/`clear` emptying a review another tuicr had open.

## Merge conflict view

Three-pane resolution for an in-progress merge (also cherry-pick/rebase
stops): **ours | result | theirs**. The outer panes are titled with the
branch and commit their content comes from (e.g. `ours · main · c713b11
Tune retry count`); the middle pane previews the resolved file as you pick.
Open it from the conflicts row in the selector or with `:merge`. Writing a
fully resolved file also stages it (the VCS “mark resolved” gesture);
unresolved conflicts are written back with their original markers.

| Key | Action |
|-----|--------|
| `j` / `k` | Scroll |
| `Ctrl-d` / `Ctrl-u`, `Ctrl-f` / `Ctrl-b` | Half-page / full-page scroll |
| `g` / `G` | Top / bottom |
| `n` / `N` (or `]` / `[`) | Next / previous conflict |
| `}` / `{` (or `Tab` / `Shift-Tab`) | Next / previous conflicted file |
| `o` | Take ours for the focused conflict |
| `t` | Take theirs |
| `b` | Take both (ours first) |
| `u` | Reset the focused conflict to unresolved |
| `e` | Write current picks, then open the file in `$EDITOR` at the focused conflict; on exit the view re-parses the file |
| `w` | Write the file; stages it once fully resolved |
| `a` | Abort the operation and restore the pre-merge state |
| `Esc` | Back (press twice to discard unsaved picks) |
| `q` | Quit |

Once the last conflict is written and staged, a **commit now?** prompt
appears: `y` commits the merge (or runs `cherry-pick/rebase/revert
--continue` with the prepared message), `n`/`Esc` returns to the view.

## Inline commit selector

Shown at the top of the diff when reviewing multiple commits. Focus it with `<leader>k` or `Tab`.
When opening a GitHub PR or GitLab MR you have reviewed before, tuicr may preselect only commits
newer than your latest submitted review; commits already covered by that review are marked with
`✓`. Use `Space` / `Enter` here to expand or adjust the range. Bitbucket does not record which
commit an approval covered, so no commits are preselected there.

| Key | Action |
|-----|--------|
| `j` / `k` | Navigate commits |
| `Space` / `Enter` | Toggle commit selection (updates diff) |
| `(` / `)` | Cycle through individual commits |
| `Esc` | Return focus to diff |

## Confirm dialogs

| Key | Action |
|-----|--------|
| `y` / `Enter` | Yes |
| `n` / `Esc` | No |

## Paste

A paste from the terminal (bracketed paste, e.g. `Ctrl-Shift-v` or `Cmd-v`) goes into
whichever text input is open: a comment, the `:` command line, `/` search, the log
filter, the file-tree prompts, the branch and theme pickers, the command palette, or a
reword in the rebase editor. With no input open it is dropped, so pasted text never runs
as keys. `Ctrl-v` itself is not bound: it used to start a visual selection, which is on
`v` / `V` only.

While git or a large reload keeps tuicr busy for more than a quarter of a second, the
bottom row shows a spinner with the git command that is running and how long it has
taken. It clears when the operation returns.

## Mouse

Mouse support is on by default. Disable with `mouse = false` in config, or for the
running session with `:set nomouse` (`:set mouse!` toggles). With it off the terminal
keeps the mouse: a drag selects text across the whole screen, and none of the actions
below apply.

| Action | Effect |
|--------|--------|
| Wheel up / down | Scroll the panel under the cursor (file list, comment navigator, diff, commit list, or help popup) without moving the cursor line |
| Click on a file | Jump to that file (lazygit-style) |
| Click on a directory | Expand or collapse it |
| Click on a diff line | Position the cursor on that line |
| Click on a commit | Toggle selection (or expand the row to load more) |
| Drag in diff | Highlight a range; press `y` to copy the selected source lines |

For full native terminal selection across the UI, hold your terminal's bypass modifier while dragging (usually **Shift** or **Option/Alt**, depending on the terminal), or switch capture off with `:set mouse!`.
