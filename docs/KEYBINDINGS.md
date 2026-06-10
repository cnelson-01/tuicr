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
| `/` | Search within diff |
| `n` / `N` | Next / previous search match |
| `Enter` | Expand or collapse hidden context between hunks |
| `zt` | Scroll cursor to top of screen |
| `zz` | Center cursor on screen |
| `zb` | Scroll cursor to bottom of screen |

## File tree

| Key | Action |
|-----|--------|
| `Space` | Toggle expand directory |
| `Enter` | Expand directory / jump to file in diff |
| `o` | Expand all directories |
| `O` | Collapse all directories |

## Panel focus

| Key | Action |
|-----|--------|
| `Tab` / `Shift-Tab` | Cycle focus forward / backward between file list, comment navigator, diff, and commit selector |
| `<leader>h` | Focus file list (left panel) |
| `<leader>l` | Focus diff view (right panel) |
| `<leader>k` | Move focus up (comments to files, or diff/files to commit selector when visible) |
| `<leader>j` | Move focus down (files to comments when visible, otherwise diff) |
| `<leader>e` | Toggle file list visibility |
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
| `i` | Edit comment at cursor |
| `y` | Copy review to clipboard |

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
| `:diff` | Toggle diff view (unified / side-by-side) |
| `:commits` | Select commits to review |
| `:merge` (`:conflicts`) | Open the side-by-side merge conflict view |
| `:submit` | Open submit picker (Comment / Approve / Request changes / Draft) |
| `:submit comment` | Submit a Comment review |
| `:submit approve` | Submit an Approve review |
| `:submit request-changes` | Submit a Request-changes review |
| `:submit draft` | Submit a Draft review (pending on GitHub) |
| `:set wrap` | Enable line wrap in diff view |
| `:set wrap!` | Toggle line wrap in diff view |
| `:set commits` | Show inline commit selector |
| `:set nocommits` | Hide inline commit selector |
| `:set commits!` | Toggle inline commit selector |
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
| `Space` | Toggle local commit selection |
| `Enter` | Confirm local commit range, open PR, load more PRs, or open merge conflicts |
| `p` | Cherry-pick selected/cursor commits onto the current branch |
| `b` | Rebase the current branch onto the cursor commit |
| `s` | Squash the selected range (must end at HEAD) into one commit |
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
| `Esc` | Back (press twice to discard unsaved picks) |
| `q` | Quit |

Once the last conflict is written and staged, a **commit now?** prompt
appears: `y` commits the merge (or runs `cherry-pick/rebase/revert
--continue` with the prepared message), `n`/`Esc` returns to the view.

## Inline commit selector

Shown at the top of the diff when reviewing multiple commits. Focus it with `<leader>k` or `Tab`.

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
