# Keybindings

Full reference. Press `?` inside tuicr for an in-app version of this list.

`<leader>` defaults to `;`. Override it with `leader = ","` in `~/.config/tuicr/config.toml`.

## Navigation

| Key | Action |
|-----|--------|
| `j` / `↓` | Scroll down |
| `k` / `↑` | Scroll up |
| `h` / `←` | Scroll left |
| `l` / `→` | Scroll right |
| `Ctrl-d` / `Ctrl-u` | Half page down / up |
| `Ctrl-f` / `Ctrl-b` | Full page down / up |
| `g` / `G` | Go to first / last file |
| `{N}G` | Go to source line N in current file |
| `{N}{motion}` | Vim-style count prefix — repeats `j` / `k` / `h` / `l` / `{` / `}` / `[` / `]` `N` times |
| `{` / `}` | Jump to previous / next file |
| `[` / `]` | Jump to previous / next hunk |
| `m` / `M` | Jump to next / previous comment |
| `/` | Search within diff (case-insensitive); matches on diff content are highlighted and the status bar shows the `[current/total]` position (headers, comments, and PR info are searchable but not highlighted) |
| `n` / `N` | Next / previous search match (wraps around) |
| `Esc` | Clear search-match highlighting; the pattern is kept so `n` / `N` still work |
| `Enter` | Expand or collapse hidden context between hunks |
| `zt` | Scroll cursor to top of screen |
| `zz` | Center cursor on screen |
| `zb` | Scroll cursor to bottom of screen |

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
| `Tab` / `Shift-Tab` | Cycle focus forward / backward between file list, comment navigator, diff, and commit selector |
| `<leader>h` | Focus file list (left panel) |
| `<leader>l` | Focus diff view (right panel) |
| `<leader>k` | Move focus up (comments to files, or diff/files to commit selector when visible) |
| `<leader>j` | Move focus down (files to comments when visible, otherwise diff) |
| `<leader>e` | Toggle file list visibility |
| `<leader>s` | Toggle commit selector visibility (also `:set commits!`) |
| `<leader>g` | Toggle the persistent log pane (also `:log`, `:set log!`) |
| `<leader>+` / `<leader>-` | Grow / shrink the log pane (also `:set logheight N`) |
| `<leader>b` | Open the branch picker (also `:branches`) |
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
screen; reload with `:e` after editing. Adding `--wait` to `$EDITOR` opts a windowed
editor back into the blocking behaviour.

## Visual mode

| Key | Action |
|-----|--------|
| `j` / `k` | Extend selection down / up |
| `c` / `Enter` | Create comment for selected range |
| `Esc` / `v` / `V` | Cancel selection |

## Comment mode

| Key | Action |
|-----|--------|
| `Tab` / `Shift-Tab` | Cycle comment type forward / backward (per `comment_types` order) |
| `Enter` / `Ctrl-Enter` / `Ctrl-s` | Save comment |
| `Shift-Enter` / `Ctrl-j` | Insert newline |
| `←` / `→` | Move cursor |
| `Ctrl-w` / `Alt-Backspace` / `Cmd-Backspace` | Delete word |
| `Ctrl-u` | Clear line |
| `Esc` / `Ctrl-c` | Cancel |

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

In command mode,
`Tab` and `Shift-Tab` complete or cycle command names.

| Command | Action |
|---------|--------|
| `:{N}` | Jump to new-side line N in current file |
| `:o{N}` | Jump to old-side line N in current file (matches deletions) |
| `:w` | Save session |
| `:e` (`:reload`) | Reload diff files |
| `:edit` | Open focused file in `$EDITOR` |
| `:clip` (`:export`) | Copy review to clipboard |
| `:copy-url` | Copy the open PR URL to clipboard (PR mode) |
| `:summary` | Show all pending local-draft comments; `j`/`k` select and `Enter` jumps |
| `:diff` | Toggle diff view (unified / side-by-side) |
| `:vim` / `:novim` (`:set vim` / `:set novim`) | Enable/toggle/disable vim modal editing in the comment box (overrides `comment_vim`) |
| `:commits` | Select commits to review |
| `:merge` (`:conflicts`) | Open the side-by-side merge conflict view |
| `:submit` | Open submit picker (Comment / Approve / Request changes / Draft) |
| `:submit comment` | Submit a Comment review |
| `:submit approve` | Submit an Approve review |
| `:submit request-changes` | Submit a Request-changes review |
| `:submit draft` | Submit a Draft review (pending on GitHub) |
| `:set wrap` | Enable line wrap in diff view |
| `:set wrap!` | Toggle line wrap in diff view |
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
| `:set reviewed` | Show files already marked reviewed |
| `:set noreviewed` | Hide files already marked reviewed |
| `:set reviewed!` / `:reviewed` | Toggle files already marked reviewed |
| `:clear` | Clear all comments |
| `:clearc` | Clear comments without clearing reviewed marks |
| `:version` | Show tuicr version |
| `:update` | Check for updates |
| `:q` | Quit (warns on unsaved comments; discards review-only state) |
| `:q!` | Force quit |
| `:x` / `:wq` | Save and quit (prompts to copy if comments exist) |
| `ZZ` | Save and quit |
| `ZQ` | Quit without saving |
| `?` | Toggle help |
| `q` | Quick quit |

