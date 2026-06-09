use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

use crate::{
    fs_tree::{FsTree, VisibleNode},
    shell::ShellPanel,
    syntax::SyntaxHighlighter,
};

const MAX_QUICK_ITEMS: usize = 200;
const MAX_FILE_SCAN_BYTES: u64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPanel {
    Explorer,
    Editor,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HoverTarget {
    #[default]
    None,
    Explorer,
    ExplorerRow(usize),
    Editor,
    Tab(usize),
    TabClose(usize),
    QuickRow(usize),
    Terminal,
    TerminalInput,
}

#[derive(Debug, Clone, Default)]
pub struct HitRegions {
    pub explorer_area: Option<Rect>,
    pub editor_area: Option<Rect>,
    pub editor_body: Option<Rect>,
    pub terminal_area: Option<Rect>,
    pub terminal_body: Option<Rect>,
    pub terminal_input: Option<Rect>,
    pub explorer_rows: Vec<(Rect, usize)>,
    pub tabs: Vec<(Rect, usize)>,
    pub tab_closes: Vec<(Rect, usize)>,
    pub quick_rows: Vec<(Rect, usize)>,
    pub last_mouse_x: u16,
    pub last_mouse_y: u16,
}

impl HitRegions {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn target_at(&self, x: u16, y: u16) -> HoverTarget {
        for (rect, index) in &self.quick_rows {
            if contains(*rect, x, y) {
                return HoverTarget::QuickRow(*index);
            }
        }

        for (rect, index) in &self.tab_closes {
            if contains(*rect, x, y) {
                return HoverTarget::TabClose(*index);
            }
        }

        for (rect, index) in &self.tabs {
            if contains(*rect, x, y) {
                return HoverTarget::Tab(*index);
            }
        }

        for (rect, index) in &self.explorer_rows {
            if contains(*rect, x, y) {
                return HoverTarget::ExplorerRow(*index);
            }
        }

        if self.terminal_input.is_some_and(|rect| contains(rect, x, y)) {
            return HoverTarget::TerminalInput;
        }

        if self.explorer_area.is_some_and(|rect| contains(rect, x, y)) {
            return HoverTarget::Explorer;
        }

        if self.editor_area.is_some_and(|rect| contains(rect, x, y)) {
            return HoverTarget::Editor;
        }

        if self.terminal_area.is_some_and(|rect| contains(rect, x, y)) {
            return HoverTarget::Terminal;
        }

        HoverTarget::None
    }
}

fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

#[derive(Debug, Clone)]
struct EditorSnapshot {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
    trailing_newline: bool,
}

#[derive(Debug, Clone)]
pub struct EditorTab {
    pub path: PathBuf,
    pub title: String,
    pub lines: Vec<String>,
    pub scroll: usize,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub dirty: bool,
    trailing_newline: bool,
    undo_stack: Vec<EditorSnapshot>,
    redo_stack: Vec<EditorSnapshot>,
}

impl EditorTab {
    fn open(path: PathBuf) -> Result<Self> {
        let bytes = std::fs::read(&path)?;
        let text = String::from_utf8_lossy(&bytes);
        let trailing_newline = text.ends_with('\n');
        let mut lines = text.lines().map(ToOwned::to_owned).collect::<Vec<_>>();
        if lines.is_empty() {
            lines.push(String::new());
        }

        let title = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("[file]")
            .to_owned();

        Ok(Self {
            path,
            title,
            lines,
            scroll: 0,
            cursor_line: 0,
            cursor_col: 0,
            dirty: false,
            trailing_newline,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        })
    }

    fn save(&mut self) -> Result<()> {
        let mut text = self.lines.join("\n");
        if self.trailing_newline && !text.ends_with('\n') {
            text.push('\n');
        }
        fs::write(&self.path, text)?;
        self.dirty = false;
        Ok(())
    }

    fn insert_char(&mut self, c: char) {
        self.push_undo();
        self.insert_char_raw(c);
        self.dirty = true;
    }

    fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        self.push_undo();
        for c in text.chars() {
            match c {
                '\r' => {}
                '\n' => self.newline_raw(),
                c => self.insert_char_raw(c),
            }
        }
        self.dirty = true;
    }

    fn insert_char_raw(&mut self, c: char) {
        let cursor_col = self.cursor_col;
        let line = self.current_line_mut();
        let byte = byte_index_for_char(line, cursor_col);
        line.insert(byte, c);
        self.cursor_col += 1;
    }

    fn newline(&mut self) {
        self.push_undo();
        self.newline_raw();
        self.dirty = true;
    }

    fn newline_raw(&mut self) {
        let cursor_col = self.cursor_col;
        let line = self.current_line_mut();
        let byte = byte_index_for_char(line, cursor_col);
        let rest = line.split_off(byte);
        self.cursor_line += 1;
        self.cursor_col = 0;
        self.lines.insert(self.cursor_line, rest);
    }

    fn backspace(&mut self) {
        if self.cursor_col == 0 && self.cursor_line == 0 {
            return;
        }

        self.push_undo();
        if self.cursor_col > 0 {
            let cursor_col = self.cursor_col;
            let line = self.current_line_mut();
            let end = byte_index_for_char(line, cursor_col);
            let start = byte_index_for_char(line, cursor_col - 1);
            line.replace_range(start..end, "");
            self.cursor_col -= 1;
        } else if self.cursor_line > 0 {
            let removed = self.lines.remove(self.cursor_line);
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].chars().count();
            self.lines[self.cursor_line].push_str(&removed);
        }
        self.dirty = true;
    }

    fn delete(&mut self) {
        let line_len = self.lines[self.cursor_line].chars().count();
        if self.cursor_col >= line_len && self.cursor_line + 1 >= self.lines.len() {
            return;
        }

        self.push_undo();
        if self.cursor_col < line_len {
            let cursor_col = self.cursor_col;
            let line = self.current_line_mut();
            let start = byte_index_for_char(line, cursor_col);
            let end = byte_index_for_char(line, cursor_col + 1);
            line.replace_range(start..end, "");
        } else if self.cursor_line + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_line + 1);
            self.lines[self.cursor_line].push_str(&next);
        }
        self.dirty = true;
    }

    fn indent_line(&mut self) {
        self.push_undo();
        self.lines[self.cursor_line].insert_str(0, "    ");
        self.cursor_col = self.cursor_col.saturating_add(4);
        self.dirty = true;
    }

    fn outdent_line(&mut self) -> bool {
        let remove_count = leading_indent_width(&self.lines[self.cursor_line]);
        if remove_count == 0 {
            return false;
        }

        self.push_undo();
        let end = byte_index_for_char(&self.lines[self.cursor_line], remove_count);
        self.lines[self.cursor_line].replace_range(0..end, "");
        self.cursor_col = self.cursor_col.saturating_sub(remove_count);
        self.dirty = true;
        true
    }

    fn duplicate_line(&mut self) {
        self.push_undo();
        let duplicate = self.lines[self.cursor_line].clone();
        self.cursor_line += 1;
        self.lines.insert(self.cursor_line, duplicate);
        self.clamp_cursor_col();
        self.dirty = true;
    }

    fn delete_line(&mut self) {
        self.push_undo();
        if self.lines.len() == 1 {
            self.lines[0].clear();
            self.cursor_col = 0;
        } else {
            self.lines.remove(self.cursor_line);
            self.cursor_line = self.cursor_line.min(self.lines.len().saturating_sub(1));
            self.clamp_cursor_col();
        }
        self.dirty = true;
    }

    fn move_line_up(&mut self) -> bool {
        if self.cursor_line == 0 {
            return false;
        }

        self.push_undo();
        self.lines.swap(self.cursor_line, self.cursor_line - 1);
        self.cursor_line -= 1;
        self.dirty = true;
        true
    }

    fn move_line_down(&mut self) -> bool {
        if self.cursor_line + 1 >= self.lines.len() {
            return false;
        }

        self.push_undo();
        self.lines.swap(self.cursor_line, self.cursor_line + 1);
        self.cursor_line += 1;
        self.dirty = true;
        true
    }

    fn toggle_line_comment(&mut self) -> bool {
        let Some(token) = comment_token_for_path(&self.path) else {
            return false;
        };

        self.push_undo();
        let line = &mut self.lines[self.cursor_line];
        let indent_chars = line.chars().take_while(|c| c.is_whitespace()).count();
        let indent_byte = byte_index_for_char(line, indent_chars);
        let body = &line[indent_byte..];
        let token_with_space = format!("{token} ");

        if body.starts_with(&token_with_space) {
            let remove_end = indent_byte + token_with_space.len();
            line.replace_range(indent_byte..remove_end, "");
            self.cursor_col = self
                .cursor_col
                .saturating_sub(token_with_space.chars().count());
        } else if body.starts_with(token) {
            let remove_end = indent_byte + token.len();
            line.replace_range(indent_byte..remove_end, "");
            self.cursor_col = self.cursor_col.saturating_sub(token.chars().count());
        } else {
            line.insert_str(indent_byte, &token_with_space);
            if self.cursor_col >= indent_chars {
                self.cursor_col = self
                    .cursor_col
                    .saturating_add(token_with_space.chars().count());
            }
        }

        self.dirty = true;
        true
    }

    fn move_cursor(&mut self, line_delta: isize, col_delta: isize) {
        self.cursor_line =
            add_signed(self.cursor_line, line_delta).min(self.lines.len().saturating_sub(1));
        self.cursor_col = add_signed(self.cursor_col, col_delta);
        self.clamp_cursor_col();
    }

    fn set_cursor(&mut self, line: usize, col: usize) {
        self.cursor_line = line.min(self.lines.len().saturating_sub(1));
        self.cursor_col = col;
        self.clamp_cursor_col();
    }

    fn current_line_mut(&mut self) -> &mut String {
        &mut self.lines[self.cursor_line]
    }

    fn clamp_cursor_col(&mut self) {
        let line_len = self.lines[self.cursor_line].chars().count();
        self.cursor_col = self.cursor_col.min(line_len);
    }

    fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo_stack.pop() else {
            return false;
        };
        self.redo_stack.push(self.snapshot());
        self.restore_snapshot(snapshot);
        self.dirty = true;
        true
    }

    fn redo(&mut self) -> bool {
        let Some(snapshot) = self.redo_stack.pop() else {
            return false;
        };
        self.undo_stack.push(self.snapshot());
        self.restore_snapshot(snapshot);
        self.dirty = true;
        true
    }

    fn snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            lines: self.lines.clone(),
            cursor_line: self.cursor_line,
            cursor_col: self.cursor_col,
            trailing_newline: self.trailing_newline,
        }
    }

    fn restore_snapshot(&mut self, snapshot: EditorSnapshot) {
        self.lines = if snapshot.lines.is_empty() {
            vec![String::new()]
        } else {
            snapshot.lines
        };
        self.trailing_newline = snapshot.trailing_newline;
        self.cursor_line = snapshot.cursor_line.min(self.lines.len().saturating_sub(1));
        self.cursor_col = snapshot.cursor_col;
        self.clamp_cursor_col();
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(self.snapshot());
        if self.undo_stack.len() > 200 {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    NewFile,
    NewDir,
    Rename(PathBuf),
    Delete(PathBuf),
    Search,
    GotoLine,
    QuitDirty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptState {
    pub kind: PromptKind,
    pub input: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickPanelKind {
    OpenFile,
    WorkspaceSearch,
    CommandPalette,
}

#[derive(Debug, Clone)]
pub struct QuickItem {
    pub label: String,
    pub detail: String,
    pub path: PathBuf,
    pub line: Option<usize>,
    pub col: Option<usize>,
    pub preview: Option<String>,
    pub command: Option<CommandAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandAction {
    QuickOpen,
    WorkspaceSearch,
    SaveFile,
    SaveAll,
    CloseActiveTab,
    CloseSavedTabs,
    NewFile,
    NewFolder,
    RenameSelected,
    DeleteSelected,
    RefreshExplorer,
    CollapseExplorer,
    RevealActiveFile,
    FindInFile,
    GotoLine,
    DuplicateLine,
    DeleteLine,
    MoveLineUp,
    MoveLineDown,
    ToggleLineComment,
    IndentLine,
    OutdentLine,
    FocusExplorer,
    FocusEditor,
    FocusTerminal,
    ClearTerminal,
    RestartTerminal,
}

#[derive(Debug, Clone)]
pub struct QuickPanel {
    pub kind: QuickPanelKind,
    pub query: String,
    pub items: Vec<QuickItem>,
    pub selected: usize,
    pub scroll: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardAction {
    Copy,
    Cut,
}

#[derive(Debug, Clone)]
pub struct ExplorerClipboard {
    pub action: ClipboardAction,
    pub path: PathBuf,
}

pub struct App {
    pub root: PathBuf,
    pub explorer: FsTree,
    pub tabs: Vec<EditorTab>,
    pub active_tab: Option<usize>,
    pub focus: FocusPanel,
    pub hover: HoverTarget,
    pub hit_regions: HitRegions,
    pub terminal: ShellPanel,
    pub syntax: SyntaxHighlighter,
    pub should_quit: bool,
    pub explorer_height: usize,
    pub editor_height: usize,
    pub terminal_height: usize,
    pub last_error: Option<String>,
    pub prompt: Option<PromptState>,
    pub message: Option<String>,
    pub search_needle: Option<String>,
    pub quick_panel: Option<QuickPanel>,
    pub quick_panel_height: usize,
    pub explorer_clipboard: Option<ExplorerClipboard>,
}

impl App {
    pub fn new(root: PathBuf) -> Result<Self> {
        let root = root.canonicalize().unwrap_or(root);
        let explorer = FsTree::new(root.clone())?;
        Ok(Self {
            root: root.clone(),
            explorer,
            tabs: Vec::new(),
            active_tab: None,
            focus: FocusPanel::Explorer,
            hover: HoverTarget::None,
            hit_regions: HitRegions::default(),
            terminal: ShellPanel::new(root.clone())?,
            syntax: SyntaxHighlighter::new(),
            should_quit: false,
            explorer_height: 0,
            editor_height: 0,
            terminal_height: 0,
            last_error: None,
            prompt: None,
            message: Some("F1/Ctrl-Shift-P commands | Ctrl-P files | Explorer: c/x/p/y/o | Editor: Ctrl-S save | Terminal: Ctrl-Q quit".to_owned()),
            search_needle: None,
            quick_panel: None,
            quick_panel_height: 0,
            explorer_clipboard: None,
        })
    }

    pub fn visible_nodes(&self) -> Vec<VisibleNode> {
        self.explorer.visible_nodes()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        if self.quick_panel.is_some() {
            return self.handle_quick_panel_key(key);
        }

        if self.prompt.is_some() {
            return self.handle_prompt_key(key);
        }

        if !matches!(self.focus, FocusPanel::Terminal) && key.code == KeyCode::F(1) {
            self.open_quick_panel(QuickPanelKind::CommandPalette)?;
            return Ok(());
        }

        if !matches!(self.focus, FocusPanel::Terminal)
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            match key.code {
                KeyCode::Char('P') => {
                    self.open_quick_panel(QuickPanelKind::CommandPalette)?;
                    return Ok(());
                }
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.open_quick_panel(QuickPanelKind::CommandPalette)?;
                    return Ok(());
                }
                KeyCode::Char('p') => {
                    self.open_quick_panel(QuickPanelKind::OpenFile)?;
                    return Ok(());
                }
                KeyCode::Char('F') | KeyCode::Char('g') => {
                    self.open_quick_panel(QuickPanelKind::WorkspaceSearch)?;
                    return Ok(());
                }
                KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.open_quick_panel(QuickPanelKind::WorkspaceSearch)?;
                    return Ok(());
                }
                _ => {}
            }
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
            self.request_quit();
            return Ok(());
        }

        match key.code {
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::CONTROL) => self.next_tab(),
            KeyCode::BackTab if self.focus == FocusPanel::Editor => {
                self.handle_editor_key(key)?;
            }
            KeyCode::BackTab if !matches!(self.focus, FocusPanel::Terminal) => self.previous_tab(),
            KeyCode::Tab if self.focus == FocusPanel::Editor => {
                self.handle_editor_key(key)?;
            }
            KeyCode::Tab if !matches!(self.focus, FocusPanel::Terminal) => self.cycle_focus(),
            KeyCode::Esc if !matches!(self.focus, FocusPanel::Terminal) => self.request_quit(),
            KeyCode::Char('q') if self.focus != FocusPanel::Terminal => self.request_quit(),
            _ => match self.focus {
                FocusPanel::Explorer => self.handle_explorer_key(key)?,
                FocusPanel::Editor => self.handle_editor_key(key)?,
                FocusPanel::Terminal => self.handle_terminal_key(key)?,
            },
        }

        Ok(())
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        self.hit_regions.last_mouse_x = mouse.column;
        self.hit_regions.last_mouse_y = mouse.row;
        let target = self.hit_regions.target_at(mouse.column, mouse.row);
        self.hover = target.clone();

        match mouse.kind {
            MouseEventKind::Moved => {}
            MouseEventKind::Down(MouseButton::Left) => self.activate_target(target)?,
            MouseEventKind::Down(MouseButton::Middle) => {
                if let HoverTarget::Tab(index) | HoverTarget::TabClose(index) = target {
                    self.close_tab(index);
                }
            }
            MouseEventKind::ScrollUp => self.scroll_target(target, -3),
            MouseEventKind::ScrollDown => self.scroll_target(target, 3),
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {}
            MouseEventKind::Drag(_) | MouseEventKind::Up(_) => {}
            MouseEventKind::Down(_) => {}
        }

        Ok(())
    }

    fn activate_target(&mut self, target: HoverTarget) -> Result<()> {
        match target {
            HoverTarget::Explorer | HoverTarget::ExplorerRow(_) => {
                self.focus = FocusPanel::Explorer;
            }
            HoverTarget::Tab(_) | HoverTarget::TabClose(_) | HoverTarget::Editor => {
                self.focus = FocusPanel::Editor;
            }
            HoverTarget::QuickRow(_) => {}
            HoverTarget::Terminal | HoverTarget::TerminalInput => {
                self.focus = FocusPanel::Terminal;
            }
            HoverTarget::None => {}
        }

        match target {
            HoverTarget::ExplorerRow(index) => {
                self.explorer.selected = index;
                self.open_or_toggle_selected()?;
            }
            HoverTarget::Tab(index) if index < self.tabs.len() => {
                self.active_tab = Some(index);
            }
            HoverTarget::TabClose(index) => self.close_tab(index),
            HoverTarget::QuickRow(index) => self.activate_quick_row(index),
            HoverTarget::Editor => {
                self.set_editor_cursor_from_mouse();
            }
            HoverTarget::Terminal | HoverTarget::TerminalInput => {
                self.send_terminal_mouse_click();
            }
            _ => {}
        }

        Ok(())
    }

    fn handle_explorer_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Up => self.move_explorer_selection(-1),
            KeyCode::Down => self.move_explorer_selection(1),
            KeyCode::PageUp => self.move_explorer_selection(-(self.explorer_height as isize)),
            KeyCode::PageDown => self.move_explorer_selection(self.explorer_height as isize),
            KeyCode::Enter | KeyCode::Right => self.open_or_toggle_selected()?,
            KeyCode::Left => self.collapse_selected(),
            KeyCode::Char('r') => self.refresh_explorer()?,
            KeyCode::Char('n') => self.start_prompt(PromptKind::NewFile, ""),
            KeyCode::Char('N') => self.start_prompt(PromptKind::NewDir, ""),
            KeyCode::Char('e') => self.prompt_rename(),
            KeyCode::Char('D') => self.prompt_delete(),
            KeyCode::Char('c') => self.copy_selected_path(),
            KeyCode::Char('x') => self.cut_selected_path(),
            KeyCode::Char('p') => self.paste_into_selected()?,
            KeyCode::Char('y') => self.duplicate_selected()?,
            KeyCode::Char('o') => self.reveal_active_file()?,
            _ => {}
        }

        Ok(())
    }

    fn handle_editor_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.modifiers.contains(KeyModifiers::SHIFT) && key.code == KeyCode::F(3) {
            self.find_next(false);
            return Ok(());
        }

        if key.modifiers.contains(KeyModifiers::ALT) {
            match key.code {
                KeyCode::Up => self.move_active_line_up(),
                KeyCode::Down => self.move_active_line_down(),
                _ => {}
            }
            return Ok(());
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('s') => self.save_active_tab(),
                KeyCode::Char('f') => {
                    let initial = self.search_needle.clone().unwrap_or_default();
                    self.start_prompt(PromptKind::Search, &initial);
                }
                KeyCode::Char('l') => self.start_prompt(PromptKind::GotoLine, ""),
                KeyCode::Char('/') => self.toggle_active_line_comment(),
                KeyCode::Char('d') => self.duplicate_active_line(),
                KeyCode::Char('w') => self.close_active_tab(),
                KeyCode::Char('z') => self.undo_active_tab(),
                KeyCode::Char('y') => self.redo_active_tab(),
                _ => {}
            }
            return Ok(());
        }

        match key.code {
            KeyCode::Up => self.move_editor_cursor(-1, 0),
            KeyCode::Down => self.move_editor_cursor(1, 0),
            KeyCode::Left => self.move_editor_cursor(0, -1),
            KeyCode::Right => self.move_editor_cursor(0, 1),
            KeyCode::PageUp => self.scroll_editor(-(self.editor_height as isize)),
            KeyCode::PageDown => self.scroll_editor(self.editor_height as isize),
            KeyCode::Home => self.set_editor_cursor_col(0),
            KeyCode::End => self.set_editor_cursor_end(),
            KeyCode::F(3) => self.find_next(true),
            KeyCode::Tab => self.indent_active_line(),
            KeyCode::BackTab => self.outdent_active_line(),
            KeyCode::Enter => self.edit_newline(),
            KeyCode::Backspace => self.edit_backspace(),
            KeyCode::Delete => self.edit_delete(),
            KeyCode::Char(c) => self.edit_insert(c),
            _ => {}
        }

        Ok(())
    }

    fn handle_terminal_key(&mut self, key: KeyEvent) -> Result<()> {
        self.terminal.send_key(key)
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter => self.finish_prompt()?,
            KeyCode::Backspace => {
                if let Some(prompt) = &mut self.prompt {
                    prompt.input.pop();
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(prompt) = &mut self.prompt {
                    prompt.input.push(c);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_quick_panel_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => self.quick_panel = None,
            KeyCode::Enter => self.activate_selected_quick_item(),
            KeyCode::Up => self.move_quick_selection(-1),
            KeyCode::Down => self.move_quick_selection(1),
            KeyCode::PageUp => self.move_quick_selection(-(self.quick_panel_height as isize)),
            KeyCode::PageDown => self.move_quick_selection(self.quick_panel_height as isize),
            KeyCode::Home => self.set_quick_selection(0),
            KeyCode::End => {
                if let Some(panel) = &self.quick_panel {
                    self.set_quick_selection(panel.items.len().saturating_sub(1));
                }
            }
            KeyCode::Backspace => {
                if let Some(panel) = &mut self.quick_panel {
                    panel.query.pop();
                }
                self.refresh_quick_panel()?;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(panel) = &mut self.quick_panel {
                    panel.query.push(c);
                }
                self.refresh_quick_panel()?;
            }
            _ => {}
        }
        Ok(())
    }

    fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            FocusPanel::Explorer => FocusPanel::Editor,
            FocusPanel::Editor => FocusPanel::Terminal,
            FocusPanel::Terminal => FocusPanel::Explorer,
        };
    }

    pub fn handle_paste(&mut self, text: String) -> Result<()> {
        if let Some(panel) = &mut self.quick_panel {
            panel.query.push_str(&text.replace(['\r', '\n'], " "));
            return self.refresh_quick_panel();
        }

        if let Some(prompt) = &mut self.prompt {
            prompt.input.push_str(&text.replace(['\r', '\n'], " "));
            return Ok(());
        }

        match self.focus {
            FocusPanel::Editor => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.insert_text(&text);
                    self.ensure_editor_cursor_visible();
                }
            }
            FocusPanel::Terminal => self.terminal.send_text(&text)?,
            FocusPanel::Explorer => {}
        }
        Ok(())
    }

    fn move_explorer_selection(&mut self, delta: isize) {
        let len = self.visible_nodes().len();
        if len == 0 {
            return;
        }

        let next = self.explorer.selected as isize + delta;
        self.explorer.selected = next.clamp(0, len.saturating_sub(1) as isize) as usize;
        self.ensure_explorer_selection_visible();
    }

    fn ensure_explorer_selection_visible(&mut self) {
        let height = self.explorer_height.max(1);
        if self.explorer.selected < self.explorer.scroll {
            self.explorer.scroll = self.explorer.selected;
        } else if self.explorer.selected >= self.explorer.scroll + height {
            self.explorer.scroll = self.explorer.selected.saturating_sub(height - 1);
        }
    }

    fn open_or_toggle_selected(&mut self) -> Result<()> {
        let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() else {
            return Ok(());
        };

        if node.is_dir {
            if let Err(error) = self.explorer.toggle(&node.path) {
                self.last_error = Some(error.to_string());
            }
        } else {
            self.open_file(&node.path);
        }

        self.ensure_explorer_selection_visible();
        Ok(())
    }

    fn collapse_selected(&mut self) {
        if let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() {
            self.explorer.collapse(&node.path);
        }
    }

    fn open_file(&mut self, path: &Path) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.path == path) {
            self.active_tab = Some(index);
            self.focus = FocusPanel::Editor;
            return;
        }

        match EditorTab::open(path.to_path_buf()) {
            Ok(tab) => {
                self.tabs.push(tab);
                self.active_tab = Some(self.tabs.len() - 1);
                self.focus = FocusPanel::Editor;
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    pub fn drain_terminal(&mut self) -> bool {
        let changed = self.terminal.drain();
        if self.terminal.child_exited() {
            self.message = Some("terminal shell exited".to_owned());
        }
        changed
    }

    pub fn active_tab(&self) -> Option<&EditorTab> {
        self.active_tab.and_then(|index| self.tabs.get(index))
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut EditorTab> {
        self.active_tab.and_then(|index| self.tabs.get_mut(index))
    }

    fn scroll_target(&mut self, target: HoverTarget, amount: isize) {
        match target {
            HoverTarget::Explorer | HoverTarget::ExplorerRow(_) => self.scroll_explorer(amount),
            HoverTarget::Editor | HoverTarget::Tab(_) | HoverTarget::TabClose(_) => {
                self.scroll_editor(amount)
            }
            HoverTarget::QuickRow(_) => self.scroll_quick_panel(amount),
            HoverTarget::Terminal | HoverTarget::TerminalInput => self.scroll_terminal(amount),
            HoverTarget::None => match self.focus {
                FocusPanel::Explorer => self.scroll_explorer(amount),
                FocusPanel::Editor => self.scroll_editor(amount),
                FocusPanel::Terminal => self.scroll_terminal(amount),
            },
        }
    }

    fn scroll_explorer(&mut self, amount: isize) {
        let len = self.visible_nodes().len();
        let max_scroll = len.saturating_sub(self.explorer_height.max(1));
        self.explorer.scroll = add_signed(self.explorer.scroll, amount).min(max_scroll);
    }

    fn scroll_editor(&mut self, amount: isize) {
        let height = self.editor_height.max(1);
        if let Some(tab) = self.active_tab_mut() {
            let max_scroll = tab.lines.len().saturating_sub(height);
            tab.scroll = add_signed(tab.scroll, amount).min(max_scroll);
        }
    }

    fn scroll_terminal(&mut self, amount: isize) {
        self.terminal.scroll(amount);
    }

    fn scroll_quick_panel(&mut self, amount: isize) {
        let height = self.quick_panel_height.max(1);
        if let Some(panel) = &mut self.quick_panel {
            let max_scroll = panel.items.len().saturating_sub(height);
            panel.scroll = add_signed(panel.scroll, amount).min(max_scroll);
            panel.selected = panel.selected.clamp(
                panel.scroll,
                panel
                    .scroll
                    .saturating_add(height.saturating_sub(1))
                    .min(panel.items.len().saturating_sub(1)),
            );
        }
    }
}

fn add_signed(value: usize, amount: isize) -> usize {
    if amount.is_negative() {
        value.saturating_sub(amount.unsigned_abs())
    } else {
        value.saturating_add(amount as usize)
    }
}

#[derive(Debug, Clone, Copy)]
struct CommandSpec {
    label: &'static str,
    detail: &'static str,
    shortcut: &'static str,
    action: CommandAction,
}

fn command_catalog() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            label: "Quick Open",
            detail: "Open a workspace file by fuzzy path",
            shortcut: "Ctrl-P",
            action: CommandAction::QuickOpen,
        },
        CommandSpec {
            label: "Search Workspace",
            detail: "Search text across workspace files",
            shortcut: "Ctrl-Shift-F",
            action: CommandAction::WorkspaceSearch,
        },
        CommandSpec {
            label: "Save File",
            detail: "Write the active editor tab to disk",
            shortcut: "Ctrl-S",
            action: CommandAction::SaveFile,
        },
        CommandSpec {
            label: "Save All",
            detail: "Write all dirty editor tabs to disk",
            shortcut: "",
            action: CommandAction::SaveAll,
        },
        CommandSpec {
            label: "Close Active Tab",
            detail: "Close the active tab when it has no unsaved edits",
            shortcut: "Ctrl-W",
            action: CommandAction::CloseActiveTab,
        },
        CommandSpec {
            label: "Close Saved Tabs",
            detail: "Close every clean tab and keep dirty tabs open",
            shortcut: "",
            action: CommandAction::CloseSavedTabs,
        },
        CommandSpec {
            label: "New File",
            detail: "Create a file under the selected explorer location",
            shortcut: "n",
            action: CommandAction::NewFile,
        },
        CommandSpec {
            label: "New Folder",
            detail: "Create a folder under the selected explorer location",
            shortcut: "N",
            action: CommandAction::NewFolder,
        },
        CommandSpec {
            label: "Rename Selected Explorer Item",
            detail: "Rename the selected file or folder",
            shortcut: "e",
            action: CommandAction::RenameSelected,
        },
        CommandSpec {
            label: "Delete Selected Explorer Item",
            detail: "Delete the selected file or folder after confirmation",
            shortcut: "D",
            action: CommandAction::DeleteSelected,
        },
        CommandSpec {
            label: "Refresh Explorer",
            detail: "Reload the workspace file tree from disk",
            shortcut: "r",
            action: CommandAction::RefreshExplorer,
        },
        CommandSpec {
            label: "Collapse Explorer Folders",
            detail: "Collapse all expanded explorer folders",
            shortcut: "",
            action: CommandAction::CollapseExplorer,
        },
        CommandSpec {
            label: "Reveal Active File in Explorer",
            detail: "Select the active editor file in the explorer tree",
            shortcut: "o",
            action: CommandAction::RevealActiveFile,
        },
        CommandSpec {
            label: "Find in File",
            detail: "Search inside the active editor tab",
            shortcut: "Ctrl-F",
            action: CommandAction::FindInFile,
        },
        CommandSpec {
            label: "Go to Line",
            detail: "Jump to a line or line:column in the active editor tab",
            shortcut: "Ctrl-L",
            action: CommandAction::GotoLine,
        },
        CommandSpec {
            label: "Duplicate Line",
            detail: "Duplicate the current editor line",
            shortcut: "Ctrl-D",
            action: CommandAction::DuplicateLine,
        },
        CommandSpec {
            label: "Delete Line",
            detail: "Delete the current editor line",
            shortcut: "",
            action: CommandAction::DeleteLine,
        },
        CommandSpec {
            label: "Move Line Up",
            detail: "Move the current editor line upward",
            shortcut: "Alt-Up",
            action: CommandAction::MoveLineUp,
        },
        CommandSpec {
            label: "Move Line Down",
            detail: "Move the current editor line downward",
            shortcut: "Alt-Down",
            action: CommandAction::MoveLineDown,
        },
        CommandSpec {
            label: "Toggle Line Comment",
            detail: "Comment or uncomment the current editor line",
            shortcut: "Ctrl-/",
            action: CommandAction::ToggleLineComment,
        },
        CommandSpec {
            label: "Indent Line",
            detail: "Indent the current editor line",
            shortcut: "Tab",
            action: CommandAction::IndentLine,
        },
        CommandSpec {
            label: "Outdent Line",
            detail: "Outdent the current editor line",
            shortcut: "Shift-Tab",
            action: CommandAction::OutdentLine,
        },
        CommandSpec {
            label: "Focus Explorer",
            detail: "Move focus to the file explorer",
            shortcut: "",
            action: CommandAction::FocusExplorer,
        },
        CommandSpec {
            label: "Focus Editor",
            detail: "Move focus to the editor",
            shortcut: "",
            action: CommandAction::FocusEditor,
        },
        CommandSpec {
            label: "Focus Terminal",
            detail: "Move focus to the integrated terminal",
            shortcut: "",
            action: CommandAction::FocusTerminal,
        },
        CommandSpec {
            label: "Clear Terminal",
            detail: "Clear the terminal viewport and scrollback",
            shortcut: "",
            action: CommandAction::ClearTerminal,
        },
        CommandSpec {
            label: "Restart Terminal",
            detail: "Kill the current shell and start a fresh PTY shell",
            shortcut: "",
            action: CommandAction::RestartTerminal,
        },
    ]
}

