mod app;
mod fs_tree;
mod shell;
mod syntax;
mod ui;

use std::{env, ffi::OsString, io, io::Write, path::PathBuf, time::Duration};

use anyhow::{Result, anyhow};
use app::App;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

fn main() -> Result<()> {
    let root = match cli_action(env::args_os().skip(1))? {
        CliAction::Run(root) => root,
        CliAction::Help => {
            println!("{}", help_text());
            return Ok(());
        }
        CliAction::Version => {
            println!("tscode {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
    };

    let mut terminal = TerminalSession::enter()?;
    let result = run(&mut terminal.terminal, App::new(root)?);
    terminal.restore()?;
    result
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CliAction {
    Run(PathBuf),
    Help,
    Version,
}

fn cli_action(args: impl IntoIterator<Item = OsString>) -> Result<CliAction> {
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Ok(CliAction::Run(env::current_dir()?));
    };

    if first == "--help" || first == "-h" {
        return Ok(CliAction::Help);
    }
    if first == "--version" || first == "-V" {
        return Ok(CliAction::Version);
    }
    if first == "--" {
        return Ok(CliAction::Run(
            args.next()
                .map(PathBuf::from)
                .unwrap_or(env::current_dir()?),
        ));
    }
    if first.to_string_lossy().starts_with('-') {
        return Err(anyhow!(
            "unknown option '{}'; try 'tscode --help'",
            first.to_string_lossy()
        ));
    }

    Ok(CliAction::Run(PathBuf::from(first)))
}

fn help_text() -> String {
    format!(
        "tscode {}\n\nUSAGE:\n    tscode [path]\n    tscode --help\n    tscode --version\n\nARGS:\n    [path]    Workspace file or directory to open. Defaults to the current directory.\n\nOPTIONS:\n    -h, --help       Show this help text without entering the TUI\n    -V, --version    Show the version without entering the TUI\n\nInside the TUI, use F1 for commands, Ctrl-Q to quit, and F6 or Ctrl-` to move focus in or out of the integrated terminal.",
        env!("CARGO_PKG_VERSION")
    )
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

    #[test]
    fn cli_action_handles_help_version_and_paths_before_tui_start() {
        assert_eq!(
            cli_action([OsString::from("--help")]).unwrap(),
            CliAction::Help
        );
        assert_eq!(
            cli_action([OsString::from("-V")]).unwrap(),
            CliAction::Version
        );
        assert_eq!(
            cli_action([OsString::from("src")]).unwrap(),
            CliAction::Run(PathBuf::from("src"))
        );
        assert_eq!(
            cli_action([OsString::from("--"), OsString::from("-workspace")]).unwrap(),
            CliAction::Run(PathBuf::from("-workspace"))
        );
        assert!(cli_action([OsString::from("--wat")]).is_err());
        assert!(help_text().contains("tscode --version"));
    }
}
