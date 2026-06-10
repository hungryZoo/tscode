# tscode Implementation Status

This document tracks what is already implemented and what remains before tscode can be treated as a complete VS Code-style TUI.

## Current Release Policy

Until the product is closer to completion, prerelease CI builds only the Apple Silicon macOS target:

- `aarch64-apple-darwin`

The previous full matrix for macOS x86_64, Linux, Windows, and Raspberry Pi targets is intentionally paused to keep iteration fast. Full multi-platform release builds should be restored when explicitly requested.

## Implemented

### Runtime Stability

- The app restores raw mode, mouse capture, cursor visibility, and the alternate screen after an internal panic.
- Panic details and a backtrace are written to `~/.cache/tscode/crash.log`, or `$XDG_CACHE_HOME/tscode/crash.log` when `XDG_CACHE_HOME` is set.
- The main loop repairs invalid runtime state before rendering, including active terminal, split terminal, active editor tab, split editor, explorer selection, quick panel selection, and prompt cursor indexes.

### Shell and Integrated Terminal

- Real PTY-backed shell sessions with independent current working directories.
- Multiple terminal tabs with mouse click switching, close, create, restart, rename, and split-terminal support.
- ANSI color/style rendering through vt100 parsing.
- Keyboard forwarding for normal shell input, modified arrows, function keys, bracketed paste, shell editing keys, control punctuation, and terminal mouse encodings.
- Terminal scrollback, mouse wheel scrolling, Shift-scroll host override, terminal text selection, terminal output copy, search, and scroll-to-bottom.
- OSC 7 cwd tracking, OSC 0/2 title tracking, and OSC52 clipboard handling.
- Clickable file references and URLs in terminal output.
- Task/file/selection/run-command workflows that submit real shell text to PTY sessions.

### File Explorer

- Real filesystem tree with lazy directory loading, expand/collapse, open file, reveal active file, refresh, and automatic external-change refresh.
- Mouse hover, click, wheel, right-click context menus, drag/drop move, and Alt-drag copy.
- Multi-select with keyboard and mouse modifiers.
- Create file/folder, rename, delete confirmation, copy, cut, paste, duplicate, compare selected files, and New Terminal Here.
- Sort by name, type, modified time, and size.
- Hidden, ignored, and generated-folder visibility toggles using `.gitignore`, `.ignore`, global Git ignore, and built-in generated-folder policies.
- Git working tree badges in the explorer.
- File create/delete/rename prompts now render as an upper centered TUI dialog instead of replacing the bottom status line.

### Editor

- Editable tabbed buffers with line numbers, syntax highlighting, dirty markers, read-only binary/non-UTF-8/large-file previews, and save/save-as/save-all.
- Untitled editors that do not create placeholder files until Save As.
- Undo/redo, paste, selection, multi-cursor, multi-occurrence selection, word movement, line commands, line comments, block comments, trailing-whitespace trim, auto-pairing, and smart indentation.
- Find/replace in file, replace all, go to line, horizontal scrolling, word wrap, and mouse coordinate mapping.
- Code folding, fold all, unfold all, gutter fold clicks, and bookmarks.
- Matching-bracket navigation and editor navigation history.
- Split editor panes with independent mouse activation and scrolling.
- Open Editors panel for live tab switching, including dirty, read-only, externally changed, and Untitled tabs.
- External disk-change detection for open file-backed tabs.

### Workspace and Code Intelligence

- Quick Open, workspace search, replace in files, workspace symbols, document symbols, references, and fallback definition lookup over visible workspace text files.
- Optional one-shot stdio LSP support for hover, signature help, definition, type definition, implementation, call hierarchy, document highlight, references, rename, completion, document symbols, workspace symbols, formatting, code actions, and diagnostics.
- Problems panel and editor gutter/status diagnostics from workspace checks or language-server diagnostics.
- Source Control panel for Git changes, diff hunks, diff tabs, branch list/create/checkout, stage/unstage, commit staged, commit all, and discard actions.
- Task detection for `.vscode/tasks.json`, `package.json`, Cargo, Make, Go, and Python projects.

### UI and Input

- Mouse-first panel focus, hover highlighting, wheel scrolling, tab switching, context menus, prompt dialogs, and quick panels.
- Command palette for explorer, editor, workspace, Git, and terminal commands.
- Keyboard fallback for core navigation, focus movement, editor commands, terminal commands, and app exit.
- Top title bar and bottom status bar with active file, diagnostics, Git branch, hover, terminal, and clipboard state.

## Not Yet Complete

### Runtime Stability

- Crash reports identify panics after they happen, but there is no in-app crash report viewer yet.
- The state repair layer prevents common index panics, but long-running soak tests with scripted mouse/keyboard/PTY load are still needed.
- There is no persistent session restore after process exit or crash.

### Terminal Parity

- Terminal emulation is still based on vt100 parsing and does not claim full xterm/kitty/iTerm2 parity.
- Complex alternate-screen applications need more compatibility testing, especially nested TUIs, bracketed paste edge cases, and advanced private modes.
- There is no terminal profile selector, environment editor, shell integration UI, or terminal process tree.
- There is no terminal tab drag reordering.

### Explorer Parity

- There is no dedicated VS Code-style modal file operation confirmation with checkbox options such as recursive overwrite choices.
- File watching is polling/snapshot based rather than using native filesystem notification APIs.
- There is no tree item inline edit widget; rename/create use the shared dialog.
- There is no symbolic-link target inspection or advanced permission editor.

### Editor Parity

- There is no minimap, breadcrumbs, sticky scroll, inline diff, peek definition, peek references, or inline rename widget.
- There is no full TextMate grammar ecosystem or theme selection UI.
- There is no settings UI for editor preferences, keybindings, color theme, font, or language behavior.
- Multi-root workspaces are not implemented.
- Large-file editing is intentionally blocked through read-only previews rather than streaming editable chunks.

### Language Features

- LSP integration is one-shot per command rather than a long-lived synchronized language-server session.
- There is no semantic tokens rendering, inlay hints, code lens, folding ranges from LSP, workspace-wide diagnostics streaming, or background indexing.
- Completion is a quick-panel picker rather than an inline suggest widget with documentation side panes.
- Debug Adapter Protocol, breakpoints, variables, watch, call stack, and debug console are not implemented.

### Source Control and Extensions

- Source Control covers core Git flows but lacks side-by-side diff review, merge conflict resolution UI, stash, remote sync, blame, timeline, and PR integration.
- There is no extension host, extension marketplace, plugin API, or VS Code settings/keybinding compatibility layer.

### Release and Install

- Current prereleases intentionally build only `aarch64-apple-darwin`.
- Linux `.deb`/`.rpm`, Windows, Raspberry Pi, and macOS x86_64 artifacts were previously validated but are paused until full-matrix release is requested again.
- The one-line installer is still present, but non-Apple-Silicon users may need an older full-matrix prerelease while Apple-only prereleases are being published.