impl App {
    fn start_prompt(&mut self, kind: PromptKind, initial: &str) {
        self.prompt = Some(PromptState {
            kind,
            input: initial.to_owned(),
        });
    }

    fn open_quick_panel(&mut self, kind: QuickPanelKind) -> Result<()> {
        self.quick_panel = Some(QuickPanel {
            kind,
            query: String::new(),
            items: Vec::new(),
            selected: 0,
            scroll: 0,
        });
        self.refresh_quick_panel()
    }

    fn refresh_quick_panel(&mut self) -> Result<()> {
        let Some(panel) = &self.quick_panel else {
            return Ok(());
        };
        let kind = panel.kind.clone();
        let query = panel.query.clone();
        let items = match kind {
            QuickPanelKind::OpenFile => self.quick_open_items(&query)?,
            QuickPanelKind::WorkspaceSearch => self.workspace_search_items(&query)?,
            QuickPanelKind::CommandPalette => self.command_palette_items(&query),
        };

        if let Some(panel) = &mut self.quick_panel {
            panel.items = items;
            panel.selected = panel.selected.min(panel.items.len().saturating_sub(1));
            panel.scroll = panel.scroll.min(panel.items.len().saturating_sub(1));
            self.ensure_quick_selection_visible();
        }
        Ok(())
    }

