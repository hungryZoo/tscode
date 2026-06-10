# tscode Software Requirements Specification

## 1. Scope

This document defines verifiable software requirements for the `tscode` prerelease.

## 2. Runtime Requirements

### R-001 Startup

The application shall start from `tscode [path]`.

### R-002 Workspace Path

If a directory path is provided, the application shall use that directory as the workspace root. If a file path is provided, the application shall use the file's parent directory as the workspace root, open the file in an editor tab, reveal it in the explorer, and start the integrated terminal in that parent directory. If omitted, it shall use the current working directory.

### R-002a CLI Metadata

`tscode --help`, `tscode -h`, `tscode --version`, and `tscode -V` shall print metadata without entering raw mode or the alternate-screen TUI. `tscode -- <path>` shall allow a path that begins with `-`.

### R-003 Terminal Safety

The application shall enter raw mode, use an alternate screen, enable mouse capture, and restore the terminal on normal exit or panic.

## 3. File Explorer Requirements

### R-101 Real Filesystem

The explorer shall read actual filesystem entries from the workspace root.

### R-102 Sorting

The explorer shall show directories before files. By default, each group shall be sorted case-insensitively by name.

The explorer shall allow the user to change sorting to name, type, modified time, or size through the keyboard, command palette, and explorer context menu. Type sorting shall group files by extension and then name. Modified-time sorting shall put newer entries before older entries within the directory/file group. Size sorting shall put larger files before smaller files within the directory/file group. Changing sort mode shall preserve the currently selected path when that path remains visible.

### R-103 Expand and Collapse

Clicking or pressing `Enter` on a directory shall toggle expanded state.

### R-103A Explorer Tree Arrow Semantics

When explorer focus is active, pressing `Right` on a collapsed directory shall expand it without moving selection, pressing `Right` on an expanded directory shall move selection to its first visible direct child when one exists, and pressing `Right` on a file shall open it in the editor. Pressing `Left` on an expanded directory shall collapse it without moving selection; pressing `Left` on a collapsed directory or file shall move selection to the nearest visible ancestor directory when one exists.

### R-104 File Open

Clicking or pressing `Enter` on a file shall open it in the editor.

### R-105 Scrolling

Mouse wheel and keyboard navigation shall scroll the explorer when content exceeds viewport height.

### R-106 Hover

The explorer shall visually highlight the row under the mouse cursor.

### R-106A Multi-Selection

The explorer shall support a visible multi-selection model. Pressing `Space` on the focused explorer row shall toggle that row in the selection set. `Ctrl`, `Command`, or `Meta` left-clicking an explorer row shall toggle that row without opening it. `Shift` left-clicking an explorer row shall select the visible row range between the selection anchor and the clicked row. Multi-selected rows shall be visually distinct from ordinary hovered rows and from the active cursor row. Pressing `Esc` outside terminal focus shall clear the explorer multi-selection before invoking app quit behavior.

### R-106B Create and Rename Safety

New File and New Folder shall prompt with the selected folder, or the selected file's parent folder, prefilled as the workspace-relative target prefix. Simple names shall be created under that selected base; explicit relative paths shall be resolved under the workspace root; parent-directory traversal and workspace-escaping absolute paths shall be rejected. Creating a new file shall open the created file in the editor, and creating a new folder shall reveal the created folder in the explorer. Explorer rename shall operate on one item at a time, shall reject workspace-root rename, shall require a single file or folder name rather than a path, and shall refuse to overwrite an existing target.

### R-106C Explorer Drag and Drop

Dragging an explorer row shall start a file-operation drag without opening the row until the mouse is released. Releasing without moving shall keep the existing click behavior: files open and directories toggle. Releasing after dragging over a folder row shall move the dragged selected set into that folder, and releasing over a file row shall use that file's parent folder as the destination. Holding `Alt` during the drag shall copy rather than move. The current drop-target row shall be visually highlighted while dragging. The operation shall use the same multi-selection normalization as explorer copy/cut/paste, shall refuse to drag the workspace root, shall refuse dropping a directory into itself or its descendants, shall avoid overwriting by choosing a unique copy-style destination name when needed, shall update open editor tab paths and navigation history after moves, shall refresh the explorer, and shall reveal the final destination.

### R-107 Open Tab Synchronization

Renaming a file or folder shall update matching open tab paths. Deleting one or more files or folders shall close matching clean open tabs. If any delete target contains a dirty open file-backed tab, the application shall refuse the filesystem delete, keep all matching tabs open, keep the dirty buffer contents intact, and tell the user to save, close, or discard the unsaved tab first.

### R-108 Explorer Clipboard

The explorer shall support copying and cutting the selected file, folder, or active multi-selection set, pasting it into the selected directory or selected file's parent directory, and recursively copying folder contents. When a folder and its descendant are both selected, batch file operations shall process the top-level selected folder once rather than duplicating descendant work.

### R-109 Duplicate

The explorer shall duplicate the selected file, folder, or active multi-selection set beside each original using non-conflicting copy names.

### R-110 Reveal Active File

The explorer shall reveal the active editor file by expanding parent folders and selecting the file row.

### R-110A Open Folder as Workspace

The command palette shall provide an Open Folder action that accepts an existing relative or absolute directory path. The explorer shall provide Open Folder as Workspace for the selected folder through `O` and the right-click context menu. Opening a different folder shall replace the workspace root, rebuild the explorer from the real filesystem, close existing clean tabs, clear split-editor, search, quick-panel, navigation, terminal-selection, problem, source-control, and explorer-operation state, and create one new integrated terminal rooted in the new workspace. The action shall refuse non-directory targets, missing paths, and any open dirty file-backed or Untitled editor tab so unsaved edits are not silently lost.

### R-111 Explorer Visibility Controls

The explorer shall support a visible-tree text filter, dot-prefixed entry visibility toggling, and generated-folder visibility toggling for common generated directories such as `target`, `dist`, `build`, and `node_modules`.

When an explorer filter is applied, matching nested files or folders shall be discovered from the real workspace filesystem even if their parent folders are currently collapsed, and those parent folders shall be expanded enough to display the matches.

### R-112 Explorer Metadata

The explorer shall show available file size metadata and read-only markers for filesystem entries.

### R-113 Explorer Git Status

When the workspace is inside a Git repository, the explorer shall show compact Git status markers for changed files and dirty parent folders. Status markers shall refresh after explorer refreshes and editor saves.

### R-114 External Explorer Refresh

The explorer shall detect externally-created, externally-deleted, renamed, and metadata-changed visible workspace entries while the app is running. When such a change is detected, the explorer shall refresh from the real filesystem without requiring a manual refresh command, preserve expanded folders and the current selection when possible, keep active explorer filters applied, expand newly matching filtered paths, and refresh Git status markers without overwriting unrelated status messages.

### R-115 Explorer Context Menu

Right-clicking an explorer row shall select that row and open a mouse-selectable context menu. The menu shall expose actions for open/toggle, open to side, Open Folder as Workspace for folders, Run File in Terminal for supported files, new file, new folder, copy path, copy relative path, copy, cut, paste, duplicate, compare selected files, rename, delete, New Terminal Here, refresh, collapse folders, sort mode selection, toggle hidden files, and toggle generated folders. When a multi-selection is active, applicable copy, cut, duplicate, delete, and path-copy actions shall apply to the whole selected set, compare selected files shall be available for exactly two selected regular files, and rename shall remain a single-item action. Activating a menu item shall call the same real filesystem, terminal, or explorer operation used by keyboard shortcuts and the command palette.

