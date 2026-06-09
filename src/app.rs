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
    pub last_mouse_x: u16,
    pub last_mouse_y: u16,
}

impl HitRegions {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn target_at(&self, x: u16, y: u16) -> HoverTarget {
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
pub struct EditorTab {
    pub path: PathBuf,
    pub title: String,
    pub lines: Vec<String>,
    pub scroll: usize,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub dirty: bool,
}

impl EditorTab {
    fn open(path: PathBuf) -> Result<Self> {
        let bytes = std::fs::read(&path)?;
        let text = String::from_utf8_lossy(&bytes);
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
        })
    }

    fn save(&mut self) -> Result<()> {
        let text = self.lines.join("\n");
        fs::write(&self.path, text)?;
        self.dirty = false;
        Ok(())
    }

    fn insert_char(&mut self, c: char) {
        let cursor_col = self.cursor_col;
        let line = self.current_line_mut();
        let byte = byte_index_for_char(line, cursor_col);
        line.insert(byte, c);
        self.cursor_col += 1;
        self.dirty = true;
    }

    fn newline(&mut self) {
        let cursor_col = self.cursor_col;
        let line = self.current_line_mut();
        let byte = byte_index_for_char(line, cursor_col);
        let rest = line.split_off(byte);
        self.cursor_line += 1;
        self.cursor_col = 0;
        self.lines.insert(self.cursor_line, rest);
        self.dirty = true;
    }

    fn backspace(&mut self) {
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
        if self.cursor_col < line_len {
            let cursor_col = self.cursor_col;
            let line = self.current_line_mut();
            let start = byte_index_for_char(line, cursor_col);
            let end = byte_index_for_char(line, cursor_col + 1);
            line.replace_range(start..end, "");
            self.dirty = true;
        } else if self.cursor_line + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_line + 1);
            self.lines[self.cursor_line].push_str(&next);
            self.dirty = true;
        }
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    NewFile,
    NewDir,
    Rename(PathBuf),
    Delete(PathBuf),
    Search,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptState {
    pub kind: PromptKind,
    pub input: String,
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
            message: Some("Tab focus | Explorer: n file, N dir, e rename, D delete | Editor: Ctrl-S save | Terminal: Ctrl-Q quit".to_owned()),
        })
    }

    pub fn visible_nodes(&self) -> Vec<VisibleNode> {
        self.explorer.visible_nodes()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        if self.prompt.is_some() {
            return self.handle_prompt_key(key);
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
            self.should_quit = true;
            return Ok(());
        }

        match key.code {
            KeyCode::Tab if !matches!(self.focus, FocusPanel::Terminal) => self.cycle_focus(),
            KeyCode::Esc if !matches!(self.focus, FocusPanel::Terminal) => self.should_quit = true,
            KeyCode::Char('q') if self.focus != FocusPanel::Terminal => self.should_quit = true,
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
            HoverTarget::Tab(_) | HoverTarget::Editor => {
                self.focus = FocusPanel::Editor;
            }
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
            _ => {}
        }

        Ok(())
    }

    fn handle_editor_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('s') => self.save_active_tab(),
                KeyCode::Char('f') => self.start_prompt(PromptKind::Search, ""),
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

    fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            FocusPanel::Explorer => FocusPanel::Editor,
            FocusPanel::Editor => FocusPanel::Terminal,
            FocusPanel::Terminal => FocusPanel::Explorer,
        };
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
            HoverTarget::Editor | HoverTarget::Tab(_) => self.scroll_editor(amount),
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
}

fn add_signed(value: usize, amount: isize) -> usize {
    if amount.is_negative() {
        value.saturating_sub(amount.unsigned_abs())
    } else {
        value.saturating_add(amount as usize)
    }
}

impl App {
    fn start_prompt(&mut self, kind: PromptKind, initial: &str) {
        self.prompt = Some(PromptState {
            kind,
            input: initial.to_owned(),
        });
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
        self.start_prompt(PromptKind::Delete(node.path), "type yes");
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
        for tab in &mut self.tabs {
            if tab.path == path {
                tab.path = new_path.clone();
                tab.title = name.to_owned();
            }
        }
        self.refresh_explorer()?;
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
        self.tabs.retain(|tab| tab.path != path);
        if self.tabs.is_empty() {
            self.active_tab = None;
        } else {
            self.active_tab = Some(self.active_tab.unwrap_or(0).min(self.tabs.len() - 1));
        }
        self.refresh_explorer()?;
        self.message = Some(format!("deleted {}", path.display()));
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

    fn save_active_tab(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            match tab.save() {
                Ok(()) => self.message = Some(format!("saved {}", tab.path.display())),
                Err(error) => self.last_error = Some(error.to_string()),
            }
        }
    }

    fn search_active(&mut self, needle: String) {
        if needle.is_empty() {
            return;
        }
        if let Some(tab) = self.active_tab_mut() {
            for (line_index, line) in tab.lines.iter().enumerate().skip(tab.cursor_line) {
                let start_col = if line_index == tab.cursor_line {
                    tab.cursor_col.saturating_add(1)
                } else {
                    0
                };
                if let Some(byte) = line
                    .char_indices()
                    .nth(start_col)
                    .map(|(idx, _)| idx)
                    .or(Some(line.len()))
                    && let Some(found) = line[byte..].find(&needle)
                {
                    let prefix = &line[..byte + found];
                    tab.set_cursor(line_index, prefix.chars().count());
                    self.ensure_editor_cursor_visible();
                    self.message = Some(format!("found '{needle}'"));
                    return;
                }
            }
        }
        self.message = Some(format!("not found: {needle}"));
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

    fn send_terminal_mouse_click(&mut self) {
        let Some(body) = self.hit_regions.terminal_body else {
            return;
        };
        let row = self.hit_regions.last_mouse_y.saturating_sub(body.y);
        let col = self.hit_regions.last_mouse_x.saturating_sub(body.x);
        let _ = self.terminal.send_mouse_click(row, col);
    }
}

fn byte_index_for_char(s: &str, char_index: usize) -> usize {
    s.char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(s.len())
}