    fn quick_open_items(&self, query: &str) -> Result<Vec<QuickItem>> {
        let mut scored = Vec::new();
        for path in collect_workspace_files(&self.root)? {
            let relative = relative_path(&self.root, &path);
            if let Some(score) = fuzzy_score(&relative, query) {
                let label = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("[file]")
                    .to_owned();
                scored.push((
                    score,
                    relative.len(),
                    QuickItem {
                        label,
                        detail: relative,
                        path,
                        line: None,
                        col: None,
                        preview: None,
                        command: None,
                    },
                ));
            }
        }

        scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        Ok(scored
            .into_iter()
            .take(MAX_QUICK_ITEMS)
            .map(|(_, _, item)| item)
            .collect())
    }

    fn workspace_search_items(&self, query: &str) -> Result<Vec<QuickItem>> {
        let needle = query.trim();
        if needle.is_empty() {
            return Ok(Vec::new());
        }

        let needle_lower = needle.to_lowercase();
        let mut items = Vec::new();
        for path in collect_workspace_files(&self.root)? {
            if items.len() >= MAX_QUICK_ITEMS {
                break;
            }
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            if metadata.len() > MAX_FILE_SCAN_BYTES {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            if bytes.contains(&0) {
                continue;
            }

            let text = String::from_utf8_lossy(&bytes);
            for (line_index, line) in text.lines().enumerate() {
                let line_lower = line.to_lowercase();
                let Some(byte_col) = line_lower.find(&needle_lower) else {
                    continue;
                };
                let relative = relative_path(&self.root, &path);
                let col = line[..byte_col].chars().count();
                items.push(QuickItem {
                    label: format!("{}:{}", relative, line_index + 1),
                    detail: relative,
                    path: path.clone(),
                    line: Some(line_index),
                    col: Some(col),
                    preview: Some(line.trim().to_owned()),
                    command: None,
                });
                if items.len() >= MAX_QUICK_ITEMS {
                    break;
                }
            }
        }

        Ok(items)
    }

    fn command_palette_items(&self, query: &str) -> Vec<QuickItem> {
        let mut scored = command_catalog()
            .into_iter()
            .filter_map(|command| {
                let haystack = format!("{} {}", command.label, command.detail);
                fuzzy_score(&haystack, query).map(|score| (score, command))
            })
            .collect::<Vec<_>>();

        scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.label.cmp(b.1.label)));
        scored
            .into_iter()
            .take(MAX_QUICK_ITEMS)
            .map(|(_, command)| QuickItem {
                label: command.label.to_owned(),
                detail: command.detail.to_owned(),
                path: self.root.clone(),
                line: None,
                col: None,
                preview: (!command.shortcut.is_empty()).then(|| command.shortcut.to_owned()),
                command: Some(command.action),
            })
            .collect()
    }

    fn prompt_rename(&mut self) {
        let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() else {
            return;
        };
        self.start_prompt(PromptKind::Rename(node.path), &node.name);
    }

    fn prompt_delete(&mut self) {
        let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() else {
            return;
        };
        self.start_prompt(PromptKind::Delete(node.path), "");
    }

    fn copy_selected_path(&mut self) {
        let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() else {
            return;
        };
        self.explorer_clipboard = Some(ExplorerClipboard {
            action: ClipboardAction::Copy,
            path: node.path.clone(),
        });
        self.message = Some(format!("copied {}", node.path.display()));
    }

    fn cut_selected_path(&mut self) {
        let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() else {
            return;
        };
        if node.path == self.root {
            self.message = Some("refusing to cut workspace root".to_owned());
            return;
        }
        self.explorer_clipboard = Some(ExplorerClipboard {
            action: ClipboardAction::Cut,
            path: node.path.clone(),
        });
        self.message = Some(format!("cut {}", node.path.display()));
    }

    fn paste_into_selected(&mut self) -> Result<()> {
        let Some(clipboard) = self.explorer_clipboard.clone() else {
            self.message = Some("clipboard empty".to_owned());
            return Ok(());
        };
        let source = clipboard.path;
        if !source.exists() {
            self.message = Some("clipboard source no longer exists".to_owned());
            self.explorer_clipboard = None;
            return Ok(());
        }

        let target_dir = self.selected_base_dir();
        let Some(name) = source.file_name() else {
            return Ok(());
        };
        let destination = unique_copy_path(&target_dir.join(name));
        if source.is_dir() && target_dir.starts_with(&source) {
            self.message = Some("cannot paste a folder into itself".to_owned());
            return Ok(());
        }

        let success_message;
        match clipboard.action {
            ClipboardAction::Copy => {
                copy_path_recursive(&source, &destination)?;
                success_message = format!("copied to {}", destination.display());
            }
            ClipboardAction::Cut => {
                fs::rename(&source, &destination)?;
                self.update_open_tabs_for_move(&source, &destination);
                self.explorer_clipboard = None;
                success_message = format!("moved to {}", destination.display());
            }
        }

        self.refresh_explorer()?;
        self.reveal_path(&destination)?;
        self.message = Some(success_message);
        Ok(())
    }

    fn duplicate_selected(&mut self) -> Result<()> {
        let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() else {
            return Ok(());
        };
        if node.path == self.root {
            self.message = Some("refusing to duplicate workspace root".to_owned());
            return Ok(());
        }
        let Some(parent) = node.path.parent() else {
            return Ok(());
        };
        let destination = unique_copy_path(
            &parent.join(
                node.path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(copy_name)
                    .unwrap_or_else(|| "copy".to_owned()),
            ),
        );
        copy_path_recursive(&node.path, &destination)?;
        self.refresh_explorer()?;
        self.reveal_path(&destination)?;
        self.message = Some(format!("duplicated to {}", destination.display()));
        Ok(())
    }

    fn reveal_active_file(&mut self) -> Result<()> {
        let Some(path) = self.active_tab().map(|tab| tab.path.clone()) else {
            self.message = Some("no active file to reveal".to_owned());
            return Ok(());
        };
        self.reveal_path(&path)?;
        self.focus = FocusPanel::Explorer;
        self.message = Some(format!("revealed {}", path.display()));
        Ok(())
    }

    fn finish_prompt(&mut self) -> Result<()> {
        let Some(prompt) = self.prompt.take() else {
            return Ok(());
        };
        match prompt.kind {
            PromptKind::NewFile => self.create_file_from_prompt(prompt.input)?,
            PromptKind::NewDir => self.create_dir_from_prompt(prompt.input)?,
            PromptKind::Rename(path) => self.rename_from_prompt(path, prompt.input)?,
            PromptKind::Delete(path) => {
                if prompt.input == "yes" {
                    self.delete_path(path)?;
                } else {
                    self.message = Some("delete cancelled".to_owned());
                }
            }
            PromptKind::Search => self.search_active(prompt.input),
            PromptKind::GotoLine => self.goto_line_from_prompt(prompt.input),
            PromptKind::QuitDirty => {
                if prompt.input == "quit" {
                    self.should_quit = true;
                } else {
                    self.message = Some("quit cancelled".to_owned());
                }
            }
        }
        Ok(())
    }

    fn selected_base_dir(&self) -> PathBuf {
        let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() else {
            return self.root.clone();
        };
        if node.is_dir {
            node.path
        } else {
            node.path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.root.clone())
        }
    }

    fn create_file_from_prompt(&mut self, name: String) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let path = self.selected_base_dir().join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            fs::write(&path, "")?;
        }
        self.refresh_explorer()?;
        self.open_file(&path);
        self.message = Some(format!("created {}", path.display()));
        Ok(())
    }

    fn create_dir_from_prompt(&mut self, name: String) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let path = self.selected_base_dir().join(name);
        fs::create_dir_all(&path)?;
        self.refresh_explorer()?;
        self.message = Some(format!("created {}", path.display()));
        Ok(())
    }

    fn rename_from_prompt(&mut self, path: PathBuf, name: String) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let new_path = path
            .parent()
            .map(|parent| parent.join(name))
            .unwrap_or_else(|| self.root.join(name));
        fs::rename(&path, &new_path)?;
        self.update_open_tabs_for_move(&path, &new_path);
        self.refresh_explorer()?;
        self.reveal_path(&new_path)?;
        self.message = Some(format!("renamed to {}", new_path.display()));
        Ok(())
    }

    fn delete_path(&mut self, path: PathBuf) -> Result<()> {
        if path == self.root {
            self.message = Some("refusing to delete workspace root".to_owned());
            return Ok(());
        }
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
        self.tabs.retain(|tab| !tab.path.starts_with(&path));
        if self
            .explorer_clipboard
            .as_ref()
            .is_some_and(|clipboard| clipboard.path.starts_with(&path))
        {
            self.explorer_clipboard = None;
        }
        if self.tabs.is_empty() {
            self.active_tab = None;
        } else {
            self.active_tab = Some(self.active_tab.unwrap_or(0).min(self.tabs.len() - 1));
        }
        self.refresh_explorer()?;
        self.message = Some(format!("deleted {}", path.display()));
        Ok(())
    }

    fn update_open_tabs_for_move(&mut self, old_path: &Path, new_path: &Path) {
        for tab in &mut self.tabs {
            if let Ok(relative) = tab.path.strip_prefix(old_path) {
                tab.path = new_path.join(relative);
                tab.title = tab
                    .path
                    .file_name()
                    .and_then(|file_name| file_name.to_str())
                    .unwrap_or("[file]")
                    .to_owned();
            }
        }
    }

    fn reveal_path(&mut self, path: &Path) -> Result<()> {
        self.explorer.reveal(path)?;
        self.ensure_explorer_selection_visible();
        Ok(())
    }

    fn refresh_explorer(&mut self) -> Result<()> {
        let selected = self
            .visible_nodes()
            .get(self.explorer.selected)
            .map(|node| node.path.clone());
        self.explorer.refresh()?;
        if let Some(path) = selected
            && let Some(index) = self
                .visible_nodes()
                .iter()
                .position(|node| node.path == path)
        {
            self.explorer.selected = index;
        }
        self.ensure_explorer_selection_visible();
        self.message = Some("explorer refreshed".to_owned());
        Ok(())
    }

    fn collapse_explorer(&mut self) {
        self.explorer.collapse_all();
        self.explorer.selected = 0;
        self.explorer.scroll = 0;
        self.message = Some("explorer collapsed".to_owned());
    }

    fn save_active_tab(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            match tab.save() {
                Ok(()) => self.message = Some(format!("saved {}", tab.path.display())),
                Err(error) => self.last_error = Some(error.to_string()),
            }
        }
    }

    fn save_all_tabs(&mut self) {
        let mut saved = 0usize;
        for tab in &mut self.tabs {
            if !tab.dirty {
                continue;
            }
            match tab.save() {
                Ok(()) => saved += 1,
                Err(error) => {
                    self.last_error = Some(error.to_string());
                    return;
                }
            }
        }
        self.message = Some(format!("saved {saved} dirty tab(s)"));
    }

    fn search_active(&mut self, needle: String) {
        let needle = needle.trim().to_owned();
        if needle.is_empty() {
            return;
        }
        self.search_needle = Some(needle);
        self.find_next(true);
    }

    fn find_next(&mut self, forward: bool) {
        let Some(needle) = self.search_needle.clone() else {
            self.message = Some("no active search".to_owned());
            return;
        };
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let found = if forward {
            find_forward(tab, &needle)
        } else {
            find_backward(tab, &needle)
        };
        if let Some((line, col)) = found {
            tab.set_cursor(line, col);
            self.ensure_editor_cursor_visible();
            self.message = Some(format!("found '{needle}'"));
        } else {
            self.message = Some(format!("not found: {needle}"));
        }
    }

    fn edit_insert(&mut self, c: char) {
        if let Some(tab) = self.active_tab_mut() {
            tab.insert_char(c);
            self.ensure_editor_cursor_visible();
        }
    }

    fn edit_newline(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.newline();
            self.ensure_editor_cursor_visible();
        }
    }

    fn edit_backspace(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.backspace();
            self.ensure_editor_cursor_visible();
        }
    }

    fn edit_delete(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.delete();
            self.ensure_editor_cursor_visible();
        }
    }

    fn indent_active_line(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.indent_line();
            self.ensure_editor_cursor_visible();
            self.message = Some("indented line".to_owned());
        }
    }

    fn outdent_active_line(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            if tab.outdent_line() {
                self.ensure_editor_cursor_visible();
                self.message = Some("outdented line".to_owned());
            } else {
                self.message = Some("line is not indented".to_owned());
            }
        }
    }

    fn duplicate_active_line(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.duplicate_line();
            self.ensure_editor_cursor_visible();
            self.message = Some("duplicated line".to_owned());
        }
    }

    fn delete_active_line(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.delete_line();
            self.ensure_editor_cursor_visible();
            self.message = Some("deleted line".to_owned());
        }
    }

    fn move_active_line_up(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            if tab.move_line_up() {
                self.ensure_editor_cursor_visible();
                self.message = Some("moved line up".to_owned());
            } else {
                self.message = Some("line already at top".to_owned());
            }
        }
    }

    fn move_active_line_down(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            if tab.move_line_down() {
                self.ensure_editor_cursor_visible();
                self.message = Some("moved line down".to_owned());
            } else {
                self.message = Some("line already at bottom".to_owned());
            }
        }
    }

    fn toggle_active_line_comment(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            if tab.toggle_line_comment() {
                self.ensure_editor_cursor_visible();
                self.message = Some("toggled line comment".to_owned());
            } else {
                self.message = Some("no line comment token for file type".to_owned());
            }
        }
    }

    fn move_editor_cursor(&mut self, line_delta: isize, col_delta: isize) {
        if let Some(tab) = self.active_tab_mut() {
            tab.move_cursor(line_delta, col_delta);
            self.ensure_editor_cursor_visible();
        }
    }

    fn set_editor_cursor_col(&mut self, col: usize) {
        if let Some(tab) = self.active_tab_mut() {
            tab.cursor_col = col;
            tab.clamp_cursor_col();
            self.ensure_editor_cursor_visible();
        }
    }

    fn set_editor_cursor_end(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.cursor_col = tab.lines[tab.cursor_line].chars().count();
            self.ensure_editor_cursor_visible();
        }
    }

    fn ensure_editor_cursor_visible(&mut self) {
        let height = self.editor_height.max(1);
        if let Some(tab) = self.active_tab_mut() {
            if tab.cursor_line < tab.scroll {
                tab.scroll = tab.cursor_line;
            } else if tab.cursor_line >= tab.scroll + height {
                tab.scroll = tab.cursor_line.saturating_sub(height - 1);
            }
        }
    }

    fn set_editor_cursor_from_mouse(&mut self) {
        let Some(body) = self.hit_regions.editor_body else {
            return;
        };
        let HoverTarget::Editor = self.hover else {
            return;
        };
        let mouse_x = self.hit_regions.last_mouse_x;
        let mouse_y = self.hit_regions.last_mouse_y;
        if let Some(tab) = self.active_tab_mut() {
            let line_number_width = tab.lines.len().max(1).to_string().len().max(3) + 2;
            let x = body.x;
            let y = body.y;
            let col = mouse_x
                .saturating_sub(x)
                .saturating_sub(line_number_width as u16) as usize;
            let line = tab.scroll + mouse_y.saturating_sub(y) as usize;
            tab.set_cursor(line, col);
            self.ensure_editor_cursor_visible();
        }
    }

    fn move_quick_selection(&mut self, delta: isize) {
        let Some(panel) = &mut self.quick_panel else {
            return;
        };
        if panel.items.is_empty() {
            return;
        }
        panel.selected = add_signed(panel.selected, delta).min(panel.items.len() - 1);
        self.ensure_quick_selection_visible();
    }

    fn set_quick_selection(&mut self, index: usize) {
        let Some(panel) = &mut self.quick_panel else {
            return;
        };
        if panel.items.is_empty() {
            return;
        }
        panel.selected = index.min(panel.items.len() - 1);
        self.ensure_quick_selection_visible();
    }

    fn ensure_quick_selection_visible(&mut self) {
        let height = self.quick_panel_height.max(1);
        let Some(panel) = &mut self.quick_panel else {
            return;
        };
        if panel.selected < panel.scroll {
            panel.scroll = panel.selected;
        } else if panel.selected >= panel.scroll + height {
            panel.scroll = panel.selected.saturating_sub(height - 1);
        }
    }

    fn activate_quick_row(&mut self, index: usize) {
        if let Some(panel) = &mut self.quick_panel {
            panel.selected = index.min(panel.items.len().saturating_sub(1));
        }
        self.activate_selected_quick_item();
    }

    fn activate_selected_quick_item(&mut self) {
        let Some(item) = self
            .quick_panel
            .as_ref()
            .and_then(|panel| panel.items.get(panel.selected))
            .cloned()
        else {
            return;
        };

        if let Some(command) = item.command {
            self.quick_panel = None;
            if let Err(error) = self.run_command(command) {
                self.last_error = Some(error.to_string());
            }
            return;
        }

        let search_query = self.quick_panel.as_ref().and_then(|panel| {
            (panel.kind == QuickPanelKind::WorkspaceSearch).then(|| panel.query.clone())
        });
        self.quick_panel = None;
        self.open_file(&item.path);
        if let Some(line) = item.line
            && let Some(tab) = self.active_tab_mut()
        {
            tab.set_cursor(line, item.col.unwrap_or(0));
            self.ensure_editor_cursor_visible();
        }
        if let Some(query) = search_query {
            self.search_needle = Some(query);
        }
        self.message = Some(format!("opened {}", item.path.display()));
    }

    fn run_command(&mut self, command: CommandAction) -> Result<()> {
        match command {
            CommandAction::QuickOpen => self.open_quick_panel(QuickPanelKind::OpenFile)?,
            CommandAction::WorkspaceSearch => {
                self.open_quick_panel(QuickPanelKind::WorkspaceSearch)?
            }
            CommandAction::SaveFile => self.save_active_tab(),
            CommandAction::SaveAll => self.save_all_tabs(),
            CommandAction::CloseActiveTab => self.close_active_tab(),
            CommandAction::CloseSavedTabs => self.close_saved_tabs(),
            CommandAction::NewFile => self.start_prompt(PromptKind::NewFile, ""),
            CommandAction::NewFolder => self.start_prompt(PromptKind::NewDir, ""),
            CommandAction::RenameSelected => self.prompt_rename(),
            CommandAction::DeleteSelected => self.prompt_delete(),
            CommandAction::RefreshExplorer => self.refresh_explorer()?,
            CommandAction::CollapseExplorer => self.collapse_explorer(),
            CommandAction::RevealActiveFile => self.reveal_active_file()?,
            CommandAction::FindInFile => {
                let initial = self.search_needle.clone().unwrap_or_default();
                self.start_prompt(PromptKind::Search, &initial);
            }
            CommandAction::GotoLine => self.start_prompt(PromptKind::GotoLine, ""),
            CommandAction::DuplicateLine => self.duplicate_active_line(),
            CommandAction::DeleteLine => self.delete_active_line(),
            CommandAction::MoveLineUp => self.move_active_line_up(),
            CommandAction::MoveLineDown => self.move_active_line_down(),
            CommandAction::ToggleLineComment => self.toggle_active_line_comment(),
            CommandAction::IndentLine => self.indent_active_line(),
            CommandAction::OutdentLine => self.outdent_active_line(),
            CommandAction::FocusExplorer => self.focus = FocusPanel::Explorer,
            CommandAction::FocusEditor => self.focus = FocusPanel::Editor,
            CommandAction::FocusTerminal => self.focus = FocusPanel::Terminal,
            CommandAction::ClearTerminal => {
                self.terminal.clear();
                self.message = Some("terminal cleared".to_owned());
            }
            CommandAction::RestartTerminal => self.restart_terminal()?,
        }
        Ok(())
    }

    fn send_terminal_mouse_click(&mut self) {
        let Some(body) = self.hit_regions.terminal_body else {
            return;
        };
        let row = self.hit_regions.last_mouse_y.saturating_sub(body.y);
        let col = self.hit_regions.last_mouse_x.saturating_sub(body.x);
        let _ = self.terminal.send_mouse_click(row, col);
    }

    fn next_tab(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        let next = self
            .active_tab
            .map_or(0, |index| (index + 1) % self.tabs.len());
        self.active_tab = Some(next);
        self.focus = FocusPanel::Editor;
    }

    fn previous_tab(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        let previous = self
            .active_tab
            .map_or(0, |index| (index + self.tabs.len() - 1) % self.tabs.len());
        self.active_tab = Some(previous);
        self.focus = FocusPanel::Editor;
    }

    fn close_active_tab(&mut self) {
        if let Some(index) = self.active_tab {
            self.close_tab(index);
        }
    }

    fn close_saved_tabs(&mut self) {
        let before = self.tabs.len();
        self.tabs.retain(|tab| tab.dirty);
        let closed = before.saturating_sub(self.tabs.len());
        self.active_tab = if self.tabs.is_empty() { None } else { Some(0) };
        self.message = if self.tabs.is_empty() {
            Some(format!("closed {closed} saved tab(s)"))
        } else {
            Some(format!(
                "closed {closed} saved tab(s); {} dirty tab(s) remain",
                self.tabs.len()
            ))
        };
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if self.tabs[index].dirty {
            self.message = Some(format!("save {} before closing", self.tabs[index].title));
            return;
        }

        let title = self.tabs[index].title.clone();
        self.tabs.remove(index);
        self.active_tab = if self.tabs.is_empty() {
            None
        } else {
            Some(index.saturating_sub(1).min(self.tabs.len() - 1))
        };
        self.message = Some(format!("closed {title}"));
    }

    fn undo_active_tab(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            if tab.undo() {
                self.ensure_editor_cursor_visible();
                self.message = Some("undo".to_owned());
            } else {
                self.message = Some("nothing to undo".to_owned());
            }
        }
    }

    fn redo_active_tab(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            if tab.redo() {
                self.ensure_editor_cursor_visible();
                self.message = Some("redo".to_owned());
            } else {
                self.message = Some("nothing to redo".to_owned());
            }
        }
    }

    fn request_quit(&mut self) {
        if self.tabs.iter().any(|tab| tab.dirty) {
            self.start_prompt(PromptKind::QuitDirty, "");
        } else {
            self.should_quit = true;
        }
    }

    fn goto_line_from_prompt(&mut self, input: String) {
        let Some((line, col)) = parse_line_col(&input) else {
            self.message = Some("enter a line number, optionally line:column".to_owned());
            return;
        };
        if let Some(tab) = self.active_tab_mut() {
            tab.set_cursor(line, col);
            self.ensure_editor_cursor_visible();
            self.focus = FocusPanel::Editor;
            self.message = Some(format!("jumped to line {}", line + 1));
        }
    }

    fn restart_terminal(&mut self) -> Result<()> {
        let _ = self.terminal.kill();
        self.terminal = ShellPanel::new(self.root.clone())?;
        self.focus = FocusPanel::Terminal;
        self.message = Some("terminal restarted".to_owned());
        Ok(())
    }
}