### R-116 Ignore-Aware Workspace Visibility

When ignored/generated entries are hidden, the explorer, quick open, workspace search, workspace symbols, go-to-definition, find-references, rename-symbol, and replace-in-files scans shall apply a shared workspace visibility policy. That policy shall respect `.gitignore`, `.ignore`, parent/global Git ignore files, and built-in generated folders such as `target`, `node_modules`, `dist`, and `build`. The `i` explorer toggle and matching command palette action shall reveal those ignored/generated entries and make them available to the same workspace scans.

### R-117 Compare Selected Files

Explorer `v`, the command palette, and the explorer context menu shall include a Compare Selected Files action. The action shall require exactly two selected regular files, read their current on-disk contents, reject binary, non-UTF-8, or oversized inputs without modifying either file, and open a read-only editor tab containing a unified diff. Repeating the same comparison shall reuse and refresh the same synthetic compare tab. Save, edit, replace, format, rename, and workspace mutation commands shall treat the compare tab as read-only.

### R-118 Sidebar Outline

The left sidebar shall support Files and Outline modes. Files mode shall show the real filesystem explorer. Outline mode shall show symbols for the active editor buffer. Pressing `m` while the sidebar is focused and command palette actions Show Outline, Show Explorer Files, and Toggle Sidebar Mode shall switch modes. Outline mode shall request installed language-server document symbols when available and shall fall back to local active-buffer symbol extraction when the language server is absent, cannot start, returns no symbols, or the active buffer is dirty/stale relative to a cached symbol response. Outline rows shall visually highlight on mouse hover, support wheel and keyboard scrolling, move selection with arrow and page keys, and jump the editor cursor to the selected symbol on mouse click or `Enter` while preserving Outline mode.

## 4. Editor Requirements

### R-201 Tabs

The editor shall support multiple open file tabs.

### R-202 Tab Selection

Clicking a tab shall make it active.

### R-203 File Reading and Editing

The editor shall read UTF-8 text files and render their contents. Invalid UTF-8 shall be lossily decoded. The active editor buffer shall support text insertion, deletion, newline, cursor movement, and save.

### R-204 Line Numbers

The editor shall show line numbers for opened files.

### R-205 Syntax Highlighting

The editor shall apply syntax highlighting based on file extension or filename when a syntax definition is available.

### R-206 Scrolling

Mouse wheel and keyboard shortcuts shall vertically scroll the active file. Long lines shall support horizontal scrolling, cursor tracking, and mouse coordinate mapping against the current horizontal offset.

### R-207 Hover

Tabs shall visually highlight on mouse hover.

### R-208 Save

`Ctrl-s` in editor focus shall write a file-backed active buffer to its file path and clear dirty state. If the active buffer is Untitled, `Ctrl-s` shall open the Save As prompt instead of writing a placeholder file.

### R-208A Save As

The command palette shall include a Save As action for the active editor tab. The action shall accept a relative path resolved under the workspace root or an absolute path, create missing parent directories, write the current in-memory editor buffer to the target path, retarget the active tab's path and title to the saved file, clear dirty state, refresh explorer and Git status markers, and reveal the saved path when it is inside the workspace. The action shall refuse to overwrite a target that is already open in another dirty editor tab.

### R-208B Untitled Editor Buffers

`Ctrl-n` or the command palette shall create a new editable Untitled editor tab without creating a filesystem placeholder. Untitled tabs shall participate in normal editing, selection, undo/redo, search, document symbols, and tab close dirty-buffer protection. Save File on an Untitled tab shall prompt for Save As. Save All shall skip dirty Untitled tabs and report that Save As is required. After Save As succeeds, the tab shall become a normal file-backed tab with the saved path, title, dirty state, explorer reveal, and Git status refresh.

### R-208C Editor Context Menu

Right-clicking the editor body or an editor tab shall focus the editor and open a mouse-selectable context menu. Right-clicking the editor body shall update the editor cursor to the clicked buffer location before opening the menu. The menu shall expose actions for save, copy, cut, paste, select all, find, replace, go to line, language-server hover, signature help, symbol highlights, go to definition, call hierarchy, find references, code action, rename symbol, trigger suggest, format document, fold/unfold, toggle line comment, run selection/current line in terminal, run saved active file in terminal, copy absolute file path, copy relative file path, revert file, close active tab, close other editors, close editors to the right, close all editors, and reopen closed editor. Activating a menu item shall call the same editor, workspace, terminal, or clipboard command used by keyboard shortcuts and the command palette.

### R-208D Dirty Tab Close Confirmation

Closing a clean editor tab through `Ctrl-w`, the tab close target, middle-click, the editor context menu, or the command palette shall close it immediately. Closing a dirty editor tab through the same entry points shall open a mouse-selectable quick panel with Save and Close, Don't Save, and Cancel actions. Save and Close shall write file-backed tabs before closing them; for Untitled tabs it shall prompt for Save As and close the tab after the target file is written. Don't Save shall close the tab without modifying the backing file or creating an Untitled placeholder. Cancel shall leave the dirty tab open.

### R-208E Binary and Large File Safety

Opening a file that contains NUL bytes, is not valid UTF-8 text, or exceeds the configured editable-size limit shall create a read-only editor tab containing a bounded hex/ascii preview instead of a writable text buffer. Editing, Save, Save As, formatting, rename, replace, workspace search, and workspace replace shall not treat that preview as writable source text for the original file.

### R-208F Editor Split

