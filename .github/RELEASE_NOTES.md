# tscode prerelease

This prerelease delivers the first SSH-friendly VS Code-style TUI workspace.

## Highlights

- Mouse-first file explorer with real filesystem reads, expand/collapse, file open, hover, and wheel scrolling.
- Explorer copy, cut, paste, duplicate, and reveal-active-file actions backed by real filesystem operations.
- File moves keep open editor tab paths in sync and reveal pasted/duplicated targets in the tree.
- Command Palette with `F1` or `Ctrl-Shift-P`, fuzzy command matching, and actions for files, editor commands, explorer operations, focus changes, and terminal management.
- Quick Open overlay with `Ctrl-P` fuzzy matching across workspace file paths.
- Workspace text search overlay with `Ctrl-Shift-F` or `Ctrl-G`, real file scanning, result previews, and jump-to-line open.
- Editable tabbed code buffers with line numbers, syntax highlighting, dirty markers, cursor movement, paste, save, repeated search, undo, and redo.
- In-file search now highlights visible matches and shows a match count in the status bar.
- `Ctrl-H` and the command palette can replace the current/next active-file match, while replace-all changes every match as one undoable edit.
- Editor text selection with `Shift`+arrow keys and mouse drag, visual selection highlighting, and selection counts in the status bar.
- Internal editor clipboard support for `Ctrl-A`, `Ctrl-C`, `Ctrl-X`, and `Ctrl-V`, including replacing selected ranges as single undoable edits.
- Editor line commands for indent/outdent, duplicate line, delete line, move line up/down, toggle line comment, go to line, and save all.
- File explorer actions for refresh, new file, new folder, rename, and delete with confirmation; folder rename/delete keeps open tabs in sync.
- Explorer collapse-all is available through the command palette.
- Tab close support through the tab `x`, middle click, or `Ctrl-W`, with unsaved-buffer protection.
- File save preserves existing trailing newlines.
- Bottom integrated terminal panel backed by a real PTY shell with forwarded keyboard input, shell state, `Ctrl-C`, terminal scrollback, clear-terminal, and restart-terminal commands.
- Terminal ANSI rendering preserves parsed foreground/background colors plus bold, dim, italic, underline, and inverse styles.
- Terminal paste now honors bracketed paste mode when the child application requests it.
- Terminal modified navigation keys use xterm-compatible CSI sequences for better shell/editor behavior inside the PTY.
- Terminal clicks on visible existing `path:line:column` references open the file in the editor, while terminal apps that request mouse events receive those clicks and wheel events through the PTY.
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
