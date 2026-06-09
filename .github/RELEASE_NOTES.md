# tscode prerelease

This prerelease delivers the first SSH-friendly VS Code-style TUI workspace.

## Highlights

- Mouse-first file explorer with real filesystem reads, expand/collapse, file open, hover, and wheel scrolling.
- Explorer copy, cut, paste, duplicate, and reveal-active-file actions backed by real filesystem operations.
- File moves keep open editor tab paths in sync and reveal pasted/duplicated targets in the tree.
- Explorer filtering with `/`, dotfile visibility toggling with `.`, generated-folder visibility toggling with `i`, and file metadata/read-only markers in tree rows.
- Explorer rows now show Git working tree badges such as `git:M`, `git:?`, and dirty folder `git:*` when the workspace is inside a Git repository.
- Command Palette with `F1` or `Ctrl-Shift-P`, fuzzy command matching, and actions for files, editor commands, explorer operations, focus changes, and terminal management.
- Quick Open overlay with `Ctrl-P` fuzzy matching across workspace file paths.
- Workspace text search overlay with `Ctrl-Shift-F` or `Ctrl-G`, real file scanning, result previews, and jump-to-line open.
- Document Symbols with `Ctrl-Shift-O` lists functions, types, classes, interfaces, modules, and impl blocks from the active editor buffer and jumps to the selected symbol.
- Workspace Symbols with `Ctrl-T` scans visible workspace text files for code symbols, including dirty open buffers from memory, and jumps directly to the selected file location.
- Go to Definition with `Ctrl-]` or the command palette jumps from the editor cursor symbol to a matching workspace symbol definition, or lists multiple definition candidates when needed.
- Find References with `Ctrl-R` or the command palette lists whole-word workspace references for the editor cursor symbol and jumps directly to the selected occurrence.
- Replace in Files with `Ctrl-Shift-H` or the command palette scans real workspace text files, writes replacements to disk, updates clean open tabs, and skips dirty open buffers instead of overwriting unsaved work.
- Run Selection in Terminal with `Ctrl-Enter` or the command palette sends selected editor text, or the current editor line when there is no selection, to the active PTY shell and focuses the integrated terminal.
- Editable tabbed code buffers with line numbers, syntax highlighting, dirty markers, cursor movement, paste, save, repeated search, undo, and redo.
- Long editor lines now support horizontal scrolling with cursor tracking, mouse-click coordinate mapping, and horizontal wheel panning.
- In-file search now highlights visible matches and shows a match count in the status bar.
- `Ctrl-H` and the command palette can replace the current/next active-file match, while replace-all changes every match as one undoable edit.
- Editor text selection with `Shift`+arrow keys and mouse drag, visual selection highlighting, and selection counts in the status bar.
- Editor word movement and word selection with `Ctrl-Left`, `Ctrl-Right`, `Ctrl-Shift-Left`, and `Ctrl-Shift-Right` where the terminal reports modified arrow keys.
- Editor smart editing now preserves indentation on newline, adds one extra indent level after opening braces/brackets/parentheses, and splits immediate closing pairs onto their own line.
- Editor auto-pairs for brackets, braces, parentheses, quotes, apostrophes, and backticks support insertion, selection wrapping, skip-over, and paired Backspace deletion.
- Internal editor clipboard support for `Ctrl-A`, `Ctrl-C`, `Ctrl-X`, and `Ctrl-V`, including replacing selected ranges as single undoable edits.
- Editor copy/cut now also exports selected text through OSC52 terminal clipboard integration where the host terminal allows it.
- Command palette path-copy commands copy active-file or selected-explorer absolute/relative paths through the same terminal clipboard export without disturbing explorer file copy/cut state.
- Editor line commands for indent/outdent, duplicate, delete, move up/down, and toggle comments now work on selected line ranges as one undoable edit, while still supporting the current line when no selection is active.
- Format Document with `Shift-Alt-F` or the command palette pipes the active buffer through installed formatters such as `rustfmt`, `prettier`, `gofmt`, `black`, `shfmt`, or `clang-format`, then applies the result as one undoable dirty-buffer edit.
- Command palette Trim Trailing Whitespace removes spaces and tabs at line ends in the active editor buffer as one undoable edit, then saves cleanly to the real file when `Ctrl-S` or Save File is used.
- Command palette Revert File reloads the active editor tab from disk, discards unsaved buffer edits, clears per-tab edit history, resets the dirty marker, and refreshes Git status markers.
- Opening files now canonicalizes paths before tab lookup, avoiding duplicate tabs and broken relative-path behavior when the OS exposes aliases such as `/tmp` and `/private/tmp`.
- File explorer actions for refresh, new file, new folder, rename, and delete with confirmation; folder rename/delete keeps open tabs in sync.
- Explorer collapse-all is available through the command palette.
- Tab close support through the tab `x`, middle click, or `Ctrl-W`, with unsaved-buffer protection.
- File save preserves existing trailing newlines.
- Bottom integrated terminal panel backed by a real PTY shell with forwarded keyboard input, shell state, `Ctrl-C`, terminal scrollback, clear-terminal, and restart-terminal commands.
- Multiple integrated terminal sessions: `F7` creates a new PTY shell, `F8` switches to the next terminal, `F9` closes the active terminal, terminal tabs switch on click, tab close targets close on click or middle-click, and `+` creates a new terminal.
- New Terminal Here opens a real PTY shell in the selected explorer folder, or the selected file's parent directory, and restarting that terminal preserves its working directory.
- Terminal focus and maximize shortcuts now work from inside terminal focus too, so `F6`/``Ctrl-` `` and `F12`/`Ctrl-J` can move in and out of the integrated terminal without trapping the user in the PTY.
- Terminal ANSI rendering preserves parsed foreground/background colors plus bold, dim, italic, underline, and inverse styles.
- Terminal paste now honors bracketed paste mode when the child application requests it.
- Terminal modified navigation keys, function keys, Shift-Tab, null, and application-cursor arrows use xterm-compatible sequences for better shell/editor behavior inside the PTY.
- Terminal clicks on visible existing `path:line:column` references open the file in the editor when the shell is not using terminal mouse mode.
- Terminal apps that request mouse events now receive xterm-compatible mouse down, release, drag, move, and wheel events through the PTY instead of only basic click/wheel forwarding.
- Installer latest-version resolution now compares semantic prerelease tags so `pre.10` sorts after `pre.9` even when the GitHub API returns prereleases out of lexical order.
- Keyboard fallback for focus cycling, navigation, scrolling, command entry, and exit.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/hungryZoo/tscode/main/install.sh | sh
```

## Release assets

- macOS: `x86_64-apple-darwin`, `aarch64-apple-darwin`
- Linux GNU: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `armv7-unknown-linux-gnueabihf`
- Linux static tarballs: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`
- Linux packages: `.deb` and `.rpm` for GNU Linux targets
- Windows: `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`