fn byte_index_for_char(s: &str, char_index: usize) -> usize {
    s.char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(s.len())
}

fn find_forward(tab: &EditorTab, needle: &str) -> Option<(usize, usize)> {
    let start_line = tab.cursor_line;
    let start_col = tab.cursor_col.saturating_add(1);
    for line_index in start_line..tab.lines.len() {
        let col = if line_index == start_line {
            start_col
        } else {
            0
        };
        if let Some(found) = line_find_from(&tab.lines[line_index], needle, col) {
            return Some((line_index, found));
        }
    }

    for line_index in 0..=start_line.min(tab.lines.len().saturating_sub(1)) {
        if let Some(found) = line_find_from(&tab.lines[line_index], needle, 0) {
            return Some((line_index, found));
        }
    }

    None
}

fn find_backward(tab: &EditorTab, needle: &str) -> Option<(usize, usize)> {
    let start_line = tab.cursor_line;
    for line_index in (0..=start_line).rev() {
        let col = if line_index == start_line {
            tab.cursor_col
        } else {
            tab.lines[line_index].chars().count()
        };
        if let Some(found) = line_rfind_before(&tab.lines[line_index], needle, col) {
            return Some((line_index, found));
        }
    }

    for line_index in ((start_line + 1)..tab.lines.len()).rev() {
        let col = tab.lines[line_index].chars().count();
        if let Some(found) = line_rfind_before(&tab.lines[line_index], needle, col) {
            return Some((line_index, found));
        }
    }

    None
}

