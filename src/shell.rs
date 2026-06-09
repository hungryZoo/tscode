use std::{
    env,
    io::{Read, Write},
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    thread,
};

use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use vt100::{Color as VtColor, MouseProtocolMode};

const SCROLLBACK: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalStyle {
    pub fg: VtColor,
    pub bg: VtColor,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

impl TerminalStyle {
    fn from_cell(cell: &vt100::Cell) -> Self {
        Self {
            fg: cell.fgcolor(),
            bg: cell.bgcolor(),
            bold: cell.bold(),
            dim: cell.dim(),
            italic: cell.italic(),
            underline: cell.underline(),
            inverse: cell.inverse(),
        }
    }

    fn is_default(self) -> bool {
        self == Self::default()
    }
}

impl Default for TerminalStyle {
    fn default() -> Self {
        Self {
            fg: VtColor::Default,
            bg: VtColor::Default,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            inverse: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSpan {
    pub text: String,
    pub style: TerminalStyle,
}

pub struct ShellPanel {
    parser: vt100::Parser,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    rx: Receiver<Vec<u8>>,
    rows: u16,
    cols: u16,
    user_scrollback: usize,
}

impl ShellPanel {
    pub fn new(workspace: PathBuf) -> Result<Self> {
        let rows = 8;
        let cols = 80;
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut command = shell_command();
        command.cwd(workspace);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");

        let child = pair
            .slave
            .spawn_command(command)
            .context("failed to spawn shell in pty")?;
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let master = pair.master;
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            let mut buf = [0_u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            parser: vt100::Parser::new(rows, cols, SCROLLBACK),
            master,
            writer,
            child,
            rx,
            rows,
            cols,
            user_scrollback: 0,
        })
    }

    pub fn drain(&mut self) -> bool {
        let mut changed = false;
        while let Ok(bytes) = self.rx.try_recv() {
            self.parser.process(&bytes);
            if self.user_scrollback == 0 {
                self.parser.screen_mut().set_scrollback(0);
            }
            changed = true;
        }
        changed
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let rows = rows.max(1);
        let cols = cols.max(2);
        if self.rows == rows && self.cols == cols {
            return;
        }

        self.rows = rows;
        self.cols = cols;
        self.parser.screen_mut().set_size(rows, cols);
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    pub fn styled_rows(&self) -> Vec<Vec<TerminalSpan>> {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let mut rendered_rows = Vec::with_capacity(rows as usize);

        for row in 0..rows {
            let mut last_used_col = None;
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let style = TerminalStyle::from_cell(cell);
                if cell.has_contents() || !style.is_default() {
                    last_used_col = Some(if cell.is_wide() {
                        col.saturating_add(1)
                    } else {
                        col
                    });
                }
            }

            let Some(last_used_col) = last_used_col else {
                rendered_rows.push(Vec::new());
                continue;
            };

            let mut spans = Vec::new();
            let mut current_style = None::<TerminalStyle>;
            let mut current_text = String::new();

            for col in 0..=last_used_col.min(cols.saturating_sub(1)) {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }

                let style = TerminalStyle::from_cell(cell);
                let text = if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                };

                if current_style == Some(style) {
                    current_text.push_str(text);
                } else {
                    if let Some(style) = current_style {
                        spans.push(TerminalSpan {
                            text: std::mem::take(&mut current_text),
                            style,
                        });
                    }
                    current_style = Some(style);
                    current_text.push_str(text);
                }
            }

            if let Some(style) = current_style {
                spans.push(TerminalSpan {
                    text: current_text,
                    style,
                });
            }
            rendered_rows.push(spans);
        }

        rendered_rows
    }

    pub fn row_text(&self, row: u16) -> Option<String> {
        self.parser.screen().rows(0, self.cols).nth(row as usize)
    }

    pub fn cursor(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
    }

    pub fn send_key(&mut self, key: KeyEvent) -> Result<()> {
        if let Some(bytes) = key_to_bytes(key) {
            self.writer.write_all(&bytes)?;
            self.writer.flush()?;
        }
        Ok(())
    }

    pub fn send_paste(&mut self, text: &str) -> Result<()> {
        if self.parser.screen().bracketed_paste() {
            self.writer.write_all(b"\x1b[200~")?;
            self.writer.write_all(text.as_bytes())?;
            self.writer.write_all(b"\x1b[201~")?;
        } else {
            self.writer.write_all(text.as_bytes())?;
        }
        self.writer.flush()?;
        Ok(())
    }

    pub fn clear(&mut self) {
        self.parser = vt100::Parser::new(self.rows, self.cols, SCROLLBACK);
        self.user_scrollback = 0;
    }

    pub fn kill(&mut self) -> Result<()> {
        self.child.kill()?;
        Ok(())
    }

    pub fn wants_mouse_events(&self) -> bool {
        !matches!(
            self.parser.screen().mouse_protocol_mode(),
            MouseProtocolMode::None
        )
    }

    pub fn send_mouse_click(&mut self, row: u16, col: u16) -> Result<bool> {
        if !self.wants_mouse_events() {
            return Ok(false);
        }

        let seq = format!(
            "\x1b[<0;{};{}M\x1b[<0;{};{}m",
            col + 1,
            row + 1,
            col + 1,
            row + 1
        );
        self.writer.write_all(seq.as_bytes())?;
        self.writer.flush()?;
        Ok(true)
    }

    pub fn send_mouse_wheel(&mut self, row: u16, col: u16, up: bool) -> Result<bool> {
        if !self.wants_mouse_events() {
            return Ok(false);
        }

        let button = if up { 64 } else { 65 };
        let seq = format!("\x1b[<{};{};{}M", button, col + 1, row + 1);
        self.writer.write_all(seq.as_bytes())?;
        self.writer.flush()?;
        Ok(true)
    }

    pub fn scroll(&mut self, amount: isize) {
        let current = self.parser.screen().scrollback();
        let next = if amount.is_negative() {
            current.saturating_sub(amount.unsigned_abs())
        } else {
            current.saturating_add(amount as usize)
        };
        self.user_scrollback = next;
        self.parser.screen_mut().set_scrollback(next);
        self.user_scrollback = self.parser.screen().scrollback();
    }

    pub fn child_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

#[cfg(windows)]
fn shell_command() -> CommandBuilder {
    let shell = env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_owned());
    CommandBuilder::new(shell)
}

#[cfg(not(windows))]
fn shell_command() -> CommandBuilder {
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
    CommandBuilder::new(shell)
}

fn key_to_bytes(key: KeyEvent) -> Option<Vec<u8>> {
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && let KeyCode::Char(c) = key.code
    {
        return ctrl_byte(c).map(|byte| with_alt(vec![byte], alt));
    }

    let bytes = match key.code {
        KeyCode::Backspace => b"\x7f".to_vec(),
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::Esc => b"\x1b".to_vec(),
        KeyCode::Left => csi_arrow('D', key.modifiers),
        KeyCode::Right => csi_arrow('C', key.modifiers),
        KeyCode::Up => csi_arrow('A', key.modifiers),
        KeyCode::Down => csi_arrow('B', key.modifiers),
        KeyCode::Home => csi_arrow('H', key.modifiers),
        KeyCode::End => csi_arrow('F', key.modifiers),
        KeyCode::PageUp => csi_tilde(5, key.modifiers),
        KeyCode::PageDown => csi_tilde(6, key.modifiers),
        KeyCode::Delete => csi_tilde(3, key.modifiers),
        KeyCode::Insert => csi_tilde(2, key.modifiers),
        KeyCode::Char(c) => c.to_string().into_bytes(),
        _ => return None,
    };

    Some(with_alt(bytes, alt && matches!(key.code, KeyCode::Char(_))))
}

fn ctrl_byte(c: char) -> Option<u8> {
    let c = c.to_ascii_lowercase();
    if c.is_ascii_lowercase() {
        Some(c as u8 - b'a' + 1)
    } else {
        match c {
            '[' => Some(0x1b),
            '\\' => Some(0x1c),
            ']' => Some(0x1d),
            '^' => Some(0x1e),
            '_' => Some(0x1f),
            _ => None,
        }
    }
}

fn with_alt(mut bytes: Vec<u8>, alt: bool) -> Vec<u8> {
    if alt {
        bytes.insert(0, 0x1b);
    }
    bytes
}

fn csi_arrow(final_byte: char, modifiers: KeyModifiers) -> Vec<u8> {
    if let Some(code) = modifier_code(modifiers) {
        format!("\x1b[1;{code}{final_byte}").into_bytes()
    } else {
        format!("\x1b[{final_byte}").into_bytes()
    }
}

fn csi_tilde(number: u8, modifiers: KeyModifiers) -> Vec<u8> {
    if let Some(code) = modifier_code(modifiers) {
        format!("\x1b[{number};{code}~").into_bytes()
    } else {
        format!("\x1b[{number}~").into_bytes()
    }
}

fn modifier_code(modifiers: KeyModifiers) -> Option<u8> {
    let mut code = 1_u8;
    if modifiers.contains(KeyModifiers::SHIFT) {
        code += 1;
    }
    if modifiers.contains(KeyModifiers::ALT) {
        code += 2;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        code += 4;
    }
    (code != 1).then_some(code)
}
