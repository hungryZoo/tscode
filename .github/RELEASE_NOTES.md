# tscode prerelease

This prerelease delivers the first SSH-friendly VS Code-style TUI workspace.

## Highlights

- Mouse-first file explorer with real filesystem reads, expand/collapse, file open, hover, and wheel scrolling.
- `tscode path/to/file` now opens the file immediately, uses its parent directory as the workspace root, reveals it in the explorer, and starts the integrated terminal in that parent directory.
- `tscode --help`/`--version` now print CLI metadata before entering raw mode, making installer and package verification work without an interactive terminal.
- Explorer sorting can now be changed by keyboard (`s`), command palette, or right-click context menu between name, type, modified time, and size while keeping folders first and preserving the selected path.
- Explorer copy, cut, paste, duplicate, and reveal-active-file actions backed by real filesystem operations.
- Explorer rows now support a right-click context menu whose mouse-selectable actions call the same real open/toggle, create, copy/cut/paste/duplicate, rename, delete, path-copy, New Terminal Here, refresh, collapse, and visibility operations as shortcuts and the command palette.
- Editor and terminal areas now support right-click context menus backed by the same real `CommandAction` path as shortcuts and the command palette. Editor menus cover save, clipboard, search, navigation, rename, suggestions, formatting, comments, run-in-terminal, path-copy, revert, and close-tab actions; terminal menus cover copy/paste/search, clear/restart/rename, session management, maximize/resize, and focus actions when the child app is not using terminal mouse reporting.
- File moves keep open editor tab paths in sync and reveal pasted/duplicated targets in the tree.
- Explorer filtering with `/` scans the real workspace tree, auto-expands collapsed parent folders for nested matches, preserves dotfile/generated-folder visibility rules, and keeps file metadata/read-only markers in tree rows.
- Explorer and workspace scans now respect `.gitignore`, `.ignore`, parent/global Git ignore rules, and generated-folder defaults through one shared visibility policy; toggling ignored entries with `i` reveals them in the explorer, quick open, workspace search, symbols, references, rename, and replace-in-files.
- Explorer now auto-refreshes when visible workspace files or folders are created, deleted, renamed, or have metadata changed by the integrated terminal, Git, build tools, or other external processes; expanded folders, selection, active filters, newly matching filtered paths, and Git badges are preserved/refreshed without a manual `r`.
- Explorer rows now show Git working tree badges such as `git:M`, `git:?`, and dirty folder `git:*` when the workspace is inside a Git repository.
- Command Palette with `F1` or `Ctrl-Shift-P`, fuzzy command matching, and actions for files, editor commands, explorer operations, focus changes, and terminal management.
- Quick Open overlay with `Ctrl-P` fuzzy matching across workspace file paths.
- Workspace text search overlay with `Ctrl-Shift-F` or `Ctrl-G`, real file scanning plus unsaved open-buffer scanning, result previews, dirty-buffer markers, and jump-to-line open.
- Document Symbols with `Ctrl-Shift-O` lists functions, types, classes, interfaces, modules, and impl blocks from the active editor buffer and jumps to the selected symbol.
- Workspace Symbols with `Ctrl-T` scans visible workspace text files for code symbols, including dirty open buffers from memory, and jumps directly to the selected file location.
- Code suggestions with `Ctrl-Space` or command palette Trigger Suggest use workspace symbols, identifier tokens, dirty open buffers, and file-type keywords, then insert the selected suggestion into the active editor as one undoable edit.
- Code folding now detects delimiter and indentation blocks, shows fold markers in the editor gutter, supports mouse gutter toggles, `Alt-[` toggle-fold, `Alt-]` unfold-all, command palette actions, and editor context-menu actions while keeping scroll and mouse coordinates mapped to visible lines.
- Editor symbol hover now resolves the identifier under the mouse against workspace symbols/references and shows definition/reference counts plus the first matching definition in a hover overlay and status summary.
- Go to Definition with `Ctrl-]` or the command palette jumps from the editor cursor symbol to a matching workspace symbol definition, or lists multiple definition candidates when needed.
- Find References with `Ctrl-R` or the command palette lists whole-word workspace references for the editor cursor symbol and jumps directly to the selected occurrence.
- Source Control from the command palette lists Git changed files and parsed diff hunks, supports fuzzy filtering, and jumps from a hunk row to the changed source line.
- Run Task with `Ctrl-Shift-B` or the command palette detects `.vscode/tasks.json`, `package.json` scripts, Cargo, Make, Go, and Python project tasks, filters them in a quick panel, and starts the selected task in a new real PTY terminal.
- Terminal search with terminal `Ctrl-F` or the command palette scans the active PTY terminal's visible screen and scrollback, highlights matches, shows `find:n/m` in the terminal header, and uses `F3`/`Shift-F3` to move between matches.
- Editor navigation history: `Alt-Left`, `Alt-Right`, or the command palette move backward and forward after quick-open, search, symbol, definition, reference, go-to-line, or terminal `path:line:column` jumps.
- Rename Symbol with `F2` or the command palette renames the identifier under the cursor across visible workspace text files, updates open buffers as undoable dirty edits, writes closed files to disk, and respects identifier boundaries so `make_client` does not rewrite `make_client_extra`.
- Replace in Files with `Ctrl-Shift-H` or the command palette scans real workspace text files, writes replacements to disk, updates clean open tabs, and skips dirty open buffers instead of overwriting unsaved work.
- Run Workspace Check from the command palette detects Cargo, Go, or Python projects, runs the project checker, collects parseable diagnostics into a Problems panel, filters those problems, and jumps directly to the source location.
- Collected workspace diagnostics now appear in the editor with severity gutter badges, subtle line backgrounds, active-file problem counts, and active-line status messages.
- Run Selection in Terminal with `Ctrl-Enter` or the command palette sends selected editor text, or the current editor line when there is no selection, to the active PTY shell and focuses the integrated terminal.
- Editable tabbed code buffers with line numbers, syntax highlighting, dirty markers, cursor movement, paste, save, repeated search, undo, and redo.
- New Untitled File with `Ctrl-N` or the command palette creates a real editable scratch tab without touching disk; Save File opens Save As, Save All reports dirty Untitled tabs, and Save As retargets the tab to the new file.
- Save As from the command palette writes the active editor buffer to a new relative or absolute path, creates parent folders, retargets the tab, refreshes explorer and Git status, and refuses dirty open target tabs.
- Open editor tabs now detect external disk changes while the app is running: clean tabs reload automatically after terminal/Git/tool writes, dirty tabs keep unsaved edits with `!`/status-bar conflict markers, deleted files show deleted-on-disk state, and Save File/Save All refuse accidental overwrites until Reload/Revert or Save As is chosen.
- Long editor lines now support horizontal scrolling with cursor tracking, mouse-click coordinate mapping, and horizontal wheel panning.
- In-file search now highlights visible matches and shows a match count in the status bar.
- `Ctrl-H` and the command palette can replace the current/next active-file match, while replace-all changes every match as one undoable edit.
- Editor text selection with `Shift`+arrow keys and mouse drag, visual selection highlighting, and selection counts in the status bar.
- Mouse multi-cursor editing: `Alt`+click inside the editor toggles extra cursors at clicked text positions, and typing/paste continues through every active cursor as one undoable edit.
- Multi-occurrence editor selection: `Ctrl-D` adds the next active-file occurrence of the current word or selection, `Ctrl-Shift-L` selects all active-file occurrences, and typing/paste/delete changes every selected occurrence as one undoable edit.
- Editor word movement and word selection with `Ctrl-Left`, `Ctrl-Right`, `Ctrl-Shift-Left`, and `Ctrl-Shift-Right` where the terminal reports modified arrow keys.
- Editor smart editing now preserves indentation on newline, adds one extra indent level after opening braces/brackets/parentheses, and splits immediate closing pairs onto their own line.
- Editor auto-pairs for brackets, braces, parentheses, quotes, apostrophes, and backticks support insertion, selection wrapping, skip-over, and paired Backspace deletion.
- Internal editor clipboard support for `Ctrl-A`, `Ctrl-C`, `Ctrl-X`, and `Ctrl-V`, including replacing selected ranges as single undoable edits.
- Editor copy/cut now also exports selected text through OSC52 terminal clipboard integration where the host terminal allows it.
- Command palette path-copy commands copy active-file or selected-explorer absolute/relative paths through the same terminal clipboard export without disturbing explorer file copy/cut state.
- Editor line commands for indent/outdent, duplicate, delete, move up/down, and toggle comments now work on selected line ranges as one undoable edit, while still supporting the current line when no selection is active.
- Format Document with `Shift-Alt-F` or the command palette pipes the active buffer through installed formatters such as `rustfmt`, `prettier`, `gofmt`, `black`, `shfmt`, or `clang-format`, then applies the result as one undoable dirty-buffer edit.
- Command palette Trim Trailing Whitespace removes spaces and tabs at line ends in the active editor buffer as one undoable edit, then saves cleanly to the real file when `Ctrl-S` or Save File is used.
- Command palette Revert File and Reload File From Disk reload the active editor tab from disk, discard unsaved buffer edits, clear per-tab edit history, reset dirty/external-file markers, and refresh Git status markers.
- Opening files now canonicalizes paths before tab lookup, avoiding duplicate tabs and broken relative-path behavior when the OS exposes aliases such as `/tmp` and `/private/tmp`.
- File explorer actions for refresh, new file, new folder, rename, and delete with confirmation; folder rename/delete keeps open tabs in sync.
- Explorer collapse-all is available through the command palette.
- Tab close support through the tab `x`, middle click, or `Ctrl-W`, with a mouse-selectable Save and Close / Don't Save / Cancel panel for dirty tabs, including Save As then close for Untitled tabs.
- File save preserves existing trailing newlines.
- Bottom integrated terminal panel backed by a real PTY shell with forwarded keyboard input, shell state, `Ctrl-C`, terminal scrollback, clear-terminal, restart-terminal commands, and a live header showing the active session, cwd, live/exited state, scrollback offset, and active terminal modes.
- Terminal cwd tracking now consumes OSC 7 current-directory reports and automatically hooks zsh/bash sessions so `cd` updates the terminal header, context menus, and restart-terminal working directory.
- Multiple integrated terminal sessions: `F7` creates a new PTY shell, `F8` switches to the next terminal, `F9` closes the active terminal, terminal tabs switch on click, tab close targets close on click or middle-click, and `+` creates a new terminal.
- New Terminal Here opens a real PTY shell in the selected explorer folder, or the selected file's parent directory, and restarting that terminal preserves its working directory.
- Rename Terminal changes the active terminal tab/header title without restarting the PTY shell or losing its cwd/session state.
- The normal integrated terminal panel can now be resized by hovering the highlighted top border and dragging it with the mouse; the resize keeps a usable terminal height and leaves editor space visible.
- Terminal focus and maximize shortcuts now work from inside terminal focus too, so `F6`/``Ctrl-` `` and `F12`/`Ctrl-J` can move in and out of the integrated terminal without trapping the user in the PTY.
- Full-screen terminal apps now receive terminal-owned keys before tscode shortcuts when alternate-screen or mouse-reporting modes are active, so keys such as `Ctrl-F`, `F3`, `F7`-`F9`, `F12`, `Ctrl-J`, and `Shift-PageUp/Down` reach tools like pagers, editors, and pickers inside the PTY.
- Terminal ANSI rendering preserves parsed foreground/background colors plus bold, dim, italic, underline, and inverse styles.
- Terminal paste now honors bracketed paste mode when the child application requests it.
- Terminal output selection now works inside the TUI: drag visible shell output to highlight cells and copy on release, use `Ctrl-Shift-C` to copy the active terminal selection again, and use `Ctrl-Shift-V` to paste the internal clipboard into the active PTY shell.
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