The editor shall support a side-by-side split view when at least one editor tab is open. `Ctrl-\`, the command palette, and the editor context menu shall split the active editor tab into a side pane. Explorer `Ctrl-Enter`, the explorer context menu, and the command palette shall open the selected explorer file to the side while preserving the previously active editor tab in the other pane. Each rendered editor pane shall have its own hit region. Clicking, dragging, or wheeling over a split pane shall activate that pane before applying cursor, selection, fold, vertical-scroll, or horizontal-scroll behavior. Closing Editor Split shall return to a single-pane editor view without closing tabs or mutating file buffers. Closing tabs shall clear or retarget stale split-pane indices so the split state cannot point at a removed tab.

### R-209 Search

`Ctrl-f` in editor focus shall prompt for text and move the cursor to the next match when found.

### R-210 Repeated Search

`F3` shall move to the next match for the active search text. `Shift-F3` shall move to the previous match.

### R-211 Undo and Redo

`Ctrl-z` and `Ctrl-y` in editor focus shall undo and redo text edits for the active buffer.

### R-212 Tab Close Safety

Clicking a tab close target, middle-clicking a tab, or pressing `Ctrl-w` shall close a saved tab immediately. Dirty tabs shall open a Save and Close / Don't Save / Cancel confirmation before any tab is removed.

### R-212A Reopen Closed Editor

`Ctrl-shift-t`, the command palette, and the editor context menu shall reopen the most recently closed editor tab. Clean file-backed tabs shall reopen from the current file contents while restoring the previous cursor, scroll, horizontal scroll, selection, and fold view state. Dirty file-backed tabs closed with Don't Save and dirty Untitled tabs closed without saving shall reopen from their in-memory buffer snapshots without writing to disk or creating Untitled placeholder files. Reopening shall focus the editor, avoid creating a duplicate if that tab path is already open, and report when no closed editor is available.

### R-212B Batch Editor Close

The command palette and editor context menu shall expose Close All Editors, Close Other Editors, and Close Editors to the Right. These actions shall close only clean target tabs, push each closed tab onto the closed-editor stack, preserve the active tab when it is not a target, keep dirty target tabs open without writing or discarding their buffers, clear stale split-pane references to removed tabs, and report how many clean tabs were closed and how many dirty tabs remained open.

### R-213 Paste

Paste events shall insert pasted text into the active editor buffer or send pasted text to the PTY when terminal focus is active.

### R-214 Newline Preservation

Saving an edited file shall preserve an existing trailing newline unless the buffer content already encodes its own final newline.

### R-215 Quick Open

`Ctrl-p` outside terminal focus shall open a quick-open overlay that fuzzy matches workspace file paths. Pressing `Enter` on a selected result shall open that file.

### R-216 Workspace Text Search

`Ctrl-shift-f` or `Ctrl-g` outside terminal focus shall open a workspace search overlay. The overlay shall scan real workspace files under the shared hidden/ignored/generated visibility policy, scan dirty open editor buffers from their in-memory text instead of stale disk text, show file/line previews, mark dirty-buffer results as unsaved, and open the selected result at its matching line.

### R-216A Replace in Files

`Ctrl-shift-h` or the command palette shall prompt for a search string and replacement string, then replace literal matches across real workspace text files. The operation shall skip files hidden by the shared hidden/ignored/generated visibility policy, skip binary or oversized files, update clean open tabs with the saved replacement content, and skip dirty open tabs so unsaved work is not overwritten.

### R-217 Command Palette

`F1` or `Ctrl-shift-p` outside terminal focus shall open a command palette overlay. The command palette shall fuzzy-match available commands and execute the selected command with `Enter`.

### R-218 Line Editing Commands

The editor shall support indenting, outdenting, duplicating, deleting, moving, and toggling line comments for the active line or selected line range. `Tab`, `Shift-tab`, `Ctrl-shift-d`, `Ctrl-/`, `Alt-up`, and `Alt-down` shall invoke the corresponding editor actions where the terminal can report those keys. Each selected-range line command shall be undoable as a single edit.

### R-218A Block Comments

`Shift-alt-a`, the command palette, and the editor context menu shall include a Toggle Block Comment action. For supported file types, the action shall wrap the active selection or indentation-preserving current line with the file type's block comment delimiters, and shall unwrap the same region when it is already surrounded by matching delimiters. Supported delimiters shall include C-style `/* */`, HTML/XML/Markdown `<!-- -->`, Lua `--[[ ]]`, and Python triple-quote blocks. The command shall update the in-memory editor buffer as one undoable edit, mark the tab dirty, preserve file contents on disk until save, and report a clear status message when the active file type has no block comment token.

### R-218B Word Wrap

`Alt-z`, the command palette, and the editor context menu shall toggle word wrap for editor panes. When word wrap is enabled, long visual editor lines shall wrap to the current pane code width without inserting newline characters, mutating the buffer, changing dirty state, or changing line numbers. Vertical scrolling, cursor visibility, hover lookup, fold-gutter clicks, mouse cursor placement, mouse drag selection, and wheel scrolling shall map wrapped visual rows back to the correct underlying file line and character column. Horizontal scrolling shall be disabled while word wrap is enabled, and the status bar shall show that wrap mode is active.

### R-219 Go To Line

The editor shall support jumping to one-based `line` or `line:column` input through `Ctrl-l` or the command palette.

### R-220 Save All

The command palette shall include a save-all command that writes every dirty file-backed editor tab to disk when the backing file is clean. It shall skip dirty Untitled tabs and dirty tabs with external disk conflicts, and it shall report the skipped counts.

### R-221 Editor Selection

The editor shall support text selection with `Shift` plus arrow keys and with mouse left-button drag inside the editor body. Editor drag selection shall preserve the drag session even when the pointer moves outside the editor body, clamp the endpoint to the nearest editor location, scroll vertically or horizontally at the visible edge when word wrap is disabled, use wrapped visual rows when word wrap is enabled, and reuse the same selected-text model used by copy, cut, replacement, run-selection-in-terminal, and selection-aware line commands. Selected text shall be visually highlighted. The editor shall also support multiple selected occurrence ranges in the active file.

### R-221A Mouse Multi-Cursor

`Alt`+click inside the editor body shall toggle an additional editor cursor at the clicked text position after applying line-number gutter, vertical scroll, wrapped visual row mapping when word wrap is enabled, and horizontal scroll offsets when word wrap is disabled. Typing, paste, `Enter`, `Backspace`, and `Delete` shall operate on every active cursor as one undoable edit when no explicit selection ranges are active. A regular editor click shall clear extra cursors and move back to a single cursor.

### R-222 Editor Clipboard

When editor focus is active, `Ctrl-a` shall select the full buffer, `Ctrl-c` shall copy the selected text to an internal editor clipboard, `Ctrl-x` shall cut the selected text to that clipboard, and `Ctrl-v` shall paste that clipboard at the cursor or replace the active selection. If there is no active editor selection, `Ctrl-c` shall copy the current line including its trailing newline and `Ctrl-x` shall cut the current line including its trailing newline as one undoable edit. `Ctrl-c` and `Ctrl-x` shall also queue an OSC52 terminal clipboard export for copied or cut text within the configured terminal-safe size limit.

### R-223 Selection Replacement

Typing, paste events, `Enter`, `Backspace`, or `Delete` with active editor selections shall replace or remove the selected range or selected occurrence ranges as a single undoable edit.

### R-224 Editor Auto Pairs and Indentation

The editor shall auto-insert closing pairs for `()`, `[]`, `{}`, double quotes, single quotes, and backticks. Typing an existing closing pair at the cursor shall move over it instead of duplicating it. `Backspace` between an empty pair shall remove both sides. `Enter` shall preserve leading indentation and add one extra indent level after an opening `{`, `[`, or `(`, splitting an immediate closing pair onto its own base-indent line.

### R-225 Search Highlighting

When an active in-file search exists, the editor shall visually highlight visible matches and show the match count in the status bar for the active file.

### R-226 Replace in File

`Ctrl-h` or the command palette shall prompt for a search string and replacement string, then replace the current match when the cursor is on a match or the next match otherwise. The replacement shall be undoable as one edit.

### R-227 Replace All in File

The command palette shall include a replace-all-in-file action that prompts for a search string and replacement string, then replaces every active-file match as one undoable edit.

### R-228 Word Navigation

The editor shall support modified-arrow word movement and word selection for `Ctrl-Left`, `Ctrl-Right`, `Ctrl-Shift-Left`, and `Ctrl-Shift-Right` when the terminal reports those keys.

### R-229 Path Clipboard Commands

The command palette shall expose commands to copy the active editor file path and the selected explorer item path as both absolute and workspace-relative strings. These commands shall queue the same OSC52 terminal clipboard export used by editor copy and cut without modifying explorer copy/cut state.

### R-230 Run Selection in Terminal

`Ctrl-enter` or the command palette shall send the active editor selection, or the current non-blank line when no selection exists, to the active PTY shell as submitted command text and then focus the integrated terminal. Blank editor selections and blank current lines shall not send PTY input.

### R-230A Run Saved File in Terminal

`F5`, the command palette, the editor context menu, and the explorer context menu shall run one supported saved source file by creating a new integrated PTY terminal rooted at that file's parent folder and submitting the derived run command. The command resolver shall support common interpreted or tool-run file types including shell scripts, Python, JavaScript, TypeScript, Ruby, PHP, Perl, Lua, Go, Java, Swift, PowerShell, batch files, and executable files. Dirty file-backed editor buffers, Untitled buffers, unsupported file types, directories, and multi-file explorer selections shall not submit PTY input and shall report a clear status message instead.

### R-231 Trim Trailing Whitespace

The command palette shall include a trim-trailing-whitespace action for the active editor tab. The action shall remove trailing spaces and tabs from all editor lines, mark the buffer dirty only when a change occurs, preserve the file's trailing newline state, and be undoable as one edit.

### R-232 Revert File

The command palette shall include a revert-file action for the active editor tab. The action shall reload the active file's current contents from disk, discard unsaved editor-buffer changes, clear selection and undo/redo history for that tab, reset the dirty marker, preserve cursor visibility within the reloaded content, and refresh Git status markers.

### R-232A External File Changes

The app shall detect when an open editor tab's backing file is modified or deleted outside `tscode`. A clean tab whose backing file changed shall reload from disk, clear disk-change status, keep the tab clean, and refresh Git status markers. A dirty tab whose backing file changed or disappeared shall keep the in-memory edits, show disk-change status in the tab strip and status bar, and prevent Save File or Save All from overwriting the changed or deleted backing file until the user explicitly reloads/reverts the tab or saves it to another path. A clean tab whose backing file was deleted shall stay open with deleted-on-disk status and shall not recreate the file through Save File without an explicit alternate save path.

### R-233 Document Symbols

`Ctrl-shift-o` or the command palette shall open a quick panel listing code symbols for the active editor buffer. For file types with a configured or discoverable language server, the app shall first publish the current in-memory buffer and request `textDocument/documentSymbol`, accepting both hierarchical `DocumentSymbol[]` and flat `SymbolInformation[]` responses. Returned symbols shall be filterable in the quick panel and selecting one shall focus the editor and move the cursor to the symbol's file location. If no LSP symbols are available, the app shall fall back to extracting common function, method, type, class, interface, module, namespace, and implementation declarations from the active buffer without requiring a language server.

### R-234 Workspace Symbols

`Ctrl-t` or the command palette shall open a quick panel listing code symbols across the workspace. For file types with a configured or discoverable language server, the app shall first publish the active in-memory buffer and request `workspace/symbol` with the current quick-panel query, accepting server symbols that include either full `Location` ranges or URI-only workspace-symbol locations. Returned symbols shall be filterable in the quick panel and selecting one shall open the owning file and move the editor cursor to that symbol. If no LSP workspace symbols are available, the app shall fall back to scanning visible workspace text files for common function, method, type, class, interface, module, namespace, and implementation declarations while applying the same hidden/generated-folder visibility policy as quick open and workspace search. Dirty open editor buffers shall be scanned from their in-memory text during fallback so unsaved symbols can be found.

### R-235 Go To Definition

`Ctrl-]` or the command palette shall use the active editor selection, when it is a valid identifier, or the identifier under the editor cursor to request `textDocument/definition` from an installed language server when a supported server is configured or discoverable for the active file type. If the language server returns one definition, the editor shall open that file and move the cursor to the definition. If the language server returns multiple definitions, a quick panel shall list the candidates. If no language server is configured, the server cannot be started, the request times out, or no LSP definition is returned, the command shall fall back to searching visible workspace text files for matching code symbol definitions. Dirty open editor buffers shall be scanned from their in-memory text during the fallback search.

### R-235A Type Definition and Implementation

The command palette and editor context menu shall include Go to Type Definition and Go to Implementation actions. Each action shall use the active editor selection, when it is a valid identifier, or the identifier under the editor cursor to request `textDocument/typeDefinition` or `textDocument/implementation` from an installed language server when a supported server is configured or discoverable for the active file type. If the language server returns one location, the editor shall open that file and move the cursor to the returned location. If the language server returns multiple locations, the app shall open a filterable quick panel listing those locations. If no language server is configured, the server cannot be started, the request times out, or no matching LSP location is returned, the app shall report a clear status message and keep the current editor state usable.

### R-235B Call Hierarchy

The command palette and editor context menu shall include Show Incoming Calls and Show Outgoing Calls actions. Each action shall use the active editor selection, when it is a valid identifier, or the identifier under the editor cursor to request `textDocument/prepareCallHierarchy` from an installed language server when a supported server is configured or discoverable for the active file type. For each prepared call hierarchy item, Show Incoming Calls shall request `callHierarchy/incomingCalls`, list returned caller symbols and call-site ranges in a filterable quick panel, and open the selected caller call site. Show Outgoing Calls shall request `callHierarchy/outgoingCalls`, list returned callee symbols in a filterable quick panel, and open the selected callee symbol. The app shall accept call hierarchy item `uri`, `range`, `selectionRange`, `kind`, `detail`, and opaque `data` fields by preserving the server-returned item in the follow-up request. If no language server is configured, the server cannot be started, the request times out, prepare returns no item, or no calls are returned, the app shall report a clear status message and keep the current editor state usable.

### R-235C Document Highlights

`Ctrl-shift-e`, the command palette, and the editor context menu shall include a Highlight Symbol action. The action shall use the active editor selection, when it is a valid identifier, or the identifier under the editor cursor to request `textDocument/documentHighlight` from an installed language server when a supported server is configured or discoverable for the active file type. The app shall publish the current in-memory buffer before the request, accept returned `DocumentHighlight` ranges with optional `kind` values for text, read, and write highlights, convert UTF-16 columns into editor character columns, and render the returned ranges directly in the active editor without changing the buffer contents or dirty state. Read, write, and text highlights shall be visually distinguishable where terminal colors allow, and the active status bar shall show the highlight count. The command palette and editor context menu shall also include Clear Symbol Highlights. Any buffer edit, reload, undo, redo, or whole-buffer replacement shall clear stale document highlights. If no language server is configured, the server cannot be started, the request times out, or no highlights are returned, the app shall report a clear status message and keep the editor usable.

### R-236 Find References

`Ctrl-r` or the command palette shall use the active editor selection, when it is a valid identifier, or the identifier under the editor cursor to request `textDocument/references` from an installed language server when a supported server is configured or discoverable for the active file type. If the language server returns references, the app shall open a filterable quick panel listing those locations and selecting a reference shall open its file and move the cursor to the occurrence. If no language server is configured, the server cannot be started, the request times out, or no LSP references are returned, the command shall fall back to whole-word workspace reference scanning. Reference fallback scanning shall apply the same visible workspace text-file policy as workspace search and shall use in-memory text for dirty open editor buffers.

### R-236A Code Suggestions

`Ctrl-space` or the command palette shall open a suggestions quick panel for the active editor cursor. The initial suggestions query shall be seeded from the identifier prefix before the cursor, while activation shall replace the current identifier span around the cursor. Suggestion candidates shall include installed-language-server `textDocument/completion` candidates when available, lightweight code symbols, identifier tokens from visible workspace text files, dirty open editor buffers from memory, and file-type keywords. The app shall request LSP completions once when the suggestions panel opens and then filter cached candidates locally while the quick-panel query changes. Selecting a suggestion shall focus the editor, update the active buffer as one undoable edit, mark the tab dirty when text changes, and preserve the backing file on disk until the user saves. If the language server is absent, fails, or times out, workspace and keyword suggestions shall still appear.

### R-236A1 Signature Help

`Ctrl-shift-space`, the command palette, or the editor context menu shall request `textDocument/signatureHelp` for the active editor buffer and cursor when a supported or configured language server is available. The app shall publish the current in-memory buffer before the request, accept returned signatures, `activeSignature`, `activeParameter`, string parameter labels, and UTF-16 offset-pair parameter labels, and display the returned signatures in a filterable quick panel that includes server name, active signature, active parameter, and documentation previews. If no server is configured, the server cannot be started, the request times out, or no signatures are returned, the app shall report a clear status message and keep the editor usable.

### R-236B Code Folding

The editor shall detect foldable delimiter and indentation blocks in the active buffer. Clicking a fold marker in the editor gutter, pressing `Alt-[`, using the command palette, or using the editor context menu shall fold or unfold the block at that line. `Alt-]`, the command palette, and the editor context menu shall unfold all folded blocks. Folded blocks shall hide their interior lines from the rendered editor viewport, vertical scroll calculations, and editor mouse-coordinate mapping while preserving the underlying file contents, dirty state, undo/redo history, and line numbers. If the editor cursor moves into a folded block through search, navigation, or editing, the block containing the cursor shall unfold so the cursor remains visible.

### R-236B1 Matching Bracket Navigation

`Ctrl-shift-\`, the command palette, and the editor context menu shall include a Go to Matching Bracket action. When the editor cursor is on a bracket or immediately after one, the action shall scan the active in-memory buffer and jump between matching `()`, `[]`, and `{}` pairs, including nested pairs of the same type. The action shall record the previous editor location in navigation history when the destination differs, keep the editor focused, update scroll so the target bracket is visible, and unfold any folded block that contains the target. If no supported bracket is at the cursor or no matching bracket exists, the app shall report a clear status message without changing the buffer.

### R-236C Symbol Hover

Moving the mouse over an identifier in the editor body shall compute a lightweight symbol hover without changing the editor cursor. The hover shall use the same visible workspace text-file provider as go-to-definition and find-references, include dirty open buffers from memory, ignore generated/hidden paths according to the current visibility settings, and display the hovered symbol, definition count, reference count, and first matching definition location/preview when available. Moving the mouse away from the editor body shall clear the symbol hover.

### R-236D Language Server Hover

The command palette and editor context menu shall include a Show Hover action. When invoked with an active editor cursor in a file type with a configured or discoverable language server, the app shall start the server over stdio, perform JSON-RPC/LSP initialization, publish the current in-memory buffer through `textDocument/didOpen`, request `textDocument/hover`, and display non-empty hover contents in a quick panel. If the server is absent, fails, times out, or returns no hover contents, the app shall report a clear status message and keep the editor usable.

### R-236E Code Actions

The command palette and editor context menu shall include a Code Action action. When invoked with an active editor cursor in a file type with a configured or discoverable language server, the app shall start the server over stdio, publish the current in-memory buffer through `textDocument/didOpen`, request `textDocument/codeAction` for the current cursor range, and include known active-buffer diagnostics in the request context when available. Returned actions shall open in a filterable quick panel. Selecting an action that contains a `WorkspaceEdit` shall apply text edits to open editor buffers as undoable dirty edits without writing their backing files, and shall write edits targeting closed files to disk after range validation. Selecting a command-only action shall execute `workspace/executeCommand`, acknowledge and collect server-initiated `workspace/applyEdit` edits, and apply those edits through the same range-validated path. If a command produces no applicable edits, or if the server is absent, fails, times out, or returns no actions, the app shall report a clear status message and keep the editor usable.

### R-237 Format Document

`Shift-alt-f` or the command palette shall format the active editor buffer. For file types with a configured or discoverable language server, the app shall first publish the current in-memory buffer and request `textDocument/formatting`; returned `TextEdit` values shall be applied as one undoable dirty-buffer edit without writing the backing file. If no LSP formatting edit is available, the app shall pipe the current text to a configured external formatter for the file type. Supported external formatter integrations shall include `rustfmt` for Rust, `prettier` for JavaScript/TypeScript/JSON/CSS/HTML/Markdown/YAML, `gofmt` for Go, `black` for Python, `shfmt` for shell scripts, and `clang-format` for C-family files where those tools are installed. The command shall mark the tab dirty when formatting changes text, preserve the on-disk file until save, and report a clear message when no formatter is configured.

### R-238 Occurrence Selection

`Ctrl-d` or the command palette shall add the next active-file occurrence of the current single-line selection, or the identifier under the editor cursor when no selection exists, to the editor selection set. `Ctrl-shift-l` or the command palette shall select all active-file occurrences. Identifier-based occurrence selection shall respect identifier boundaries. Copy, cut, typing, paste, `Enter`, `Backspace`, and `Delete` shall operate on all selected occurrence ranges as one undoable edit, and the status bar shall show the selected occurrence count. Continued typing after replacing selected occurrences shall preserve the resulting cursor set for additional simultaneous edits.

### R-238A Editor Gutter Line Selection

Clicking or dragging in the editor line-number gutter, excluding the fold marker cell, shall select complete logical editor lines rather than placing a character cursor. Dragging downward or upward shall select the inclusive line range between the drag anchor and active row, keep the active cursor at the drag edge for viewport scrolling, and report the selected line count on release. The selected line range shall use the same selection model as keyboard line selections, so copy, cut, delete-line, duplicate-line, move-line, indent/outdent, line-comment, block-comment, typing, paste, and run-selection commands operate on the actual selected buffer lines as one undoable edit where applicable. Fold marker clicks in the gutter shall continue to fold or unfold code blocks.

### R-239 Editor Navigation History

The app shall record the current editor file, line, and column before quick-panel result jumps, go-to-definition jumps, go-to-line jumps, and terminal `path:line:column` reference jumps when the destination differs from the current location. `Alt-left`, `Alt-right`, and command palette actions shall move backward and forward through this navigation history, reopening the target file if needed and restoring the recorded cursor position. Renaming or moving files and folders through the explorer shall remap matching navigation-history paths; deleting files or folders through the explorer shall remove matching history entries.

### R-240 Rename Symbol

`F2` or the command palette shall prompt for a replacement identifier for the active editor selection, when it is a valid identifier, or the identifier under the editor cursor. When a supported or configured language server is available, the command shall request `textDocument/rename` with the current in-memory active buffer and apply returned `WorkspaceEdit` text edits. Returned edits targeting open editor buffers shall be updated in memory as undoable dirty edits without immediately writing their backing files, while returned edits targeting closed files shall be written to disk after range validation. If no language server is configured, the server cannot be started, the request times out, or no valid LSP edits are returned, the command shall fall back to replacing whole-identifier occurrences across visible workspace text files using the same hidden/generated-folder visibility policy as workspace search. The fallback shall update open editor buffers as undoable dirty edits, write matching closed files to disk, skip binary and oversized files, and avoid modifying longer identifiers that merely contain the old name.

### R-241 Workspace Problems

The command palette shall include Run Workspace Check, Run LSP Diagnostics, and Show Problems actions, and the editor context menu shall include Run LSP Diagnostics. Run Workspace Check shall detect supported project roots, run the matching external checker in the workspace root, collect parseable file diagnostics from checker output, and open a Problems quick panel. Run LSP Diagnostics shall start the configured or discoverable stdio language server for the active editor buffer, initialize it, publish the current in-memory buffer with `textDocument/didOpen`, collect `textDocument/publishDiagnostics` notifications, map severity/source/code/message/location fields into Problems entries, and open the Problems quick panel. Selecting a problem shall open the referenced file and move the editor cursor to the diagnostic line and column. The Problems panel shall be filterable with the same quick-panel query input and shall report when no supported checker or language server is detected, a language server times out, or no parseable diagnostics are found. Collected diagnostics for the active file shall also appear inside the editor as line-level gutter markers, subtle line backgrounds, active-file problem counts, and active-line status text without modifying the file buffer. If dirty editor buffers exist during Run Workspace Check, the completion message shall indicate that unsaved buffers were not checked.

All bottom prompt inputs and quick-panel query inputs shall expose an editable cursor, render the cursor location, accept insertion and paste at the cursor, and support `Left`/`Right`, `Home`/`End`, `Backspace`, `Delete`, `Ctrl-A`, `Ctrl-E`, `Ctrl-U`, and `Ctrl-K`. Quick-panel `Ctrl-Home` and `Ctrl-End` shall continue to jump to the first and last result rows while unmodified `Home` and `End` edit the query cursor.

### R-242 Source Control

The command palette shall include Source Control, Git Checkout Branch, Git Create Branch, Stage All Changes, Unstage All Changes, Commit Staged Changes, Commit All Changes, and Discard All Changes actions. When the workspace is inside a Git repository, Source Control shall refresh Git status and current branch, show the current branch in the status bar, open a quick panel listing changed files, include diff hunk entries parsed from `git diff --unified=0`, include branch/stage/unstage/commit/discard command rows for paths whose porcelain index/worktree state supports that operation, support filtering those entries with the quick-panel query, open selected changed-file rows as read-only Git diff tabs, synthesize a readable new-file diff for untracked text files, prevent editing/saving those diff tabs, list local branches, checkout a selected local branch, create and checkout a new branch after validating a prompted branch name, block branch checkout/create when any file-backed editor tab has unsaved changes, stage selected paths through `git add`, unstage selected paths through `git restore --staged` with reset/rm-cached fallbacks, stage all changes through `git add -A`, unstage all changes, commit staged changes after prompting for a non-empty message, commit all changes by staging every current Git change and then committing after prompting for a non-empty message, discard selected tracked changes through `git restore` or compatible fallbacks, remove selected untracked files through `git clean`, discard all changed paths after typed confirmation, block commit-all and discard operations when an affected file-backed editor tab has unsaved changes, close clean editor tabs for files removed by discard, refresh the explorer and Source Control panel after each operation, and open an existing changed file at the selected hunk line. Deleted or otherwise missing files shall remain visible as changed-file entries without crashing when selected.

### R-243 Run Task

`Ctrl-shift-b` or the command palette shall open a Run Task quick panel outside terminal focus. The panel shall detect shell tasks from `.vscode/tasks.json`, package manager scripts from `package.json`, Cargo tasks from `Cargo.toml`, Make targets from `Makefile`, Go tasks from `go.mod`, and Python tasks from `pyproject.toml` or `setup.py`. The panel shall support fuzzy filtering. Selecting a task shall create a new integrated PTY terminal session using the task working directory, send the task command as submitted shell input, focus the terminal, and keep other terminal sessions alive.

## 5. Integrated Terminal Requirements

### R-301 Command Input

The bottom panel shall contain an interactive platform shell running in a pseudo terminal.

### R-302 Command Execution

Typing while the terminal is focused shall send input bytes to the shell PTY.

### R-303 Output Capture

The terminal panel shall parse PTY output and render the resulting terminal screen.

The terminal panel shall show active-session status including terminal title, working directory, live/exited state, child exit code or signal when available, nonzero scrollback offset, and active child-requested modes such as alternate screen, bracketed paste, and mouse reporting. Exited terminal tabs shall remain visible until closed or restarted, and restarting a terminal shall clear the stored exit status. Unlocked terminal sessions shall update their displayed title when the child emits OSC 0 or OSC 2 title reports.

### R-304 Working Directory

New terminal sessions shall start with the workspace root as the working directory unless the user explicitly creates them from a selected explorer location or task. The app shall track terminal working-directory changes reported by the child shell through OSC 7 current-directory sequences, shall inject automatic cwd reporting for supported interactive zsh and bash sessions, shall update the terminal header and context-menu details after `cd`, and shall restart that terminal in the latest tracked working directory.

### R-305 Scrolling

Mouse wheel and keyboard shortcuts shall scroll terminal output using normal terminal scrollback direction. Wheel up and `Shift-PageUp` shall move away from the live bottom into older retained output; wheel down and `Shift-PageDown` shall move back toward the live bottom.

### R-306 Hover

Terminal panel controls and focusable areas shall visually react to hover/focus.

### R-307 Interactive Signals

When the terminal is focused, `Ctrl-c` shall be forwarded to the PTY shell rather than exiting the application.

Editor `Ctrl-c` shall copy selected editor text instead of sending a terminal signal because the terminal is not focused.

### R-308 Terminal Paste

Paste events while terminal focus is active shall write pasted bytes to the PTY shell.

### R-309 Terminal Clear

The command palette shall include a clear-terminal command that resets the rendered terminal viewport and scrollback while keeping the current shell session alive.

### R-310 Terminal Restart

The command palette shall include a restart-terminal command that terminates the current PTY child and creates a fresh shell session in that terminal session's stored working directory.

### R-311 Terminal ANSI Styles

The terminal renderer shall preserve visible ANSI foreground colors, background colors, bold, dim, italic, underline, and inverse styles parsed from PTY output.

### R-312 Terminal Modified Keys

When terminal focus is active, modified navigation keys, function keys, Shift-Tab, null, and application-cursor arrows shall be encoded as xterm-compatible sequences where crossterm reports enough information. Shell editing keys and control punctuation shall be forwarded as legacy terminal bytes where widely supported: `Alt-Backspace` as `ESC DEL`, `Ctrl-Backspace` as `Ctrl-W`, `Alt-Enter` as `ESC CR`, `Alt-Tab` as `ESC TAB`, and control punctuation/digit combinations such as `Ctrl-/`, `Ctrl-6`, and `Ctrl-8` as their traditional C0 control bytes.

### R-313 Terminal Bracketed Paste

When the child terminal application enables bracketed paste mode, paste events shall be wrapped in bracketed paste control sequences before being written to the PTY.

### R-314 Terminal File References

Clicking a visible shell output token that resolves to an existing workspace file path, optionally followed by `:line` or `:line:column`, shall open that file in the editor and move the cursor to the referenced location. The reference parser shall also recognize quoted paths with trailing line/column suffixes, Python traceback lines such as `File "path", line N`, parenthesized stack-frame formats such as `path(line,column)`, and Node/Jest/TypeScript stack frames such as `at fn (path:line:column)` when the resolved file exists.

### R-314A Terminal Web and File Links

When the child terminal app has not requested xterm mouse reporting, moving the mouse over a visible `http://`, `https://`, or `file://` link shall visually underline the link range. Clicking an `http://` or `https://` link shall copy the URL to the internal clipboard and queue an OSC52 terminal clipboard export when it is within the configured size limit. Clicking a percent-decoded `file://` URL that resolves to an existing file, optionally followed by `:line` or `:line:column`, shall open that file in the editor and move the cursor to the referenced location.

### R-315 Terminal Mouse Pass-Through

When the child terminal application requests xterm mouse events, terminal mouse down, release, drag, move, and wheel events shall be forwarded to the PTY instead of being interpreted as source-reference clicks or scrollback movement. The forwarded encoding shall follow the requested xterm mouse mode and coordinate encoding, including SGR, default, and UTF-8 encodings where the parser exposes them. Holding `Shift` while dragging or using the wheel over the terminal body shall intentionally bypass child mouse forwarding and use tscode's host terminal selection/scrollback controls.

### R-316 Terminal Text Selection

When the child terminal application has not requested xterm mouse events, dragging across the terminal body shall select visible terminal cells and render a visual highlight over the selected range. Releasing a non-empty terminal selection shall copy the selected visible text to the internal clipboard and queue an OSC52 terminal clipboard export when it is within the configured terminal-safe size limit. Clicking without dragging shall continue to resolve visible terminal file references. The same visible-output selection behavior shall be available with `Shift`+drag even when the child application has requested xterm mouse reporting.

### R-317 Terminal Clipboard Shortcuts and OSC52

When terminal focus is active, `Ctrl-Shift-C` shall copy the active terminal text selection without sending `Ctrl-C` to the PTY, and `Ctrl-Shift-V` shall paste the internal clipboard into the active PTY shell using bracketed paste when the child application has enabled it. Plain terminal `Ctrl-C` shall remain a PTY signal. When a child PTY program emits an OSC52 clipboard write, the app shall decode the clipboard payload, update the internal clipboard, and queue the same host-terminal OSC52 export used by editor and terminal selection copy actions. OSC52 clipboard read/query requests shall not overwrite the internal clipboard.

### R-317A Terminal Output Copy and Live Scroll

The command palette and terminal context menu shall include Copy Terminal Output and Scroll Terminal to Bottom actions. Copy Terminal Output shall collect the active terminal session's retained scrollback plus visible viewport text, trim only terminal padding at row ends and trailing blank rows, copy that text to the internal clipboard, and queue an OSC52 terminal clipboard export when it is within the configured terminal-safe size limit. Scroll Terminal to Bottom shall reset the active terminal session's user scrollback offset to the live PTY output without clearing terminal contents or restarting the shell.

### R-317B Terminal Command Prompt and Recent Commands

The command palette and terminal context menu shall include Run Terminal Command and Run Recent Terminal Command actions. Run Terminal Command shall prompt for non-empty shell text, normalize CRLF/CR line endings, submit the command to the active PTY with terminal enter bytes, focus the terminal, and add it to a bounded most-recently-used command history. Commands submitted by Run Selection in Terminal, Run Saved File in Terminal, and Run Task shall also enter the same history. Run Recent Terminal Command shall open a filterable quick panel backed by that history, avoid duplicate entries when rerunning a command, and submit the selected command to the active PTY.

### R-318 Terminal Layout Controls

The application shall support moving focus in and out of the terminal, maximizing/restoring the terminal panel, and increasing/decreasing the normal terminal panel height through shortcuts such as `F6`/``Ctrl-` `` and `F12`/`Ctrl-J`, command palette actions, or mouse dragging the terminal panel's highlighted top border. Mouse drag resizing shall clamp the terminal panel to a usable minimum height while leaving editor space visible. The terminal focus and maximize shortcuts shall work even when the terminal panel is currently focused.

### R-319 Multiple Terminal Sessions

The integrated terminal shall support multiple PTY shell sessions. The user shall be able to create a new terminal, switch the active terminal, close a terminal, and preserve each terminal's independent shell state while it remains open.

### R-319A Split Terminal Panes

The command palette, terminal context menu, and `Ctrl-shift-5` shortcut shall include a Split Terminal action when the child terminal app is not owning app shortcuts. Split Terminal shall create a new PTY shell session using the active terminal session's current working directory, render the previous and new sessions as side-by-side terminal panes, focus the new pane, and keep both shells independent. Clicking, dragging, scrolling, selecting text, searching, pasting, and opening file references inside a split pane shall apply to the pane under the mouse or the active pane. Closing one visible pane shall leave the other shell session alive and clear stale split-pane state.

### R-320 Terminal Tab Mouse Controls

The terminal panel shall render terminal session tabs with mouse hover highlighting. Clicking a terminal tab shall activate that session, clicking a close target shall close that session, and clicking the new-terminal target shall create a new PTY session.

### R-321 Rename Terminal

The command palette and terminal context menu shall include a Rename Terminal action. The action shall prompt for a new non-empty title for the active terminal session, update and lock the terminal tab/header title against later child OSC title updates, keep the existing PTY child alive, preserve the session working directory, and leave the number of terminal sessions unchanged. Blank or whitespace-only titles shall be rejected without changing the active terminal title.

### R-322 New Terminal Here

Explorer `t` and the command palette shall create a new integrated PTY terminal whose current working directory is the selected explorer folder, or the selected file's parent folder. Restarting that terminal shall preserve its current working directory instead of resetting it to the workspace root.

### R-323 Terminal Search

When terminal focus is active, `Ctrl-f` or the command palette shall prompt for a literal search string for the active terminal session. The search shall scan the active terminal's current visible screen and scrollback retained by the terminal parser, highlight all visible matches, show the selected match count in the terminal header, and scroll the terminal viewport to the selected match. `F3` shall move to the next terminal match and `Shift-F3` shall move to the previous terminal match without sending those keys to the PTY while terminal search is active.

### R-324 Terminal Child Keyboard Ownership

When the active terminal child has entered alternate-screen mode or requested terminal mouse reporting, terminal-focused app conveniences that would otherwise intercept shell input, including terminal search, terminal search navigation, terminal tab management shortcuts, terminal maximize shortcuts, and terminal scrollback shortcuts, shall be forwarded to the PTY instead. `F6` and ``Ctrl-` `` shall remain available to move focus out of the terminal, and `Ctrl-Shift-C`/`Ctrl-Shift-V` shall remain terminal selection copy and clipboard paste shortcuts.

### R-325 Terminal Context Menu

Right-clicking the terminal body or terminal tabs shall focus the terminal and open a mouse-selectable context menu when the active child application has not requested terminal mouse reporting. Right-clicking a terminal tab shall first select that terminal session. The menu shall expose actions for copy selection, copy all output, paste, terminal search, clear terminal, restart terminal, rename terminal, new terminal, close terminal, next terminal, previous terminal, toggle terminal maximize, scroll to bottom, increase terminal height, decrease terminal height, focus editor, and focus explorer. When the child application has requested terminal mouse reporting, right-click mouse events over the terminal body shall be forwarded to the PTY instead of opening the app-owned context menu.

## 6. Mouse Requirements

### R-401 Mouse Capture

The application shall enable mouse capture while running.

### R-402 Click Focus

Clicking explorer, editor, editor split pane, tab strip, or terminal shall move focus to that panel.

### R-403 Wheel Routing

Mouse wheel events shall apply to the panel or split pane under the cursor when possible.

### R-404 Hover Routing

Mouse move events shall update hover target state and cause a redraw.

## 7. Keyboard Requirements

### R-501 Focus Cycle

`Tab` shall cycle focus from the explorer, indent the active line in editor focus, and be forwarded to the PTY shell in terminal focus.

### R-502 Exit

`q` or `Esc` shall exit from non-terminal normal browsing mode. `Ctrl-q` shall exit globally.

Dirty editor buffers shall require an explicit `quit` confirmation before the application exits.

### R-503 Editor Scroll

`PageUp`, `PageDown`, `Up`, and `Down` shall scroll or move through the focused editor. Horizontal wheel gestures shall pan long editor lines.

### R-504 Terminal Editing

When terminal focus is active, printable characters and supported control/navigation keys shall be forwarded to the PTY shell.

## 8. Packaging Requirements

### R-601 Release Archives

Each binary target shall be packaged as an archive with `tscode` or `tscode.exe`, README, and license metadata when available.

### R-602 Linux Packages

The release workflow shall produce `.deb` and `.rpm` packages for supported Linux targets where tooling permits.

### R-603 Installer

`install.sh` shall detect OS and CPU architecture, download the matching GitHub release artifact, and install it into a writable path. Installer GitHub API and release-asset downloads shall use retry, connection-timeout, total-timeout, and stalled-transfer limits so a dead CDN connection fails or retries instead of hanging indefinitely.

When `TSCODE_VERSION` is unset or set to `latest`, the installer shall choose the highest semantic release tag available from GitHub releases instead of trusting API response order.

### R-604 CI Release

The GitHub Actions workflow shall build and upload release artifacts when a version tag is pushed.

## 9. Acceptance Tests

- Start `cargo run -- .` on macOS and confirm the TUI renders.
- Click a directory and confirm it expands or collapses.
- Click a file and confirm a tab opens.
- Use Explorer `Ctrl-Enter` or Open to Side and confirm the selected file opens in a side-by-side editor pane while the previous tab remains visible; click or wheel each pane and confirm the hovered pane becomes active and scrolls independently.
- Hover rows/tabs and confirm highlight changes.
- Copy, paste, duplicate, and cut/move explorer items and confirm filesystem results.
- Drag one or more selected explorer files onto another folder and confirm the files move, then repeat with `Alt` held and confirm the files are copied while originals remain.
- Select two text files in the explorer, run Compare Selected Files from the context menu or command palette, confirm a read-only unified diff tab opens, and confirm Save File does not modify either source file.
- Open a file, reveal it in the explorer, and confirm its row is selected.
- In the explorer, use `Right` on a folder to expand it, press `Right` again to move into its first child, then use `Left` to return to the folder, collapse it, and move to its parent.
- Edit text, use `Ctrl-z`/`Ctrl-y`, save with `Ctrl-s`, and confirm file contents on disk.
- Use Save As from the command palette to write the active buffer to a nested new path, confirm parent folders are created, confirm the active tab retargets to the new file, confirm the original source file is unchanged, and confirm a dirty open target is refused.
- Create an Untitled tab with `Ctrl-n`, type text, confirm no `Untitled-*` placeholder exists on disk, confirm Save File opens Save As, confirm Save As writes the real target and retargets the tab, and confirm Save All skips any remaining dirty Untitled tab.
- Open a binary file containing NUL bytes, confirm the editor tab is read-only with a hex/ascii preview, attempt typing and saving, and confirm the original bytes on disk are unchanged and workspace search does not return that preview as source text.
- Close a dirty file-backed tab and confirm Save and Close writes the file then closes the tab, Don't Save closes without changing disk, and Cancel keeps the dirty tab open.
- Close a dirty Untitled tab, choose Save and Close, confirm Save As writes the target file, closes the tab, and leaves no `Untitled-*` placeholder on disk.
- Use `Ctrl-f` and `F3` to move through search matches.
- Use `Ctrl-f` and confirm visible search highlights and match count.
- Use `Ctrl-h` to replace one match, save, and confirm file contents on disk.
- Use the command palette replace-all action and confirm all active-file matches change as one undoable edit.
- Use `F1` to open the command palette and execute an editor command.
- Use `Ctrl-l` or the command palette to jump to a line.
- Use `Ctrl-shift-\`, the command palette, or the editor context menu to jump between matching brackets, including a nested pair, then confirm `Alt-left` returns to the previous cursor location.
- Use `Shift-alt-a`, the command palette, or the editor context menu to toggle a block comment on a selection and on the current line, then undo and confirm the original text and selection are restored.
- Select all text, cut it, paste it back, save, and confirm file contents on disk.
- With no editor selection, press `Ctrl-c` and confirm the current line plus newline is copied; press `Ctrl-x` and confirm the current line is removed, then paste it back and confirm the buffer is restored.
- Drag in the editor, including past the bottom/right visible edge, confirm the selection extends while the viewport scrolls, then copy or cut the selected text and confirm the clipboard/file contents match the selected range.
- Type brackets/quotes and press `Enter` inside a brace pair, then confirm auto-pairing, skip-over, paired deletion, and auto-indent behavior.
- Use line editing commands, save, and confirm file contents on disk.
- Use `Ctrl-p` to quick-open a file by path fragment.
- Use workspace search to find text in a different file and jump to the matching line.
- Run Workspace Check in a broken Cargo, Go, or Python project and confirm the Problems panel opens with clickable diagnostics.
- Run LSP Diagnostics against a configured mock or installed language server and confirm active-buffer diagnostics appear in the Problems panel and editor gutter.
- Modify a tracked Git file and create an untracked file, open Source Control, confirm changed-file and hunk rows appear, confirm selecting a changed-file row opens a read-only diff tab, confirm local branch checkout reloads clean open tabs and refuses dirty open tabs, confirm branch creation validates a prompted branch name and switches to it, confirm stage/unstage rows update `git diff --cached`, confirm Commit Staged commits only the index with the prompted message, confirm Commit All refuses to run when an affected editor tab is dirty and otherwise commits tracked plus untracked disk changes, confirm discard requires typing `discard`, confirm discarding the tracked file restores it from Git, confirm discarding the untracked file removes it and closes any clean matching tab, confirm discard-all refuses to run when an affected editor tab is dirty, and confirm the hunk row opens the file at the changed line.
- Use `Ctrl-shift-b` or the command palette in a project with task metadata and confirm detected tasks appear, filtering works, and selecting one starts a new PTY terminal running that command.
- Use Split Terminal or `Ctrl-shift-5`, confirm a second side-by-side PTY pane appears in the same working directory, click each pane and confirm input goes to the clicked shell, then close one pane and confirm the other shell remains usable.
- Print repeated text in the terminal, use terminal `Ctrl-f`, confirm matches are highlighted and counted, and use `F3`/`Shift-F3` to move between matches in scrollback.
- Scroll terminal output away from the bottom, run Copy Terminal Output, confirm retained output is copied, then run Scroll Terminal to Bottom and confirm the live prompt/output is visible again.
- Use the command palette to clear and restart the integrated terminal.
- Use the mouse wheel over explorer/editor/terminal and confirm scroll changes.
- Type `pwd` in the terminal panel and confirm the output points at the workspace.
- In terminal focus, confirm shell editing shortcuts such as `Alt-Backspace`, `Ctrl-Backspace`, `Ctrl-/`, and `Ctrl-6` are forwarded to the PTY instead of being swallowed by the app.
- Print colored terminal output and confirm ANSI colors/styles are rendered.
- Print `src/app.rs:10:1`, click it in the terminal panel, and confirm the editor opens that file at line 10, column 1.
- Build release artifacts through the release workflow.
