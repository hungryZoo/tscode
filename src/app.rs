use std::path::{Path, PathBuf};

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
    pub terminal_area: Option<Rect>,
    pub terminal_input: Option<Rect>,
    pub explorer_rows: Vec<(Rect, usize)>,
    pub tabs: Vec<(Rect, usize)>,
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
        })
    }
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
            terminal: ShellPanel::new(root),
            syntax: SyntaxHighlighter::new(),
            should_quit: false,
            explorer_height: 0,
            editor_height: 0,
            terminal_height: 0,
            last_error: None,
        })
    }

    pub fn visible_nodes(&self) -> Vec<VisibleNode> {
        self.explorer.visible_nodes()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return Ok(());
        }

        match key.code {
            KeyCode::Tab => self.cycle_focus(),
            KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('q') if self.focus != FocusPanel::Terminal => self.should_quit = true,
            _ => match self.focus {
                FocusPanel::Explorer => self.handle_explorer_key(key)?,
                FocusPanel::Editor => self.handle_editor_key(key),
                FocusPanel::Terminal => self.handle_terminal_key(key)?,
            },
        }

        Ok(())
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
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
            _ => {}
        }

        Ok(())
    }

    fn handle_editor_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.scroll_editor(-1),
            KeyCode::Down => self.scroll_editor(1),
            KeyCode::PageUp => self.scroll_editor(-(self.editor_height as isize)),
            KeyCode::PageDown => self.scroll_editor(self.editor_height as isize),
            KeyCode::Left => self.activate_relative_tab(-1),
            KeyCode::Right => self.activate_relative_tab(1),
            _ => {}
        }
    }

    fn handle_terminal_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Enter => {
                if let Err(error) = self.terminal.submit() {
                    self.last_error = Some(error.to_string());
                }
            }
            KeyCode::Backspace => self.terminal.backspace(),
            KeyCode::Up => self.scroll_terminal(-1),
            KeyCode::Down => self.scroll_terminal(1),
            KeyCode::PageUp => self.scroll_terminal(-(self.terminal_height as isize)),
            KeyCode::PageDown => self.scroll_terminal(self.terminal_height as isize),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.terminal.push_char(c);
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
        let max_scroll = self.terminal.max_scroll(self.terminal_height.max(1));
        self.terminal.scroll = add_signed(self.terminal.scroll, amount).min(max_scroll);
    }

    fn activate_relative_tab(&mut self, delta: isize) {
        let len = self.tabs.len();
        let Some(active) = self.active_tab else {
            return;
        };
        if len == 0 {
            return;
        }

        let next = (active as isize + delta).clamp(0, len.saturating_sub(1) as isize);
        self.active_tab = Some(next as usize);
    }
}

fn add_signed(value: usize, amount: isize) -> usize {
    if amount.is_negative() {
        value.saturating_sub(amount.unsigned_abs())
    } else {
        value.saturating_add(amount as usize)
    }
}
