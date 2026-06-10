# tscode prerelease

This prerelease delivers the first SSH-friendly VS Code-style TUI workspace.

## Highlights

- Mouse-first file explorer with real filesystem reads, expand/collapse, file open, hover, and wheel scrolling.
- `tscode path/to/file` now opens the file immediately, uses its parent directory as the workspace root, reveals it in the explorer, and starts the integrated terminal in that parent directory.
- `tscode --help`/`--version` now print CLI metadata before entering raw mode, making installer and package verification work without an interactive terminal.
- Open Folder from the command palette or explorer Open Folder as Workspace (`O`/right-click) now switches the current workspace root to another existing folder, rebuilds the explorer, and starts a fresh PTY terminal there while blocking dirty editor tabs.
- Explorer sorting can now be changed by keyboard (`s`), command palette, or right-click context menu between name, type, modified time, and size while keeping folders first and preserving the selected path.
- Explorer copy, cut, paste, duplicate, and reveal-active-file actions backed by real filesystem operations.
- Explorer New File/New Folder now prefill the selected folder path, create items under the selected folder or selected file's parent, reveal new folders, open new files, and refuse workspace-escaping paths.
- Explorer multi-select is now a real file-operation selection model: `Space` toggles the focused row, `Shift`+click range-selects visible rows, `Ctrl`/`Command`/`Meta`+click toggles rows, selected rows show a distinct marker/background, and copy/cut/paste/duplicate/delete/path-copy apply to the whole selected set.
- Explorer drag/drop now moves selected rows into the highlighted destination folder, while `Alt`+drag copies them instead; normal click-to-open still happens on release when no drag occurs.
- Explorer rows now support a right-click context menu whose mouse-selectable actions call the same real open/toggle, create, copy/cut/paste/duplicate, rename, delete, path-copy, New Terminal Here, refresh, collapse, and visibility operations as shortcuts and the command palette.
- Explorer Compare Selected Files with `v`, the command palette, or the right-click context menu now reads two selected text files from disk and opens a protected read-only unified diff tab.
- Explorer `Right`/`Left` keys now follow normal file-tree semantics: expand, descend into the first child, collapse, and move back to the parent instead of using a simple toggle-only behavior.
- Editor and terminal areas now support right-click context menus backed by the same real `CommandAction` path as shortcuts and the command palette. Editor menus cover save, clipboard, search, navigation, rename, suggestions, formatting, comments, run-in-terminal, path-copy, revert, and close-tab actions; terminal menus cover copy/paste/search, clear/restart/rename, session management, maximize/resize, and focus actions when the child app is not using terminal mouse reporting.
- File moves keep open editor tab paths in sync and reveal pasted/duplicated targets in the tree.
- Explorer filtering with `/` scans the real workspace tree, auto-expands collapsed parent folders for nested matches, preserves dotfile/generated-folder visibility rules, and keeps file metadata/read-only markers in tree rows.
- Explorer and workspace scans now respect `.gitignore`, `.ignore`, parent/global Git ignore rules, and generated-folder defaults through one shared visibility policy; toggling ignored entries with `i` reveals them in the explorer, quick open, workspace search, symbols, references, rename, and replace-in-files.
- Explorer now auto-refreshes when visible workspace files or folders are created, deleted, renamed, or have metadata changed by the integrated terminal, Git, build tools, or other external processes; expanded folders, selection, active filters, newly matching filtered paths, and Git badges are preserved/refreshed without a manual `r`.
- Explorer rows now show Git working tree badges such as `git:M`, `git:?`, and dirty folder `git:*` when the workspace is inside a Git repository.
- Command Palette with `F1` or `Ctrl-Shift-P`, fuzzy command matching, and actions for files, editor commands, explorer operations, focus changes, and terminal management.
- Prompt and quick-panel inputs now behave like editable command fields: `Left`/`Right`, `Home`/`End`, `Backspace`, `Delete`, `Ctrl-A`/`Ctrl-E`, `Ctrl-U`/`Ctrl-K`, and paste operate at the visible cursor instead of only appending text.
- Quick Open overlay with `Ctrl-P` fuzzy matching across workspace file paths.
- Binary, non-UTF-8, and very large files now open as protected read-only hex/ascii previews, and those previews are excluded from workspace search, replace, symbol scans, rename fallback, and completion token scans so original bytes cannot be rewritten accidentally.
- Workspace text search overlay with `Ctrl-Shift-F` or `Ctrl-G`, real file scanning plus unsaved open-buffer scanning, result previews, dirty-buffer markers, and jump-to-line open.
- Document Symbols with `Ctrl-Shift-O` asks an installed language server for `textDocument/documentSymbol` first, lists hierarchical or flat LSP symbols in a filterable quick panel, falls back to local extraction when needed, and jumps to the selected symbol.
- Sidebar Outline mode now turns the left pane into a persistent active-file symbol list via sidebar `m` or command palette Show Outline / Show Explorer Files / Toggle Sidebar Mode. Outline rows use LSP document symbols when available, fall back to local extraction when the server is absent, highlight on mouse hover, scroll with wheel/keyboard, and jump to symbols on click or `Enter` without losing the Outline view.
- Workspace Symbols with `Ctrl-T` asks an installed language server for `workspace/symbol` first, lists returned server symbols in the same filterable quick panel, falls back to visible workspace scans including dirty open buffers, and jumps directly to the selected file location.
- Optional installed-language-server integration now speaks stdio JSON-RPC/LSP for `textDocument/hover`, `textDocument/signatureHelp`, `textDocument/definition`, `textDocument/typeDefinition`, `textDocument/implementation`, `textDocument/prepareCallHierarchy`, `callHierarchy/incomingCalls`, `callHierarchy/outgoingCalls`, `textDocument/documentHighlight`, `textDocument/references`, `textDocument/rename`, `textDocument/completion`, `textDocument/documentSymbol`, `workspace/symbol`, `textDocument/formatting`, `textDocument/codeAction`, and `textDocument/publishDiagnostics`. Rust, Python, TypeScript/JavaScript, Go, and C-family files try common servers such as `rust-analyzer`, `pyright-langserver`, `typescript-language-server`, `gopls`, and `clangd`, while `TSCODE_LSP_COMMAND` can override the server for custom setups.
- Code suggestions with `Ctrl-Space` or command palette Trigger Suggest use installed LSP candidates when available plus workspace symbols, identifier tokens, dirty open buffers, and file-type keywords, then insert the selected suggestion into the active editor as one undoable edit.
- Code Action from the command palette or editor context menu asks the installed language server for quick fixes/refactors at the editor cursor, lists returned actions in a filterable quick panel, applies returned workspace edits, and executes command-only actions that produce `workspace/applyEdit` edits through `workspace/executeCommand`.
- Code folding now detects delimiter and indentation blocks, shows fold markers in the editor gutter, supports mouse gutter toggles, `Alt-[` toggle-fold, `Alt-0` fold-all, `Alt-]` unfold-all, command palette actions, and editor context-menu actions while keeping scroll and mouse coordinates mapped to visible lines.
- Editor bookmarks now use the leftmost gutter marker cell, `Alt-B`, command palette, and editor context-menu actions to toggle marked lines; `Alt-N`/`Alt-P`, Show Bookmarks, Next/Previous Bookmark, and Clear Bookmarks navigate or manage bookmarks across open tabs, with status-bar counts and line-edit position updates.
- Go to Matching Bracket with `Ctrl-Shift-\`, the command palette, or the editor context menu jumps between matching `()`, `[]`, and `{}` pairs in the active buffer, including nested pairs and after-bracket cursor positions, while preserving editor navigation history.
- Split Editor with `Ctrl-\`, the command palette, or the editor context menu renders side-by-side editor panes; Explorer `Ctrl-Enter`, the explorer context menu, or the command palette opens the selected file to the side, and mouse click/wheel events activate and scroll the pane under the pointer.
- Editor symbol hover now resolves the identifier under the mouse against workspace symbols/references and shows definition/reference counts plus the first matching definition in a hover overlay and status summary.
- Show Hover from the command palette or editor context menu asks the installed language server for hover documentation and displays it in a quick panel without leaving the TUI.
- Signature Help with `Ctrl-Shift-Space`, the command palette, or the editor context menu asks the installed language server for `textDocument/signatureHelp` and displays signatures, active parameter labels, and documentation in a filterable quick panel.
- Go to Definition with `Ctrl-]` or the command palette asks the installed language server first, then falls back to matching workspace symbol definitions or listing multiple definition candidates when needed.
- Go to Type Definition and Go to Implementation from the command palette or editor context menu ask the installed language server for `textDocument/typeDefinition` and `textDocument/implementation`, jump directly for one result, or list multiple server locations in a filterable quick panel.
- Show Incoming Calls and Show Outgoing Calls from the command palette or editor context menu ask the installed language server for Call Hierarchy results, list callers/callees in filterable quick panels, and jump to the selected caller call site or callee symbol.
- Highlight Symbol with `Ctrl-Shift-E`, the command palette, or the editor context menu asks the installed language server for `textDocument/documentHighlight`, paints text/read/write ranges in the active editor, shows the highlight count in the status bar, and clears stale ranges after buffer edits.
- Find References with `Ctrl-R` or the command palette asks the installed language server first, then falls back to whole-word workspace references for the editor cursor symbol and jumps directly to the selected occurrence.
- Run LSP Diagnostics from the command palette or editor context menu asks the installed language server for active-buffer diagnostics, opens the Problems panel, and feeds editor gutter/status markers.
- Source Control from the command palette lists Git changed files and parsed diff hunks, opens changed-file rows as read-only diff tabs, shows the current branch in the status bar, supports fuzzy filtering, can list/create/checkout local branches, stage/unstage, commit staged changes, commit all changes, or discard selected paths/all changes with confirmation, blocks branch/all-change/destructive actions when dirty editor buffers would be lost, and jumps from a hunk row to the changed source line.
- Run Task with `Ctrl-Shift-B` or the command palette detects `.vscode/tasks.json`, `package.json` scripts, Cargo, Make, Go, and Python project tasks, filters them in a quick panel, and starts the selected task in a new real PTY terminal.
- Run Active File in Terminal with `F5`, the command palette, or editor/explorer context menus now starts a new real PTY terminal in the file's folder and runs supported saved source files, while dirty, Untitled, unsupported, directory, or multi-selection inputs are blocked before any shell input is sent.
- Terminal search with terminal `Ctrl-F` or the command palette scans the active PTY terminal's visible screen and scrollback, highlights matches, shows `find:n/m` in the terminal header, and uses `F3`/`Shift-F3` to move between matches.
- Terminal Copy All Output and Scroll to Bottom commands now operate on the active PTY viewport plus retained scrollback from the command palette and terminal context menu.
- Run Terminal Command prompts for shell text and writes the submitted command to the active PTY; Run Recent Terminal Command opens a filterable picker of tscode-submitted commands from Run Command, Run Selection, Run Active File, and Run Task, then sends the selected command back into the active PTY.
- Editor navigation history: `Alt-Left`, `Alt-Right`, or the command palette move backward and forward after quick-open, search, symbol, definition, reference, go-to-line, or terminal `path:line:column` jumps.
- Rename Symbol with `F2` or the command palette asks the installed language server for a semantic `WorkspaceEdit` first, applies returned edits to open buffers as undoable dirty edits and closed files on disk, then falls back to visible workspace text-file rename with identifier boundaries.
- Replace in Files with `Ctrl-Shift-H` or the command palette scans real workspace text files, writes replacements to disk, updates clean open tabs, and skips dirty open buffers instead of overwriting unsaved work.
- Run Workspace Check from the command palette detects Cargo, Go, or Python projects, runs the project checker, collects parseable diagnostics into a Problems panel, filters those problems, and jumps directly to the source location.
- Collected workspace and language-server diagnostics now appear in the editor with severity gutter badges, subtle line backgrounds, active-file problem counts, and active-line status messages.
- Terminal file-reference clicks now recognize richer compiler, test, traceback, and stack-frame output including quoted paths, Python `File "path", line N` lines, and `path(line,column)` references.
- Terminal file-reference clicks now also recognize Node/Jest/TypeScript stack frames like `at fn (path:line:column)`, including paths with spaces inside the parentheses.
- Terminal link hover/click now recognizes `http://`, `https://`, and percent-encoded `file://...:line:column` URLs; web URLs are copied through the internal/OSC52 clipboard, and file URLs open directly in the editor.
- Run Selection in Terminal with `Ctrl-Enter` or the command palette sends selected editor text, or the current editor line when there is no selection, to the active PTY shell and focuses the integrated terminal.
- Editable tabbed code buffers with line numbers, syntax highlighting, dirty markers, cursor movement, paste, save, repeated search, undo, and redo.
- New Untitled File with `Ctrl-N` or the command palette creates a real editable scratch tab without touching disk; Save File opens Save As, Save All reports dirty Untitled tabs, and Save As retargets the tab to the new file.
- Save As from the command palette writes the active editor buffer to a new relative or absolute path, creates parent folders, retargets the tab, refreshes explorer and Git status, and refuses dirty open target tabs.
- Open editor tabs now detect external disk changes while the app is running: clean tabs reload automatically after terminal/Git/tool writes, dirty tabs keep unsaved edits with `!`/status-bar conflict markers, deleted files show deleted-on-disk state, and Save File/Save All refuse accidental overwrites until Reload/Revert or Save As is chosen.
- Reopen Closed Editor with `Ctrl-Shift-T`, the command palette, or the editor context menu restores the most recently closed editor tab, including dirty/Untitled buffers that were closed without saving and clean file view state.
- Close All Editors, Close Other Editors, and Close Editors to the Right are available from the command palette and editor context menu; they close clean target tabs, preserve dirty tabs, repair split-pane state, and feed the closed-editor stack for Reopen Closed Editor.
- Long editor lines now support horizontal scrolling with cursor tracking, mouse-click coordinate mapping, and horizontal wheel panning.
- Toggle Word Wrap with `Alt-Z`, the command palette, or the editor context menu wraps long visual lines to the pane width without changing file contents, while wheel, cursor visibility, hover, and mouse clicks map back to the correct buffer columns.
- Editor mouse drag selection now keeps the drag active after leaving the editor body, clamps the endpoint to the nearest code location, edge-scrolls the viewport, and feeds the same copy/cut/replace/run-selection paths as keyboard selection.
- Editor line-number gutter drag now selects complete logical lines, including upward drags, and routes the selected line range into the same real line commands such as delete, indent/outdent, comments, copy/cut, and run selection.
- In-file search now highlights visible matches and shows a match count in the status bar.
- `Ctrl-H` and the command palette can replace the current/next active-file match, while replace-all changes every match as one undoable edit.
- Editor text selection with `Shift`+arrow keys and mouse drag, visual selection highlighting, and selection counts in the status bar.
- Mouse multi-cursor editing: `Alt`+click inside the editor toggles extra cursors at clicked text positions, and typing/paste continues through every active cursor as one undoable edit.
- Multi-occurrence editor selection: `Ctrl-D` adds the next active-file occurrence of the current word or selection, `Ctrl-Shift-L` selects all active-file occurrences, and typing/paste/delete changes every selected occurrence as one undoable edit.
- Editor word movement and word selection with `Ctrl-Left`, `Ctrl-Right`, `Ctrl-Shift-Left`, and `Ctrl-Shift-Right` where the terminal reports modified arrow keys.
- Editor smart editing now preserves indentation on newline, adds one extra indent level after opening braces/brackets/parentheses, and splits immediate closing pairs onto their own line.
- Editor auto-pairs for brackets, braces, parentheses, quotes, apostrophes, and backticks support insertion, selection wrapping, skip-over, and paired Backspace deletion.
- Internal editor clipboard support for `Ctrl-A`, `Ctrl-C`, `Ctrl-X`, and `Ctrl-V`, including replacing selected ranges as single undoable edits.
- Editor `Ctrl-C` and `Ctrl-X` now copy or cut the current line when there is no active selection, including the trailing newline, so line clipboard behavior matches normal code editors.
- Editor copy/cut now also exports selected text through OSC52 terminal clipboard integration where the host terminal allows it.
- Command palette path-copy commands copy active-file or selected-explorer absolute/relative paths through the same terminal clipboard export without disturbing explorer file copy/cut state.
- Editor line commands for indent/outdent, duplicate, delete, move up/down, and toggle comments now work on selected line ranges as one undoable edit, while still supporting the current line when no selection is active.
- Toggle Block Comment with `Shift-Alt-A`, the command palette, or the editor context menu wraps/unwraps the active selection or current line using supported file-type block comment tokens such as `/* */`, `<!-- -->`, `--[[ ]]`, and Python triple quotes.
- Format Document with `Shift-Alt-F` or the command palette now tries installed LSP `textDocument/formatting` first, applies returned text edits as one undoable dirty-buffer edit, and falls back to external formatters such as `rustfmt`, `prettier`, `gofmt`, `black`, `shfmt`, or `clang-format`.
- Command palette Trim Trailing Whitespace removes spaces and tabs at line ends in the active editor buffer as one undoable edit, then saves cleanly to the real file when `Ctrl-S` or Save File is used.
- Command palette Revert File and Reload File From Disk reload the active editor tab from disk, discard unsaved buffer edits, clear per-tab edit history, reset dirty/external-file markers, and refresh Git status markers.
- Opening files now canonicalizes paths before tab lookup, avoiding duplicate tabs and broken relative-path behavior when the OS exposes aliases such as `/tmp` and `/private/tmp`.
- File explorer actions for refresh, new file, new folder, rename, and delete with confirmation; rename refuses to move the workspace root or overwrite an existing target, folder rename/delete keeps open tabs in sync, and explorer delete now refuses to discard unsaved dirty open tabs.
- Explorer collapse-all is available through the command palette.
- Tab close support through the tab `x`, middle click, or `Ctrl-W`, with a mouse-selectable Save and Close / Don't Save / Cancel panel for dirty tabs, including Save As then close for Untitled tabs.
- File save preserves existing trailing newlines.
- Bottom integrated terminal panel backed by a real PTY shell with forwarded keyboard input, shell state, `Ctrl-C`, terminal scrollback, clear-terminal, restart-terminal commands, and a live header showing the active session, cwd, live/exited state, child `exit:N` or `signal:NAME` status, scrollback offset, and active terminal modes.
- Terminal cwd tracking now consumes OSC 7 current-directory reports and automatically hooks zsh/bash sessions so `cd` updates the terminal header, context menus, and restart-terminal working directory.
- Terminal title tracking now consumes OSC 0/2 title reports from shells and terminal apps for unlocked sessions, while user-renamed terminal tabs stay locked to the chosen title.
- Terminal child apps that emit OSC52 clipboard writes now update tscode's internal clipboard and forward the copy request to the host terminal clipboard where supported.
- Multiple integrated terminal sessions: ``Ctrl-Shift-` ``/`F7` creates a new PTY shell, `F8` switches to the next terminal, `F9` closes the active terminal, terminal tabs switch on click, tab close targets close on click or middle-click, and `+` creates a new terminal.
- Split Terminal with `Ctrl-Shift-5`, the command palette, or the terminal context menu creates a side-by-side PTY pane from the active terminal's current working directory; clicking either pane focuses that shell, and closing one pane leaves the other shell alive.
- New Terminal Here opens a real PTY shell in the selected explorer folder, or the selected file's parent directory, and restarting that terminal preserves its working directory.
- Rename Terminal changes and locks the active terminal tab/header title without restarting the PTY shell or losing its cwd/session state.
- The normal integrated terminal panel can now be resized by hovering the highlighted top border and dragging it with the mouse; the resize keeps a usable terminal height and leaves editor space visible.
- Terminal focus and maximize shortcuts now work from inside terminal focus too, so `F6`/``Ctrl-` `` and `F12`/`Ctrl-J` can move in and out of the integrated terminal without trapping the user in the PTY.
- Full-screen terminal apps now receive terminal-owned keys before tscode shortcuts when alternate-screen or mouse-reporting modes are active, so keys such as `Ctrl-F`, `F3`, ``Ctrl-Shift-` ``/`F7`-`F9`, `F12`, `Ctrl-J`, and `Shift-PageUp/Down` reach tools like pagers, editors, and pickers inside the PTY.
- Terminal ANSI rendering preserves parsed foreground/background colors plus bold, dim, italic, underline, and inverse styles.
- Terminal paste now honors bracketed paste mode when the child application requests it.
- Terminal scrollback now follows normal terminal direction: wheel up and `Shift-PageUp` move into older output, while wheel down and `Shift-PageDown` return toward the live bottom.
- Terminal child apps that request mouse reporting still receive normal mouse input, but `Shift`+wheel and `Shift`+drag now deliberately bypass the child and control tscode's host scrollback/visible-output selection like a desktop terminal emulator.
- Terminal output selection now works inside the TUI: drag visible shell output to highlight cells and copy on release, use `Ctrl-Shift-C` to copy the active terminal selection again, and use `Ctrl-Shift-V` to paste the internal clipboard into the active PTY shell.
- Terminal modified navigation keys, function keys, Shift-Tab, null, and application-cursor arrows use xterm-compatible sequences for better shell/editor behavior inside the PTY.
- Terminal shell-editing shortcuts now forward more real terminal bytes, including `Alt-Backspace`, `Ctrl-Backspace`, `Alt-Enter`, `Alt-Tab`, and control punctuation/digits such as `Ctrl-/`, `Ctrl-6`, and `Ctrl-8`.
- Terminal clicks on visible existing `path:line:column` references open the file in the editor when the shell is not using terminal mouse mode.
- Terminal apps that request mouse events now receive xterm-compatible mouse down, release, drag, move, and wheel events through the PTY, including SGR, default, and UTF-8 coordinate encodings.
- Release CI now builds `aarch64-unknown-linux-gnu` with the Ubuntu `gcc-aarch64-linux-gnu` linker instead of the older cross Docker image path, avoiding a proc-macro resolution failure seen in the prerelease pipeline.
- Installer latest-version resolution now compares semantic prerelease tags so `pre.10` sorts after `pre.9` even when the GitHub API returns prereleases out of lexical order.
- Installer GitHub API and release-asset downloads now use retry, connection timeout, total timeout, and stalled-transfer detection so an idle CDN connection does not hang the install forever.
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