fn line_find_from(line: &str, needle: &str, char_col: usize) -> Option<usize> {
    let byte = byte_index_for_char(line, char_col);
    line[byte..]
        .find(needle)
        .map(|found| line[..byte + found].chars().count())
}

fn line_rfind_before(line: &str, needle: &str, char_col: usize) -> Option<usize> {
    let byte = byte_index_for_char(line, char_col);
    line[..byte]
        .rfind(needle)
        .map(|found| line[..found].chars().count())
}

fn collect_workspace_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_workspace_files_into(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_workspace_files_into(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        entries.push(entry);
    }
    entries.sort_by_key(|a| a.file_name());

    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if should_skip_dir(&path) {
                continue;
            }
            let _ = collect_workspace_files_into(&path, files);
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn should_skip_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    matches!(
        name,
        ".git"
            | ".hg"
            | ".svn"
            | "target"
            | "node_modules"
            | "dist"
            | "build"
            | ".next"
            | ".nuxt"
            | ".cache"
            | "__pycache__"
    )
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn fuzzy_score(candidate: &str, query: &str) -> Option<usize> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Some(candidate.len());
    }

    let candidate_lower = candidate.to_lowercase();
    let mut last_match = 0usize;
    let mut score = 0usize;
    for query_char in query.chars() {
        let haystack = &candidate_lower[last_match..];
        let found = haystack.find(query_char)?;
        score = score.saturating_add(found);
        last_match = last_match.saturating_add(found + query_char.len_utf8());
    }

    let starts_bonus = match candidate_lower.find(&query) {
        Some(0) => 0,
        Some(_) => 10,
        None => 25,
    };
    Some(score.saturating_add(starts_bonus))
}