The summary replaces the diff while leaving the file sidebar visible when it is open. The first
pending comment is selected when the summary opens. Use `j`/`k` to select the next
or previous comment; the view scrolls automatically to keep the selection visible. `Enter` returns
to the continuous diff and jumps to the selected comment, leaving single-file view if necessary,
while `Esc` returns without jumping. Reviewed files and hunks are revealed for the jump without
losing their reviewed state.

`draft` applies to GitHub only. `comment` and `approve` work on GitHub, GitLab, and Bitbucket.
`request-changes` works on GitHub and GitLab, but not Bitbucket yet.

## Log pane

`<leader>g` (or `:log`) docks the commit graph above the diff and keeps it there while
you review, JetBrains-style. It replaces the inline commit selector while open. Resting
the cursor on a commit for a moment loads that commit's diff below; rows whose diff is
on screen carry the `▌` bar. Set `show_log = true` in `config.toml` to open it on
startup, and `log_height = N` (or `:set logheight N`) for its height.

| Key | Action |
|-----|--------|
| `j` / `k`, `g` / `G`, `Ctrl-d` / `Ctrl-u` | Move the cursor; the highlighted commit is previewed after a short pause |
| `Space` | Toggle the cursor commit in the pick set (non-contiguous picks allowed); on the overflow row, load more history |
| `Enter` | Load the picks (or the cursor commit) as the review; opens the merge view from the conflicts row |
| `p` | Cherry-pick the picks (or cursor commit) onto the current branch |
| `b` | Rebase the current branch onto the cursor commit |
| `s` | Squash the picks into one commit |
| `c` | Check out the cursor commit's branch, or the commit itself (detached) |
| `n` | New branch at the cursor commit: opens `:branch ` for the name |
| `a` | Abort the in-progress merge / cherry-pick / rebase / revert |
| `B` | Open the branch picker |
| `Tab` / `Shift-Tab`, `<leader>k` / `<leader>j` | Move focus between the pane and the other panels |
| `Esc` | Return focus to the diff |
| Click / wheel | Move the cursor (previews) / scroll |

## Branch picker

`<leader>b`, `B` in a selector, or `:branches` lists local branches (current marked `*`) and
remote-tracking branches, with each local branch's upstream and ahead/behind counts. Log rows
carry every ref as a chip: `[main]` local, dimmed `[origin/main]` remote, `[v1.0]` tag, and a
bold chip (or `[HEAD]` when detached) on the checked-out commit.

| Key | Action |
|-----|--------|
| any printable key | Filter branches by substring (case-insensitive) |
| `↑` / `↓`, `Ctrl-p` / `Ctrl-n` | Move the cursor |
| `Enter` | Check out the highlighted branch (a remote branch creates its local tracking branch) |
| `Ctrl-s` | Stash the working tree, check out, then pop the stash |
| `Backspace` / `Ctrl-u` | Edit / clear the filter |
| `Esc` | Clear the filter, then close |

A checkout refuses to run over a dirty working tree; `Ctrl-s` here or `:checkout! <target>`
stash first (untracked files included) and restore afterwards. If the pop conflicts, the stash
is kept and the status bar says so.

## Commit selection / review target selector

The Local tab draws a commit graph across all local branches: each branch gets a
colored track, with branch chips on tips. When the repository has unresolved
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
| `Tab` / `Shift-Tab` | Switch between Local and Pull Requests tabs |
| `j` / `k` | Move selection |
| `Space` | Toggle the commit under the cursor in the pick set (non-contiguous picks allowed — pull in single commits from other branches; consecutive presses still sweep a run) |
| `Enter` | Review the picked commits (must be a contiguous run), open PR, load more PRs, or open merge conflicts |
| `p` | Cherry-pick the picked (or cursor) commits onto the current branch — works with non-contiguous picks |
| `b` | Rebase the current branch onto the cursor commit |
| `s` | Squash the selected range (must end at HEAD) into one commit |
| `c` | Check out the cursor commit's branch, or the commit itself (detached) |
| `n` | New branch at the cursor commit: opens `:branch ` for the name |
| `a` | Abort the in-progress merge / cherry-pick / rebase / revert |
| `B` | Open the branch picker |
| `/` | Filter currently loaded PR rows locally |
| `r` | In Pull Requests tab, toggle all open PRs / PRs requesting your review |
| `q` / `Esc` | Quit / return |

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

## Mouse

Mouse support is on by default. Disable with `mouse = false` in config.

| Action | Effect |
|--------|--------|
| Wheel up / down | Scroll the panel under the cursor (file list, comment navigator, diff, commit list, or help popup) without moving the cursor line |
| Click on a file | Jump to that file (lazygit-style) |
| Click on a directory | Expand or collapse it |
| Click on a diff line | Position the cursor on that line |
| Click on a commit | Toggle selection (or expand the row to load more) |
| Drag in diff | Highlight a range; press `y` to copy the selected source lines |

For full native terminal selection across the UI, hold your terminal's bypass modifier while dragging (usually **Shift** or **Option/Alt**, depending on the terminal).
