mod app;
mod fs_tree;
mod lsp;
mod shell;
mod syntax;
mod ui;

use std::{
    any::Any,
    backtrace::Backtrace,
    env,
    ffi::OsString,
    fs, io,
    io::Write,
    panic::{self, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

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
    let panic_report = install_panic_reporter();
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        run(&mut terminal.terminal, App::new(root)?)
    }));
    let restore_result = terminal.restore();
    match result {
        Ok(result) => {
            restore_result?;
            result
        }
        Err(payload) => {
            let report = panic_report
                .lock()
                .ok()
                .and_then(|report| report.clone())
                .unwrap_or_else(|| {
                    let timestamp = current_unix_time();
                    let time_utc = format_unix_timestamp_utc(timestamp);
                    format!(
                        "{}\nversion: {}\ntime_utc: {}\nunix_time: {}\nlocation: unknown\npanic: {}\nbacktrace:\n{}\n",
                        crash_report_header(&time_utc),
                        env!("CARGO_PKG_VERSION"),
                        time_utc,
                        timestamp,
                        panic_payload_message(payload.as_ref()),
                        Backtrace::force_capture()
                    )
                });
            match write_crash_report(&report) {
                Ok(path) => match restore_result {
                    Ok(()) => Err(anyhow!(
                        "tscode crashed; terminal restored; crash report written to {}",
                        path.display()
                    )),
                    Err(error) => Err(anyhow!(
                        "tscode crashed; terminal restore failed: {error}; crash report written to {}",
                        path.display()
                    )),
                },
                Err(error) => match restore_result {
                    Ok(()) => Err(anyhow!(
                        "tscode crashed; terminal restored; failed to write crash report: {error}; {report}"
                    )),
                    Err(restore_error) => Err(anyhow!(
                        "tscode crashed; terminal restore failed: {restore_error}; failed to write crash report: {error}; {report}"
                    )),
                },
            }
        }
    }
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
    app.repair_runtime_state()?;
    terminal.draw(|frame| ui::draw(frame, &mut app))?;

    while !app.should_quit {
        app.repair_runtime_state()?;
        let terminal_changed = app.drain_terminal();
        let files_changed = app.check_external_file_changes();
        let tree_changed = app.check_workspace_tree_changes();
        if terminal_changed || files_changed || tree_changed {
            app.repair_runtime_state()?;
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
            app.repair_runtime_state()?;
            terminal.draw(|frame| ui::draw(frame, &mut app))?;
            flush_clipboard_export(terminal, &mut app)?;
        }
    }

    Ok(())
}

fn install_panic_reporter() -> Arc<Mutex<Option<String>>> {
    let report = Arc::new(Mutex::new(None));
    let report_for_hook = Arc::clone(&report);
    panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|location| format!("{}:{}", location.file(), location.line()))
            .unwrap_or_else(|| "unknown".to_owned());
        let message = panic_payload_message(info.payload());
        let timestamp = current_unix_time();
        let time_utc = format_unix_timestamp_utc(timestamp);
        let text = format!(
            "{}\nversion: {}\ntime_utc: {}\nunix_time: {}\nlocation: {location}\npanic: {message}\nbacktrace:\n{}\n",
            crash_report_header(&time_utc),
            env!("CARGO_PKG_VERSION"),
            time_utc,
            timestamp,
            Backtrace::force_capture()
        );
        if let Ok(mut report) = report_for_hook.lock() {
            *report = Some(text);
        }
    }));
    report
}

fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

fn write_crash_report(report: &str) -> Result<PathBuf> {
    let path = crash_report_path();
    append_crash_report_to_path(&path, report)?;
    Ok(path)
}

fn append_crash_report_to_path(path: &Path, report: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut entry = report.to_owned();
    if !entry.ends_with('\n') {
        entry.push('\n');
    }
    if !entry.ends_with("\n\n") {
        entry.push('\n');
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(entry.as_bytes())?;
    Ok(())
}

fn crash_report_path() -> PathBuf {
    if let Some(cache_home) = env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(cache_home).join("tscode").join("crash.log");
    }
    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".cache")
            .join("tscode")
            .join("crash.log");
    }
    env::temp_dir().join("tscode-crash.log")
}

fn crash_report_header(time_utc: &str) -> String {
    format!("--- tscode panic {time_utc} ---")
}

fn current_unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn format_unix_timestamp_utc(timestamp: u64) -> String {
    let days = (timestamp / 86_400) as i64;
    let seconds_of_day = timestamp % 86_400;
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_from_days(days_since_unix_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (year as i32, month as u32, day as u32)
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

    #[test]
    fn crash_report_timestamp_formats_utc() {
        assert_eq!(format_unix_timestamp_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(
            format_unix_timestamp_utc(1_781_141_921),
            "2026-06-11T01:38:41Z"
        );
    }

    #[test]
    fn crash_report_writer_appends_entries_with_separator() {
        let path = env::temp_dir().join(format!(
            "tscode-test-crash-report-{}.log",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);

        append_crash_report_to_path(&path, "first").unwrap();
        append_crash_report_to_path(&path, "second\n").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "first\n\nsecond\n\n");

        let _ = fs::remove_file(path);
    }
}
