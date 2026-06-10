use std::{
    env, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process,
    sync::mpsc::{self, Receiver},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use vt100::{Color as VtColor, MouseProtocolEncoding, MouseProtocolMode};

const SCROLLBACK: usize = 10_000;
const MAX_OSC_PAYLOAD_BYTES: usize = 4096;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSearchMatch {
    pub row: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellExitStatus {
    pub code: u32,
    pub signal: Option<String>,
    pub success: bool,
}

impl ShellExitStatus {
    fn from_status(status: portable_pty::ExitStatus) -> Self {
        Self {
            code: status.exit_code(),
            signal: status.signal().map(str::to_owned),
            success: status.success(),
        }
    }

    pub fn label(&self) -> String {
        if let Some(signal) = &self.signal {
            format!("signal:{signal}")
        } else {
            format!("exit:{}", self.code)
        }
    }
}

pub struct ShellPanel {
    parser: vt100::Parser,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    rx: Receiver<Vec<u8>>,
    osc: OscTracker,
    integration_dir: Option<PathBuf>,
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

        let shell = shell_program();
        let mut command = CommandBuilder::new(&shell);
        command.cwd(workspace);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        let integration_dir = configure_shell_integration(&shell, &mut command);

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
            osc: OscTracker::default(),
            integration_dir,
            rows,
            cols,
            user_scrollback: 0,
        })
    }

    pub fn drain(&mut self) -> bool {
        let mut changed = false;
        while let Ok(bytes) = self.rx.try_recv() {
            self.osc.process(&bytes);
            self.parser.process(&bytes);
            if self.user_scrollback == 0 {
                self.parser.screen_mut().set_scrollback(0);
            }
            changed = true;
        }
        changed
    }

    #[cfg(test)]
    pub fn process_output_for_test(&mut self, bytes: &[u8]) {
        self.osc.process(bytes);
        self.parser.process(bytes);
    }

    pub fn take_cwd_update(&mut self) -> Option<PathBuf> {
        self.osc.take_cwd_update()
    }

    pub fn take_title_update(&mut self) -> Option<String> {
        self.osc.take_title_update()
    }

    pub fn take_clipboard_update(&mut self) -> Option<String> {
        self.osc.take_clipboard_update()
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

    pub fn visible_top_row(&mut self) -> usize {
        let max_scrollback = self.max_scrollback();
        max_scrollback.saturating_sub(self.parser.screen().scrollback())
    }

    pub fn scroll_to_global_row(&mut self, row: usize) {
        let max_scrollback = self.max_scrollback();
        let offset = max_scrollback.saturating_sub(row);
        self.user_scrollback = offset;
        self.parser.screen_mut().set_scrollback(offset);
        self.user_scrollback = self.parser.screen().scrollback();
    }

    pub fn search_matches(&mut self, needle: &str) -> Vec<TerminalSearchMatch> {
        if needle.is_empty() {
            return Vec::new();
        }

        self.all_row_text()
            .into_iter()
            .enumerate()
            .flat_map(|(row, text)| {
                terminal_line_matches(&text, needle)
                    .into_iter()
                    .map(move |(start, end)| TerminalSearchMatch { row, start, end })
            })
            .collect()
    }

    pub fn cursor(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
    }

    pub fn scrollback(&self) -> usize {
        self.parser.screen().scrollback()
    }

    pub fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    pub fn alternate_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }

    pub fn bracketed_paste(&self) -> bool {
        self.parser.screen().bracketed_paste()
    }

    pub fn mouse_protocol_mode(&self) -> MouseProtocolMode {
        self.parser.screen().mouse_protocol_mode()
    }

    pub fn hide_cursor(&self) -> bool {
        self.parser.screen().hide_cursor()
    }

    pub fn send_key(&mut self, key: KeyEvent) -> Result<()> {
        if let Some(bytes) = key_to_bytes(key, self.parser.screen().application_cursor()) {
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

    pub fn send_text(&mut self, text: &str) -> Result<()> {
        self.writer.write_all(text.as_bytes())?;
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

    pub fn send_mouse_event(
        &mut self,
        kind: MouseEventKind,
        row: u16,
        col: u16,
        modifiers: KeyModifiers,
    ) -> Result<bool> {
        let mode = self.parser.screen().mouse_protocol_mode();
        if mode == MouseProtocolMode::None {
            return Ok(false);
        }

        let Some(bytes) = mouse_event_to_bytes(
            kind,
            row,
            col,
            modifiers,
            mode,
            self.parser.screen().mouse_protocol_encoding(),
        ) else {
            return Ok(true);
        };

        self.writer.write_all(&bytes)?;
        self.writer.flush()?;
        Ok(true)
    }

    pub fn send_mouse_click(&mut self, row: u16, col: u16) -> Result<bool> {
        if !self.send_mouse_event(
            MouseEventKind::Down(MouseButton::Left),
            row,
            col,
            KeyModifiers::empty(),
        )? {
            return Ok(false);
        }
        let _ = self.send_mouse_event(
            MouseEventKind::Up(MouseButton::Left),
            row,
            col,
            KeyModifiers::empty(),
        )?;
        Ok(true)
    }

    pub fn send_mouse_wheel(&mut self, row: u16, col: u16, up: bool) -> Result<bool> {
        let kind = if up {
            MouseEventKind::ScrollUp
        } else {
            MouseEventKind::ScrollDown
        };
        self.send_mouse_event(kind, row, col, KeyModifiers::empty())
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

    fn max_scrollback(&mut self) -> usize {
        let current = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(usize::MAX);
        let max = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(current);
        max
    }

    fn all_row_text(&mut self) -> Vec<String> {
        let current = self.parser.screen().scrollback();
        let max_scrollback = self.max_scrollback();
        let (rows, cols) = self.parser.screen().size();
        let total_rows = max_scrollback + rows as usize;
        let mut output = Vec::with_capacity(total_rows);

        for global_row in 0..total_rows {
            let top_row = global_row.min(max_scrollback);
            let offset = max_scrollback.saturating_sub(top_row);
            self.parser.screen_mut().set_scrollback(offset);
            let local_row = global_row.saturating_sub(top_row);
            let text = self
                .parser
                .screen()
                .rows(0, cols)
                .nth(local_row)
                .unwrap_or_default();
            output.push(text);
        }

        self.parser.screen_mut().set_scrollback(current);
        output
    }

    pub fn child_exit_status(&mut self) -> Option<ShellExitStatus> {
        self.child
            .try_wait()
            .ok()
            .flatten()
            .map(ShellExitStatus::from_status)
    }
}

impl Drop for ShellPanel {
    fn drop(&mut self) {
        if let Some(dir) = &self.integration_dir {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OscState {
    Ground,
    Esc,
    Osc,
    OscEsc,
}

#[derive(Debug)]
struct OscTracker {
    state: OscState,
    buffer: Vec<u8>,
    cwd_update: Option<PathBuf>,
    title_update: Option<String>,
    clipboard_update: Option<String>,
}

impl Default for OscTracker {
    fn default() -> Self {
        Self {
            state: OscState::Ground,
            buffer: Vec::new(),
            cwd_update: None,
            title_update: None,
            clipboard_update: None,
        }
    }
}

impl OscTracker {
    fn process(&mut self, bytes: &[u8]) {
        for byte in bytes {
            match self.state {
                OscState::Ground => {
                    if *byte == 0x1b {
                        self.state = OscState::Esc;
                    }
                }
                OscState::Esc => {
                    if *byte == b']' {
                        self.buffer.clear();
                        self.state = OscState::Osc;
                    } else if *byte != 0x1b {
                        self.state = OscState::Ground;
                    }
                }
                OscState::Osc => match *byte {
                    0x07 => {
                        self.finish_osc();
                        self.state = OscState::Ground;
                    }
                    0x1b => self.state = OscState::OscEsc,
                    _ => self.push_osc_byte(*byte),
                },
                OscState::OscEsc => {
                    if *byte == b'\\' {
                        self.finish_osc();
                        self.state = OscState::Ground;
                    } else {
                        self.push_osc_byte(0x1b);
                        if *byte == 0x1b {
                            self.state = OscState::OscEsc;
                        } else {
                            self.push_osc_byte(*byte);
                            self.state = OscState::Osc;
                        }
                    }
                }
            }
        }
    }

    fn take_cwd_update(&mut self) -> Option<PathBuf> {
        self.cwd_update.take()
    }

    fn take_title_update(&mut self) -> Option<String> {
        self.title_update.take()
    }

    fn take_clipboard_update(&mut self) -> Option<String> {
        self.clipboard_update.take()
    }

    fn push_osc_byte(&mut self, byte: u8) {
        if self.buffer.len() < MAX_OSC_PAYLOAD_BYTES {
            self.buffer.push(byte);
        } else {
            self.buffer.clear();
            self.state = OscState::Ground;
        }
    }

    fn finish_osc(&mut self) {
        if let Ok(payload) = std::str::from_utf8(&self.buffer) {
            if let Some(path) = osc7_path(payload) {
                self.cwd_update = Some(path);
            } else if let Some(title) = osc_title(payload) {
                self.title_update = Some(title);
            } else if let Some(text) = osc52_clipboard_text(payload) {
                self.clipboard_update = Some(text);
            }
        }
        self.buffer.clear();
    }
}

fn terminal_line_matches(line: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }

    let mut matches = Vec::new();
    let mut byte_start = 0usize;
    while byte_start <= line.len() {
        let Some(found) = line[byte_start..].find(needle) else {
            break;
        };
        let start_byte = byte_start + found;
        let end_byte = start_byte + needle.len();
        let start = line[..start_byte].chars().count();
        let end = start + needle.chars().count();
        matches.push((start, end));
        byte_start = end_byte;
    }
    matches
}

fn osc7_path(payload: &str) -> Option<PathBuf> {
    let url = payload.strip_prefix("7;")?.strip_prefix("file://")?;
    let path_start = url.find('/')?;
    let decoded = percent_decode(&url[path_start..])?;
    let path = PathBuf::from(decoded);
    if !path.is_absolute() || !path.is_dir() {
        return None;
    }
    Some(path.canonicalize().unwrap_or(path))
}

fn osc_title(payload: &str) -> Option<String> {
    let title = payload
        .strip_prefix("0;")
        .or_else(|| payload.strip_prefix("2;"))?;
    let title = sanitize_terminal_title(title);
    (!title.is_empty()).then_some(title)
}

fn osc52_clipboard_text(payload: &str) -> Option<String> {
    let payload = payload.strip_prefix("52;")?;
    let (_, encoded) = payload.split_once(';')?;
    let encoded = encoded.trim();
    if encoded.is_empty() || encoded == "?" {
        return None;
    }

    let decoded = BASE64.decode(encoded).ok()?;
    String::from_utf8(decoded).ok()
}

fn sanitize_terminal_title(title: &str) -> String {
    title
        .chars()
        .filter(|ch| !ch.is_control())
        .take(80)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hi = *bytes.get(index + 1)?;
            let lo = *bytes.get(index + 2)?;
            output.push(hex_value(hi)? * 16 + hex_value(lo)?);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(windows)]
fn shell_program() -> String {
    env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_owned())
}

#[cfg(not(windows))]
fn shell_program() -> String {
    env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned())
}

fn configure_shell_integration(shell: &str, command: &mut CommandBuilder) -> Option<PathBuf> {
    command.env("TSCODE_SHELL_INTEGRATION", "1");
    let shell_name = Path::new(shell)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(shell);

    match shell_name {
        "bash" => {
            configure_bash_cwd_tracking(command);
            None
        }
        "zsh" => configure_zsh_cwd_tracking(command),
        _ => None,
    }
}

fn configure_bash_cwd_tracking(command: &mut CommandBuilder) {
    let hook = r#"printf '\033]7;file://%s%s\007' "${HOSTNAME:-localhost}" "$PWD""#;
    let prompt_command = match env::var("PROMPT_COMMAND") {
        Ok(existing) if !existing.trim().is_empty() => format!("{hook}; {existing}"),
        _ => hook.to_owned(),
    };
    command.env("PROMPT_COMMAND", prompt_command);
}

fn configure_zsh_cwd_tracking(command: &mut CommandBuilder) -> Option<PathBuf> {
    let dir = create_shell_integration_dir("zsh")?;
    let original_zdotdir = env::var("ZDOTDIR")
        .ok()
        .or_else(|| env::var("HOME").ok())
        .unwrap_or_else(|| ".".to_owned());

    let zshenv = r#"if [ -n "${TSCODE_ORIGINAL_ZDOTDIR:-}" ] && [ -r "${TSCODE_ORIGINAL_ZDOTDIR}/.zshenv" ]; then
  source "${TSCODE_ORIGINAL_ZDOTDIR}/.zshenv"
fi
"#;
    let zshrc = r#"if [ -n "${TSCODE_ORIGINAL_ZDOTDIR:-}" ] && [ -r "${TSCODE_ORIGINAL_ZDOTDIR}/.zshrc" ]; then
  source "${TSCODE_ORIGINAL_ZDOTDIR}/.zshrc"
fi

__tscode_osc7() {
  printf '\033]7;file://%s%s\007' "${HOST:-localhost}" "$PWD"
}

autoload -Uz add-zsh-hook 2>/dev/null
if whence add-zsh-hook >/dev/null 2>&1; then
  add-zsh-hook precmd __tscode_osc7
else
  precmd_functions+=(__tscode_osc7)
fi
__tscode_osc7
"#;

    if fs::write(dir.join(".zshenv"), zshenv)
        .and_then(|_| fs::write(dir.join(".zshrc"), zshrc))
        .is_err()
    {
        let _ = fs::remove_dir_all(&dir);
        return None;
    }

    command.env("TSCODE_ORIGINAL_ZDOTDIR", original_zdotdir);
    command.env("ZDOTDIR", &dir);
    Some(dir)
}

fn create_shell_integration_dir(label: &str) -> Option<PathBuf> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let dir = env::temp_dir().join(format!("tscode-{label}-{}-{now}", process::id()));
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn key_to_bytes(key: KeyEvent, application_cursor: bool) -> Option<Vec<u8>> {
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && let KeyCode::Char(c) = key.code
    {
        return ctrl_byte(c).map(|byte| with_alt(vec![byte], alt));
    }

    let bytes = match key.code {
        KeyCode::Backspace => backspace_key(key.modifiers),
        KeyCode::Enter => with_alt(b"\r".to_vec(), alt),
        KeyCode::Tab => with_alt(b"\t".to_vec(), alt),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => b"\x1b".to_vec(),
        KeyCode::Left => cursor_key('D', key.modifiers, application_cursor),
        KeyCode::Right => cursor_key('C', key.modifiers, application_cursor),
        KeyCode::Up => cursor_key('A', key.modifiers, application_cursor),
        KeyCode::Down => cursor_key('B', key.modifiers, application_cursor),
        KeyCode::Home => cursor_key('H', key.modifiers, application_cursor),
        KeyCode::End => cursor_key('F', key.modifiers, application_cursor),
        KeyCode::PageUp => csi_tilde(5, key.modifiers),
        KeyCode::PageDown => csi_tilde(6, key.modifiers),
        KeyCode::Delete => csi_tilde(3, key.modifiers),
        KeyCode::Insert => csi_tilde(2, key.modifiers),
        KeyCode::F(number) => function_key(number, key.modifiers)?,
        KeyCode::Null => b"\0".to_vec(),
        KeyCode::Char(c) => with_alt(c.to_string().into_bytes(), alt),
        _ => return None,
    };

    Some(bytes)
}

fn ctrl_byte(c: char) -> Option<u8> {
    let c = c.to_ascii_lowercase();
    if c.is_ascii_lowercase() {
        Some(c as u8 - b'a' + 1)
    } else {
        match c {
            ' ' | '@' => Some(0x00),
            '[' => Some(0x1b),
            '\\' => Some(0x1c),
            ']' => Some(0x1d),
            '^' => Some(0x1e),
            '_' => Some(0x1f),
            '/' | '-' => Some(0x1f),
            '?' => Some(0x7f),
            '2' => Some(0x00),
            '3' => Some(0x1b),
            '4' => Some(0x1c),
            '5' => Some(0x1d),
            '6' => Some(0x1e),
            '7' => Some(0x1f),
            '8' => Some(0x7f),
            _ => None,
        }
    }
}

fn backspace_key(modifiers: KeyModifiers) -> Vec<u8> {
    let mut bytes = if modifiers.contains(KeyModifiers::CONTROL) {
        // Shells using readline/zle bind Ctrl-W to backward-kill-word, which is
        // the closest widely-supported legacy terminal behavior for Ctrl-Backspace.
        vec![0x17]
    } else {
        b"\x7f".to_vec()
    };
    if modifiers.contains(KeyModifiers::ALT) {
        bytes.insert(0, 0x1b);
    }
    bytes
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

fn cursor_key(final_byte: char, modifiers: KeyModifiers, application_cursor: bool) -> Vec<u8> {
    if modifier_code(modifiers).is_some() {
        return csi_arrow(final_byte, modifiers);
    }

    if application_cursor {
        format!("\x1bO{final_byte}").into_bytes()
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

fn function_key(number: u8, modifiers: KeyModifiers) -> Option<Vec<u8>> {
    let final_byte = match number {
        1 => Some('P'),
        2 => Some('Q'),
        3 => Some('R'),
        4 => Some('S'),
        _ => None,
    };

    if let Some(final_byte) = final_byte {
        return Some(if let Some(code) = modifier_code(modifiers) {
            format!("\x1b[1;{code}{final_byte}").into_bytes()
        } else {
            format!("\x1bO{final_byte}").into_bytes()
        });
    }

    let number = match number {
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        _ => return None,
    };
    Some(csi_tilde(number, modifiers))
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

fn mouse_event_to_bytes(
    kind: MouseEventKind,
    row: u16,
    col: u16,
    modifiers: KeyModifiers,
    mode: MouseProtocolMode,
    encoding: MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    let mut code = match kind {
        MouseEventKind::Down(button) => button_code(button),
        MouseEventKind::Up(button) if reports_release(mode) => match encoding {
            MouseProtocolEncoding::Sgr => button_code(button),
            MouseProtocolEncoding::Default | MouseProtocolEncoding::Utf8 => 3,
        },
        MouseEventKind::Drag(button) if reports_drag(mode) => button_code(button) + 32,
        MouseEventKind::Moved if mode == MouseProtocolMode::AnyMotion => 35,
        MouseEventKind::ScrollUp => 64,
        MouseEventKind::ScrollDown => 65,
        MouseEventKind::ScrollLeft => 66,
        MouseEventKind::ScrollRight => 67,
        _ => return None,
    };

    if modifiers.contains(KeyModifiers::SHIFT) {
        code += 4;
    }
    if modifiers.contains(KeyModifiers::ALT) {
        code += 8;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        code += 16;
    }

    let x = col.saturating_add(1);
    let y = row.saturating_add(1);
    let release = matches!(kind, MouseEventKind::Up(_));

    match encoding {
        MouseProtocolEncoding::Sgr => {
            let final_byte = if release { 'm' } else { 'M' };
            Some(format!("\x1b[<{code};{x};{y}{final_byte}").into_bytes())
        }
        MouseProtocolEncoding::Default => default_mouse_sequence(code, x, y),
        MouseProtocolEncoding::Utf8 => utf8_mouse_sequence(code, x, y),
    }
}

fn button_code(button: MouseButton) -> u16 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

fn reports_release(mode: MouseProtocolMode) -> bool {
    matches!(
        mode,
        MouseProtocolMode::PressRelease
            | MouseProtocolMode::ButtonMotion
            | MouseProtocolMode::AnyMotion
    )
}

fn reports_drag(mode: MouseProtocolMode) -> bool {
    matches!(
        mode,
        MouseProtocolMode::ButtonMotion | MouseProtocolMode::AnyMotion
    )
}

fn default_mouse_sequence(code: u16, x: u16, y: u16) -> Option<Vec<u8>> {
    let cb = u8::try_from(code.checked_add(32)?).ok()?;
    let cx = u8::try_from(x.checked_add(32)?).ok()?;
    let cy = u8::try_from(y.checked_add(32)?).ok()?;
    Some(vec![0x1b, b'[', b'M', cb, cx, cy])
}

fn utf8_mouse_sequence(code: u16, x: u16, y: u16) -> Option<Vec<u8>> {
    let mut bytes = vec![0x1b, b'[', b'M'];
    push_utf8_mouse_value(&mut bytes, code)?;
    push_utf8_mouse_value(&mut bytes, x)?;
    push_utf8_mouse_value(&mut bytes, y)?;
    Some(bytes)
}

fn push_utf8_mouse_value(bytes: &mut Vec<u8>, value: u16) -> Option<()> {
    let codepoint = u32::from(value.checked_add(32)?);
    let ch = char::from_u32(codepoint)?;
    let mut buffer = [0_u8; 4];
    bytes.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn terminal_key_encoding_supports_application_cursor_and_function_keys() {
        assert_eq!(
            key_to_bytes(key(KeyCode::Up, KeyModifiers::empty()), false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            key_to_bytes(key(KeyCode::Up, KeyModifiers::empty()), true),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            key_to_bytes(key(KeyCode::Left, KeyModifiers::CONTROL), true),
            Some(b"\x1b[1;5D".to_vec())
        );
        assert_eq!(
            key_to_bytes(key(KeyCode::F(1), KeyModifiers::empty()), false),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            key_to_bytes(key(KeyCode::F(5), KeyModifiers::SHIFT), false),
            Some(b"\x1b[15;2~".to_vec())
        );
        assert_eq!(
            key_to_bytes(key(KeyCode::BackTab, KeyModifiers::SHIFT), false),
            Some(b"\x1b[Z".to_vec())
        );
    }

    #[test]
    fn terminal_key_encoding_supports_shell_editing_shortcuts() {
        assert_eq!(
            key_to_bytes(key(KeyCode::Backspace, KeyModifiers::empty()), false),
            Some(b"\x7f".to_vec())
        );
        assert_eq!(
            key_to_bytes(key(KeyCode::Backspace, KeyModifiers::ALT), false),
            Some(b"\x1b\x7f".to_vec())
        );
        assert_eq!(
            key_to_bytes(key(KeyCode::Backspace, KeyModifiers::CONTROL), false),
            Some(vec![0x17])
        );
        assert_eq!(
            key_to_bytes(
                key(
                    KeyCode::Backspace,
                    KeyModifiers::CONTROL | KeyModifiers::ALT
                ),
                false,
            ),
            Some(b"\x1b\x17".to_vec())
        );
        assert_eq!(
            key_to_bytes(key(KeyCode::Enter, KeyModifiers::ALT), false),
            Some(b"\x1b\r".to_vec())
        );
        assert_eq!(
            key_to_bytes(key(KeyCode::Tab, KeyModifiers::ALT), false),
            Some(b"\x1b\t".to_vec())
        );
    }

    #[test]
    fn terminal_key_encoding_supports_control_punctuation() {
        assert_eq!(
            key_to_bytes(key(KeyCode::Char('/'), KeyModifiers::CONTROL), false),
            Some(vec![0x1f])
        );
        assert_eq!(
            key_to_bytes(key(KeyCode::Char('6'), KeyModifiers::CONTROL), false),
            Some(vec![0x1e])
        );
        assert_eq!(
            key_to_bytes(key(KeyCode::Char('8'), KeyModifiers::CONTROL), false),
            Some(vec![0x7f])
        );
        assert_eq!(
            key_to_bytes(
                key(
                    KeyCode::Char('3'),
                    KeyModifiers::CONTROL | KeyModifiers::ALT
                ),
                false
            ),
            Some(b"\x1b\x1b".to_vec())
        );
    }

    #[test]
    fn terminal_line_matches_return_character_columns() {
        assert_eq!(
            terminal_line_matches("alpha beta alpha", "alpha"),
            vec![(0, 5), (11, 16)]
        );
        assert_eq!(terminal_line_matches("écho alpha", "alpha"), vec![(5, 10)]);
        assert!(terminal_line_matches("alpha", "").is_empty());
    }

    #[test]
    fn shell_exit_status_labels_codes_and_signals() {
        let success = ShellExitStatus::from_status(portable_pty::ExitStatus::with_exit_code(0));
        assert_eq!(success.label(), "exit:0");
        assert!(success.success);

        let failure = ShellExitStatus::from_status(portable_pty::ExitStatus::with_exit_code(7));
        assert_eq!(failure.label(), "exit:7");
        assert!(!failure.success);

        let signal = ShellExitStatus::from_status(portable_pty::ExitStatus::with_signal("TERM"));
        assert_eq!(signal.label(), "signal:TERM");
        assert!(!signal.success);
    }

    #[test]
    fn osc7_tracker_reads_cwd_from_bel_and_st_sequences() {
        let root = std::env::temp_dir().join(format!("tscode-test-osc7-{}", std::process::id()));
        let space_dir = root.join("space dir");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&space_dir).unwrap();
        let canonical = space_dir.canonicalize().unwrap();
        let encoded_path = canonical.to_string_lossy().replace(' ', "%20");

        let mut tracker = OscTracker::default();
        tracker.process(b"ignored\x1b]7;file://localhost");
        tracker.process(format!("{encoded_path}\x07tail").as_bytes());
        assert_eq!(tracker.take_cwd_update(), Some(canonical.clone()));

        tracker.process(format!("\x1b]7;file://host{encoded_path}\x1b\\").as_bytes());
        assert_eq!(tracker.take_cwd_update(), Some(canonical));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn osc_tracker_reads_terminal_titles_from_osc_zero_and_two() {
        let mut tracker = OscTracker::default();
        tracker.process(b"\x1b]0;demo terminal\x07");
        assert_eq!(
            tracker.take_title_update(),
            Some("demo terminal".to_owned())
        );

        tracker.process(b"\x1b]2; build\tlogs \x1b\\");
        assert_eq!(tracker.take_title_update(), Some("buildlogs".to_owned()));

        tracker.process(b"\x1b]1;ignored\x07");
        assert_eq!(tracker.take_title_update(), None);
    }

    #[test]
    fn osc_tracker_reads_osc52_clipboard_updates() {
        let mut tracker = OscTracker::default();
        tracker.process(b"\x1b]52;c;aGVsbG8gZnJvbSBwdHk=\x07");
        assert_eq!(
            tracker.take_clipboard_update(),
            Some("hello from pty".to_owned())
        );

        tracker.process(b"\x1b]52;p;dHNjb2Rl\x1b\\");
        assert_eq!(tracker.take_clipboard_update(), Some("tscode".to_owned()));

        tracker.process(b"\x1b]52;c;?\x07");
        assert_eq!(tracker.take_clipboard_update(), None);
    }

    #[test]
    fn terminal_mouse_encoding_reports_sgr_press_release_drag_and_wheel() {
        assert_eq!(
            mouse_event_to_bytes(
                MouseEventKind::Down(MouseButton::Left),
                4,
                9,
                KeyModifiers::empty(),
                MouseProtocolMode::PressRelease,
                MouseProtocolEncoding::Sgr,
            ),
            Some(b"\x1b[<0;10;5M".to_vec())
        );
        assert_eq!(
            mouse_event_to_bytes(
                MouseEventKind::Up(MouseButton::Left),
                4,
                9,
                KeyModifiers::empty(),
                MouseProtocolMode::PressRelease,
                MouseProtocolEncoding::Sgr,
            ),
            Some(b"\x1b[<0;10;5m".to_vec())
        );
        assert_eq!(
            mouse_event_to_bytes(
                MouseEventKind::Drag(MouseButton::Right),
                0,
                0,
                KeyModifiers::CONTROL,
                MouseProtocolMode::ButtonMotion,
                MouseProtocolEncoding::Sgr,
            ),
            Some(b"\x1b[<50;1;1M".to_vec())
        );
        assert_eq!(
            mouse_event_to_bytes(
                MouseEventKind::ScrollDown,
                2,
                3,
                KeyModifiers::SHIFT,
                MouseProtocolMode::Press,
                MouseProtocolEncoding::Sgr,
            ),
            Some(b"\x1b[<69;4;3M".to_vec())
        );
    }

    #[test]
    fn terminal_mouse_encoding_respects_requested_motion_modes() {
        assert_eq!(
            mouse_event_to_bytes(
                MouseEventKind::Drag(MouseButton::Left),
                1,
                1,
                KeyModifiers::empty(),
                MouseProtocolMode::PressRelease,
                MouseProtocolEncoding::Sgr,
            ),
            None
        );
        assert_eq!(
            mouse_event_to_bytes(
                MouseEventKind::Moved,
                1,
                1,
                KeyModifiers::empty(),
                MouseProtocolMode::AnyMotion,
                MouseProtocolEncoding::Sgr,
            ),
            Some(b"\x1b[<35;2;2M".to_vec())
        );
    }

    #[test]
    fn terminal_mouse_encoding_uses_real_utf8_coordinates_when_requested() {
        assert_eq!(
            mouse_event_to_bytes(
                MouseEventKind::Down(MouseButton::Left),
                0,
                200,
                KeyModifiers::empty(),
                MouseProtocolMode::Press,
                MouseProtocolEncoding::Utf8,
            ),
            Some(b"\x1b[M \xc3\xa9!".to_vec())
        );

        assert_eq!(
            mouse_event_to_bytes(
                MouseEventKind::Down(MouseButton::Left),
                0,
                200,
                KeyModifiers::empty(),
                MouseProtocolMode::Press,
                MouseProtocolEncoding::Default,
            ),
            Some(vec![0x1b, b'[', b'M', 32, 233, 33])
        );
    }
}