fn copy_path_recursive(source: &Path, destination: &Path) -> Result<()> {
    if source.is_dir() {
        fs::create_dir_all(destination)?;
        let mut entries = Vec::new();
        for entry in fs::read_dir(source)? {
            entries.push(entry?);
        }
        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            let child_source = entry.path();
            let child_destination = destination.join(entry.file_name());
            copy_path_recursive(&child_source, &child_destination)?;
        }
    } else {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
    }
    Ok(())
}

fn unique_copy_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }

    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("copy");
    let extension = path.extension().and_then(|extension| extension.to_str());

    for index in 1.. {
        let suffix = if index == 1 {
            " copy".to_owned()
        } else {
            format!(" copy {index}")
        };
        let file_name = match extension {
            Some(extension) => format!("{stem}{suffix}.{extension}"),
            None => format!("{stem}{suffix}"),
        };
        let candidate = parent.join(file_name);
        if !candidate.exists() {
            return candidate;
        }
    }

    unreachable!()
}

fn copy_name(name: &str) -> String {
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(name);
    match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) => format!("{stem} copy.{extension}"),
        None => format!("{stem} copy"),
    }
}

fn leading_indent_width(line: &str) -> usize {
    if line.starts_with('\t') {
        return 1;
    }
    line.chars().take_while(|c| *c == ' ').take(4).count()
}

