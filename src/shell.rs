use std::{
    env,
    path::PathBuf,
    process::{Command, Stdio},
};

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct ShellPanel {
    pub workspace: PathBuf,
    pub input: String,
    pub lines: Vec<String>,
    pub scroll: usize,
}

impl ShellPanel {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            input: String::new(),
            lines: vec![
                "tscode integrated terminal".to_owned(),
                "Type a command and press Enter.".to_owned(),
            ],
            scroll: 0,
        }
    }

    pub fn push_char(&mut self, c: char) {
        self.input.push(c);
    }

    pub fn backspace(&mut self) {
        self.input.pop();
    }

    pub fn submit(&mut self) -> Result<()> {
        let command = self.input.trim().to_owned();
        self.lines.push(format!("$ {}", command));
        self.input.clear();

        if command.is_empty() {
            self.scroll_to_bottom(1);
            return Ok(());
        }

        let output = platform_shell(&command)
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .output()?;

        append_output(&mut self.lines, &String::from_utf8_lossy(&output.stdout));
        append_output(&mut self.lines, &String::from_utf8_lossy(&output.stderr));

        if !output.status.success() {
            self.lines.push(format!(
                "[exit status: {}]",
                output.status.code().unwrap_or(-1)
            ));
        }

        self.scroll_to_bottom(1);
        Ok(())
    }

    pub fn max_scroll(&self, height: usize) -> usize {
        self.lines.len().saturating_sub(height.max(1))
    }

    pub fn scroll_to_bottom(&mut self, height: usize) {
        self.scroll = self.max_scroll(height);
    }
}

#[cfg(windows)]
fn platform_shell(command: &str) -> Command {
    let mut shell = Command::new("cmd");
    shell.arg("/C").arg(command);
    shell
}

#[cfg(not(windows))]
fn platform_shell(command: &str) -> Command {
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
    let mut command_builder = Command::new(shell);
    command_builder.arg("-lc").arg(command);
    command_builder
}

fn append_output(lines: &mut Vec<String>, text: &str) {
    for line in text.lines() {
        lines.push(line.to_owned());
    }
}
