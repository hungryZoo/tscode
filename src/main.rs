mod app;
mod fs_tree;
mod shell;
mod syntax;
mod ui;

use std::{env, io, io::Write, path::PathBuf, time::Duration};

use anyhow::Result;
use app::App;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

fn main() -> Result<()> {
    let root = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(env::current_dir()?);

    let mut terminal = TerminalSession::enter()?;
    let result = run(&mut terminal.terminal, App::new(root)?);
    terminal.restore()?;
    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, mut app: App) -> Result<()> {
    terminal.draw(|frame| ui::draw(frame, &mut app))?;

    while !app.should_quit {
        if app.drain_terminal() {
            terminal.draw(|frame| ui::draw(frame, &mut app))?;
        }

        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) => app.handle_key(key)?,
                Event::Mouse(mouse) => app.handle_mouse(mouse)?,
                Event::Resize(_, _) => {
                    terminal.autoresize()?;
                }
                Event::Paste(text) => app.handle_paste(text)?,
                Event::FocusGained | Event::FocusLost => {}
            }
            terminal.draw(|frame| ui::draw(frame, &mut app))?;
            flush_clipboard_export(terminal, &mut app)?;
        }
    }

    Ok(())
}

fn flush_clipboard_export(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    let Some(text) = app.take_clipboard_export() else {
        return Ok(());
    };

    let backend = terminal.backend_mut();
    write!(backend, "{}", osc52_clipboard_sequence(&text))?;
    backend.flush()?;
    Ok(())
}

fn osc52_clipboard_sequence(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", BASE64.encode(text.as_bytes()))
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    restored: bool,
}

impl TerminalSession {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        write!(stdout, "\x1b[?1003h")?;
        stdout.flush()?;

        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;

        Ok(Self {
            terminal,
            restored: false,
        })
    }

    fn restore(&mut self) -> Result<()> {
        if !self.restored {
            disable_raw_mode()?;
            let backend = self.terminal.backend_mut();
            write!(backend, "\x1b[?1003l")?;
            execute!(backend, DisableMouseCapture, LeaveAlternateScreen)?;
            self.terminal.show_cursor()?;
            self.restored = true;
        }

        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_clipboard_sequence_encodes_text_as_base64() {
        assert_eq!(osc52_clipboard_sequence("hello"), "\x1b]52;c;aGVsbG8=\x07");
    }
}