fn comment_token_for_path(path: &Path) -> Option<&'static str> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some(
            "rs" | "js" | "jsx" | "ts" | "tsx" | "go" | "java" | "kt" | "kts" | "c" | "h" | "cc"
            | "cpp" | "hpp" | "cs" | "swift" | "scala" | "php" | "dart",
        ) => Some("//"),
        Some(
            "py" | "rb" | "sh" | "bash" | "zsh" | "fish" | "toml" | "yml" | "yaml" | "ini" | "conf"
            | "dockerfile" | "makefile",
        ) => Some("#"),
        Some("sql") => Some("--"),
        _ => {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_ascii_lowercase);
            match name.as_deref() {
                Some("dockerfile" | "makefile") => Some("#"),
                _ => None,
            }
        }
    }
}

fn parse_line_col(input: &str) -> Option<(usize, usize)> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut parts = trimmed.split([':', ',']);
    let line = parts.next()?.trim().parse::<usize>().ok()?;
    if line == 0 {
        return None;
    }
    let col = parts
        .next()
        .and_then(|part| part.trim().parse::<usize>().ok())
        .unwrap_or(1);
    Some((line - 1, col.saturating_sub(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("tscode-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn editor_undo_redo_and_save_preserves_trailing_newline() {
        let path = temp_file("undo-redo.txt");
        let _ = fs::remove_file(&path);
        fs::write(&path, "alpha\n").unwrap();

        let mut tab = EditorTab::open(path.clone()).unwrap();
        tab.insert_char('X');
        assert_eq!(tab.lines[0], "Xalpha");

        assert!(tab.undo());
        assert_eq!(tab.lines[0], "alpha");

        assert!(tab.redo());
        assert_eq!(tab.lines[0], "Xalpha");

        tab.save().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "Xalpha\n");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn editor_line_commands_modify_real_buffer() {
        let path = temp_file("line-commands.rs");
        let _ = fs::remove_file(&path);
        fs::write(&path, "fn main() {\nprintln!(\"hi\");\n}\n").unwrap();

        let mut tab = EditorTab::open(path.clone()).unwrap();
        tab.set_cursor(1, 0);
        tab.indent_line();
        assert_eq!(tab.lines[1], "    println!(\"hi\");");

        assert!(tab.outdent_line());
        assert_eq!(tab.lines[1], "println!(\"hi\");");

        assert!(tab.toggle_line_comment());
        assert_eq!(tab.lines[1], "// println!(\"hi\");");
        assert!(tab.toggle_line_comment());
        assert_eq!(tab.lines[1], "println!(\"hi\");");

        tab.duplicate_line();
        assert_eq!(tab.lines[2], "println!(\"hi\");");

        assert!(tab.move_line_down());
        assert_eq!(tab.cursor_line, 3);

        tab.delete_line();
        assert_eq!(tab.lines, vec!["fn main() {", "println!(\"hi\");", "}"]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn search_wraps_forward_and_backward() {
        let path = temp_file("search.txt");
        let _ = fs::remove_file(&path);
        fs::write(&path, "alpha\nbeta alpha\n").unwrap();

        let mut tab = EditorTab::open(path.clone()).unwrap();
        tab.set_cursor(0, 0);
        assert_eq!(find_forward(&tab, "alpha"), Some((1, 5)));

        tab.set_cursor(1, 5);
        assert_eq!(find_forward(&tab, "alpha"), Some((0, 0)));
        assert_eq!(find_backward(&tab, "alpha"), Some((0, 0)));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn fuzzy_score_matches_path_fragments_in_order() {
        assert!(fuzzy_score("src/main.rs", "smr").is_some());
        assert!(fuzzy_score("src/main.rs", "zzz").is_none());
    }

    #[test]
    fn command_palette_finds_executable_commands() {
        let root = std::env::temp_dir().join(format!("tscode-test-command-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE))
            .unwrap();
        assert!(
            app.quick_panel
                .as_ref()
                .is_some_and(|panel| panel.kind == QuickPanelKind::CommandPalette)
        );
        app.quick_panel = None;

        let commands = app.command_palette_items("restart term");
        assert!(commands.iter().any(|item| {
            item.command == Some(CommandAction::RestartTerminal) && item.label == "Restart Terminal"
        }));

        let commands = app.command_palette_items("go line");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::GotoLine))
        );
        let _ = app.terminal.kill();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parse_line_col_uses_one_based_input() {
        assert_eq!(parse_line_col("12"), Some((11, 0)));
        assert_eq!(parse_line_col("12:4"), Some((11, 3)));
        assert_eq!(parse_line_col("0"), None);
        assert_eq!(parse_line_col(""), None);
    }

    #[test]
    fn workspace_file_search_reads_real_files() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-workspace-{}", std::process::id()));
        let nested = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            nested.join("main.rs"),
            "fn main() {\n    println!(\"needle\");\n}\n",
        )
        .unwrap();
        fs::write(root.join("README.md"), "other\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        let open_items = app.quick_open_items("main").unwrap();
        assert!(open_items.iter().any(|item| item.detail == "src/main.rs"));

        let search_items = app.workspace_search_items("needle").unwrap();
        assert_eq!(search_items.len(), 1);
        assert_eq!(search_items[0].detail, "src/main.rs");
        assert_eq!(search_items[0].line, Some(1));

        app.open_file(&search_items[0].path);
        assert!(app.active_tab().is_some());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn copy_path_recursive_copies_directories_and_unique_names() {
        let root = std::env::temp_dir().join(format!("tscode-test-copy-{}", std::process::id()));
        let source = root.join("src");
        let nested = source.join("nested");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("file.txt"), "copied").unwrap();

        let destination = root.join("src copy");
        copy_path_recursive(&source, &destination).unwrap();
        assert_eq!(
            fs::read_to_string(destination.join("nested/file.txt")).unwrap(),
            "copied"
        );

        assert_eq!(unique_copy_path(&source), root.join("src copy 2"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cut_paste_updates_open_tab_paths() {
        let root = std::env::temp_dir().join(format!("tscode-test-cut-{}", std::process::id()));
        let src_dir = root.join("src");
        let dst_dir = root.join("dst");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&dst_dir).unwrap();
        fs::write(src_dir.join("main.rs"), "fn main() {}\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let source = canonical_root.join("src/main.rs");
        let destination = canonical_root.join("dst/main.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&source);
        app.explorer_clipboard = Some(ExplorerClipboard {
            action: ClipboardAction::Cut,
            path: source.clone(),
        });
        app.explorer.reveal(&canonical_root.join("dst")).unwrap();
        app.paste_into_selected().unwrap();

        assert!(!source.exists());
        assert!(destination.exists());
        assert_eq!(app.active_tab().unwrap().path, destination);
        let _ = fs::remove_dir_all(root);
    }
}
