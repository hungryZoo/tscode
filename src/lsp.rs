use std::{
    env, fs,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use serde_json::{Value, json};

const REQUEST_TIMEOUT: Duration = Duration::from_millis(2_500);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);

fn request_timeout() -> Duration {
    if cfg!(test) {
        Duration::from_secs(10)
    } else {
        REQUEST_TIMEOUT
    }
}

#[derive(Debug, Clone)]
pub struct DocumentPosition {
    pub root: PathBuf,
    pub path: PathBuf,
    pub text: String,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspHover {
    pub contents: String,
    pub server: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspLocation {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
    pub preview: Option<String>,
    pub server: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspCompletion {
    pub label: String,
    pub detail: Option<String>,
    pub insert_text: Option<String>,
    pub server: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspCodeAction {
    pub title: String,
    pub kind: Option<String>,
    pub is_preferred: bool,
    pub edit: Option<LspWorkspaceEdit>,
    pub command_title: Option<String>,
    pub command: Option<LspCommand>,
    pub server: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspCommand {
    pub title: String,
    pub command: String,
    pub arguments: Vec<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspDiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
    Unknown,
}

impl LspDiagnosticSeverity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Information => "note",
            Self::Hint => "help",
            Self::Unknown => "problem",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspDiagnostic {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
    pub severity: LspDiagnosticSeverity,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
    pub server: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspWorkspaceEdit {
    pub edits: Vec<LspTextEdit>,
    pub server: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspTextEdit {
    pub path: PathBuf,
    pub start_line: usize,
    pub start_utf16_col: usize,
    pub end_line: usize,
    pub end_utf16_col: usize,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LanguageServerConfig {
    name: String,
    command: String,
    args: Vec<String>,
    language_id: String,
}

pub fn server_name_for_path(path: &Path) -> Option<String> {
    language_server_for_path(path).map(|config| config.name)
}

pub fn hover(position: &DocumentPosition) -> Result<Option<LspHover>> {
    let Some(config) = language_server_for_path(&position.path) else {
        return Ok(None);
    };
    request_hover_with_config(position, &config)
}

pub fn definitions(position: &DocumentPosition) -> Result<Vec<LspLocation>> {
    let Some(config) = language_server_for_path(&position.path) else {
        return Ok(Vec::new());
    };
    request_definitions_with_config(position, &config)
}

pub fn references(position: &DocumentPosition) -> Result<Vec<LspLocation>> {
    let Some(config) = language_server_for_path(&position.path) else {
        return Ok(Vec::new());
    };
    request_references_with_config(position, &config)
}

pub fn completions(position: &DocumentPosition) -> Result<Vec<LspCompletion>> {
    let Some(config) = language_server_for_path(&position.path) else {
        return Ok(Vec::new());
    };
    request_completions_with_config(position, &config)
}

pub fn diagnostics(position: &DocumentPosition) -> Result<Vec<LspDiagnostic>> {
    let Some(config) = language_server_for_path(&position.path) else {
        return Ok(Vec::new());
    };
    request_diagnostics_with_config(position, &config)
}

pub fn code_actions(
    position: &DocumentPosition,
    diagnostics: &[LspDiagnostic],
) -> Result<Vec<LspCodeAction>> {
    let Some(config) = language_server_for_path(&position.path) else {
        return Ok(Vec::new());
    };
    request_code_actions_with_config(position, diagnostics, &config)
}

pub fn execute_code_action_command(
    position: &DocumentPosition,
    action: &LspCodeAction,
) -> Result<Option<LspWorkspaceEdit>> {
    let Some(command) = &action.command else {
        return Ok(None);
    };
    let Some(config) = language_server_for_path(&position.path) else {
        return Ok(None);
    };
    request_execute_command_with_config(position, command, &config)
}

pub fn formatting(position: &DocumentPosition) -> Result<Option<LspWorkspaceEdit>> {
    let Some(config) = language_server_for_path(&position.path) else {
        return Ok(None);
    };
    request_formatting_with_config(position, &config)
}

pub fn rename(position: &DocumentPosition, new_name: &str) -> Result<Option<LspWorkspaceEdit>> {
    let Some(config) = language_server_for_path(&position.path) else {
        return Ok(None);
    };
    request_rename_with_config(position, new_name, &config)
}

fn request_hover_with_config(
    position: &DocumentPosition,
    config: &LanguageServerConfig,
) -> Result<Option<LspHover>> {
    let params = text_document_position_params(position);
    let Some(response) = run_lsp_request(position, config, "textDocument/hover", params)? else {
        return Ok(None);
    };
    let Some(contents) = hover_contents(response.result.as_ref()) else {
        return Ok(None);
    };
    Ok(Some(LspHover {
        contents,
        server: config.name.clone(),
    }))
}

fn request_definitions_with_config(
    position: &DocumentPosition,
    config: &LanguageServerConfig,
) -> Result<Vec<LspLocation>> {
    let params = text_document_position_params(position);
    let Some(response) = run_lsp_request(position, config, "textDocument/definition", params)?
    else {
        return Ok(Vec::new());
    };
    Ok(parse_locations(
        response.result.as_ref(),
        position,
        &config.name,
    ))
}

fn request_references_with_config(
    position: &DocumentPosition,
    config: &LanguageServerConfig,
) -> Result<Vec<LspLocation>> {
    let mut params = text_document_position_params(position);
    if let Some(object) = params.as_object_mut() {
        object.insert(
            "context".to_owned(),
            json!({
                "includeDeclaration": true
            }),
        );
    }
    let Some(response) = run_lsp_request(position, config, "textDocument/references", params)?
    else {
        return Ok(Vec::new());
    };
    Ok(parse_locations(
        response.result.as_ref(),
        position,
        &config.name,
    ))
}

fn request_completions_with_config(
    position: &DocumentPosition,
    config: &LanguageServerConfig,
) -> Result<Vec<LspCompletion>> {
    let mut params = text_document_position_params(position);
    if let Some(object) = params.as_object_mut() {
        object.insert(
            "context".to_owned(),
            json!({
                "triggerKind": 1
            }),
        );
    }
    let Some(response) = run_lsp_request(position, config, "textDocument/completion", params)?
    else {
        return Ok(Vec::new());
    };
    Ok(parse_completions(response.result.as_ref(), &config.name))
}

fn request_diagnostics_with_config(
    position: &DocumentPosition,
    config: &LanguageServerConfig,
) -> Result<Vec<LspDiagnostic>> {
    let Ok(mut process) = spawn_lsp(config, &position.root) else {
        return Ok(Vec::new());
    };

    let deadline = Instant::now() + request_timeout();
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "processId": null,
            "rootUri": path_to_file_uri(&position.root),
            "capabilities": {},
            "clientInfo": {
                "name": "tscode",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    });
    if send_message(&mut process.stdin, &initialize).is_err() {
        process.kill();
        return Ok(Vec::new());
    }
    if next_response(&process.messages, 1, deadline).is_none() {
        process.kill();
        return Ok(Vec::new());
    }

    let initialized = json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {}
    });
    let did_open = json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": path_to_file_uri(&position.path),
                "languageId": config.language_id,
                "version": 1,
                "text": position.text
            }
        }
    });

    if send_message(&mut process.stdin, &initialized).is_err()
        || send_message(&mut process.stdin, &did_open).is_err()
    {
        process.kill();
        return Ok(Vec::new());
    }

    let mut diagnostics = Vec::new();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match process.messages.recv_timeout(remaining) {
            Ok(Ok(value)) => {
                if let Some(items) = parse_publish_diagnostics(&value, position, &config.name) {
                    diagnostics = items;
                    if !diagnostics.is_empty() {
                        break;
                    }
                }
            }
            Ok(Err(_)) | Err(_) => break,
        }
    }

    let shutdown = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "shutdown",
        "params": null
    });
    let exit = json!({
        "jsonrpc": "2.0",
        "method": "exit",
        "params": null
    });
    let _ = send_message(&mut process.stdin, &shutdown);
    let _ = next_response(&process.messages, 3, Instant::now() + SHUTDOWN_TIMEOUT);
    let _ = send_message(&mut process.stdin, &exit);
    process.finish();

    Ok(diagnostics)
}

fn request_code_actions_with_config(
    position: &DocumentPosition,
    diagnostics: &[LspDiagnostic],
    config: &LanguageServerConfig,
) -> Result<Vec<LspCodeAction>> {
    let Some(response) = run_lsp_request(
        position,
        config,
        "textDocument/codeAction",
        code_action_params(position, diagnostics),
    )?
    else {
        return Ok(Vec::new());
    };
    Ok(parse_code_actions(response.result.as_ref(), &config.name))
}

fn request_execute_command_with_config(
    position: &DocumentPosition,
    command: &LspCommand,
    config: &LanguageServerConfig,
) -> Result<Option<LspWorkspaceEdit>> {
    let params = json!({
        "command": command.command.clone(),
        "arguments": command.arguments.clone()
    });
    let (response, mut edits) = run_lsp_request_collecting_workspace_edits(
        position,
        config,
        "workspace/executeCommand",
        params,
    )?;

    if let Some(response) = response
        && let Some(edit) = parse_workspace_edit(response.result.as_ref(), &config.name)
    {
        edits.extend(edit.edits);
    }

    if edits.is_empty() {
        return Ok(None);
    }

    Ok(Some(LspWorkspaceEdit {
        edits,
        server: config.name.clone(),
    }))
}

fn request_formatting_with_config(
    position: &DocumentPosition,
    config: &LanguageServerConfig,
) -> Result<Option<LspWorkspaceEdit>> {
    let Some(response) = run_lsp_request(
        position,
        config,
        "textDocument/formatting",
        formatting_params(position),
    )?
    else {
        return Ok(None);
    };
    Ok(parse_formatting_edits(
        response.result.as_ref(),
        position,
        &config.name,
    ))
}

fn request_rename_with_config(
    position: &DocumentPosition,
    new_name: &str,
    config: &LanguageServerConfig,
) -> Result<Option<LspWorkspaceEdit>> {
    let mut params = text_document_position_params(position);
    if let Some(object) = params.as_object_mut() {
        object.insert("newName".to_owned(), Value::String(new_name.to_owned()));
    }
    let Some(response) = run_lsp_request(position, config, "textDocument/rename", params)? else {
        return Ok(None);
    };
    Ok(parse_workspace_edit(response.result.as_ref(), &config.name))
}

#[derive(Debug, Clone)]
struct LspResponse {
    result: Option<Value>,
}

fn run_lsp_request(
    position: &DocumentPosition,
    config: &LanguageServerConfig,
    method: &str,
    params: Value,
) -> Result<Option<LspResponse>> {
    let Ok(mut process) = spawn_lsp(config, &position.root) else {
        return Ok(None);
    };

    let deadline = Instant::now() + request_timeout();
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "processId": null,
            "rootUri": path_to_file_uri(&position.root),
            "capabilities": {},
            "clientInfo": {
                "name": "tscode",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    });
    if send_message(&mut process.stdin, &initialize).is_err() {
        process.kill();
        return Ok(None);
    }
    if next_response(&process.messages, 1, deadline).is_none() {
        process.kill();
        return Ok(None);
    }

    let initialized = json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {}
    });
    let did_open = json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": path_to_file_uri(&position.path),
                "languageId": config.language_id,
                "version": 1,
                "text": position.text
            }
        }
    });
    let request = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": method,
        "params": params
    });

    if send_message(&mut process.stdin, &initialized).is_err()
        || send_message(&mut process.stdin, &did_open).is_err()
        || send_message(&mut process.stdin, &request).is_err()
    {
        process.kill();
        return Ok(None);
    }

    let response = next_response(&process.messages, 2, deadline).and_then(|value| {
        if value.get("error").is_some() {
            None
        } else {
            Some(LspResponse {
                result: value.get("result").cloned(),
            })
        }
    });

    let shutdown = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "shutdown",
        "params": null
    });
    let exit = json!({
        "jsonrpc": "2.0",
        "method": "exit",
        "params": null
    });
    let _ = send_message(&mut process.stdin, &shutdown);
    let _ = next_response(&process.messages, 3, Instant::now() + SHUTDOWN_TIMEOUT);
    let _ = send_message(&mut process.stdin, &exit);
    process.finish();

    Ok(response)
}

fn run_lsp_request_collecting_workspace_edits(
    position: &DocumentPosition,
    config: &LanguageServerConfig,
    method: &str,
    params: Value,
) -> Result<(Option<LspResponse>, Vec<LspTextEdit>)> {
    let Ok(mut process) = spawn_lsp(config, &position.root) else {
        return Ok((None, Vec::new()));
    };

    let deadline = Instant::now() + request_timeout();
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "processId": null,
            "rootUri": path_to_file_uri(&position.root),
            "capabilities": {
                "workspace": {
                    "applyEdit": true
                }
            },
            "clientInfo": {
                "name": "tscode",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    });
    if send_message(&mut process.stdin, &initialize).is_err() {
        process.kill();
        return Ok((None, Vec::new()));
    }
    if next_response(&process.messages, 1, deadline).is_none() {
        process.kill();
        return Ok((None, Vec::new()));
    }

    let initialized = json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {}
    });
    let did_open = json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": path_to_file_uri(&position.path),
                "languageId": config.language_id,
                "version": 1,
                "text": position.text
            }
        }
    });
    let request = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": method,
        "params": params
    });

    if send_message(&mut process.stdin, &initialized).is_err()
        || send_message(&mut process.stdin, &did_open).is_err()
        || send_message(&mut process.stdin, &request).is_err()
    {
        process.kill();
        return Ok((None, Vec::new()));
    }

    let (response_value, edits) =
        next_response_collecting_workspace_edits(&mut process, 2, deadline, &config.name);
    let response = response_value.and_then(|value| {
        if value.get("error").is_some() {
            None
        } else {
            Some(LspResponse {
                result: value.get("result").cloned(),
            })
        }
    });

    let shutdown = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "shutdown",
        "params": null
    });
    let exit = json!({
        "jsonrpc": "2.0",
        "method": "exit",
        "params": null
    });
    let _ = send_message(&mut process.stdin, &shutdown);
    let _ = next_response(&process.messages, 3, Instant::now() + SHUTDOWN_TIMEOUT);
    let _ = send_message(&mut process.stdin, &exit);
    process.finish();

    Ok((response, edits))
}

struct LspProcess {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Result<Value, String>>,
}

impl LspProcess {
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn finish(&mut self) {
        match self.child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
            Err(_) => {
                let _ = self.child.kill();
            }
        }
    }
}

fn spawn_lsp(config: &LanguageServerConfig, root: &Path) -> io::Result<LspProcess> {
    let mut child = Command::new(&config.command)
        .args(&config.args)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("language server stdin unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("language server stdout unavailable"))?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_message(&mut reader) {
                Ok(Some(value)) => {
                    if tx.send(Ok(value)).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let _ = tx.send(Err(error.to_string()));
                    break;
                }
            }
        }
    });

    Ok(LspProcess {
        child,
        stdin,
        messages: rx,
    })
}

fn send_message(stdin: &mut ChildStdin, value: &Value) -> io::Result<()> {
    let body = value.to_string();
    write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    stdin.flush()
}

fn read_message(reader: &mut BufReader<impl Read>) -> io::Result<Option<Value>> {
    let mut content_length = None;
    let mut saw_header = false;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Ok(None);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        saw_header = true;
        if let Some(length) = line.strip_prefix("Content-Length:") {
            content_length = length.trim().parse::<usize>().ok();
        }
    }

    if !saw_header {
        return Ok(None);
    }
    let Some(length) = content_length else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "LSP message missing Content-Length",
        ));
    };
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let value = serde_json::from_slice(&body).map_err(io::Error::other)?;
    Ok(Some(value))
}

fn next_response(
    messages: &Receiver<Result<Value, String>>,
    id: i64,
    deadline: Instant,
) -> Option<Value> {
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match messages.recv_timeout(remaining) {
            Ok(Ok(value)) => {
                if value.get("id").and_then(Value::as_i64) == Some(id) {
                    return Some(value);
                }
            }
            Ok(Err(_)) | Err(_) => return None,
        }
    }
    None
}

fn next_response_collecting_workspace_edits(
    process: &mut LspProcess,
    id: i64,
    deadline: Instant,
    server: &str,
) -> (Option<Value>, Vec<LspTextEdit>) {
    let mut edits = Vec::new();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match process.messages.recv_timeout(remaining) {
            Ok(Ok(value)) => {
                if value.get("method").and_then(Value::as_str) == Some("workspace/applyEdit") {
                    let request_id = value.get("id").cloned().unwrap_or(Value::Null);
                    let edit = parse_workspace_edit(
                        value.get("params").and_then(|params| params.get("edit")),
                        server,
                    );
                    let applied = edit.is_some();
                    if let Some(edit) = edit {
                        edits.extend(edit.edits);
                    }
                    let result = if applied {
                        json!({ "applied": true })
                    } else {
                        json!({
                            "applied": false,
                            "failureReason": "tscode did not receive a workspace edit"
                        })
                    };
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "result": result
                    });
                    if send_message(&mut process.stdin, &response).is_err() {
                        return (None, edits);
                    }
                    continue;
                }

                if let Some(request_id) = value
                    .get("id")
                    .cloned()
                    .filter(|_| value.get("method").and_then(Value::as_str).is_some())
                {
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "error": {
                            "code": -32601,
                            "message": "method not supported by tscode one-shot LSP client"
                        }
                    });
                    let _ = send_message(&mut process.stdin, &response);
                    continue;
                }

                if value.get("id").and_then(Value::as_i64) == Some(id) {
                    return (Some(value), edits);
                }
            }
            Ok(Err(_)) | Err(_) => return (None, edits),
        }
    }
    (None, edits)
}

fn text_document_position_params(position: &DocumentPosition) -> Value {
    let line_text = position.text.lines().nth(position.line).unwrap_or_default();
    json!({
        "textDocument": {
            "uri": path_to_file_uri(&position.path)
        },
        "position": {
            "line": position.line,
            "character": char_col_to_utf16(line_text, position.col)
        }
    })
}

fn code_action_params(position: &DocumentPosition, diagnostics: &[LspDiagnostic]) -> Value {
    let line_text = position.text.lines().nth(position.line).unwrap_or_default();
    let utf16_col = char_col_to_utf16(line_text, position.col);
    let diagnostics = diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic_to_lsp_value(diagnostic, position))
        .collect::<Vec<_>>();
    json!({
        "textDocument": {
            "uri": path_to_file_uri(&position.path)
        },
        "range": {
            "start": {
                "line": position.line,
                "character": utf16_col
            },
            "end": {
                "line": position.line,
                "character": utf16_col
            }
        },
        "context": {
            "diagnostics": diagnostics,
            "only": ["quickfix", "refactor", "source"]
        }
    })
}

fn formatting_params(position: &DocumentPosition) -> Value {
    json!({
        "textDocument": {
            "uri": path_to_file_uri(&position.path)
        },
        "options": {
            "tabSize": 4,
            "insertSpaces": true,
            "trimTrailingWhitespace": true,
            "insertFinalNewline": true,
            "trimFinalNewlines": true
        }
    })
}

fn diagnostic_to_lsp_value(
    diagnostic: &LspDiagnostic,
    position: &DocumentPosition,
) -> Option<Value> {
    if !same_path(&diagnostic.path, &position.path) {
        return None;
    }
    let line_text = line_text_for_path(&diagnostic.path, diagnostic.line, position)?;
    let line_len = line_text.chars().count();
    let start_col = diagnostic.col.min(line_len);
    let end_col = if start_col < line_len {
        start_col + 1
    } else {
        start_col
    };
    let mut value = json!({
        "range": {
            "start": {
                "line": diagnostic.line,
                "character": char_col_to_utf16(&line_text, start_col)
            },
            "end": {
                "line": diagnostic.line,
                "character": char_col_to_utf16(&line_text, end_col)
            }
        },
        "severity": diagnostic.severity.as_lsp_number(),
        "message": diagnostic.message
    });
    if let Some(object) = value.as_object_mut() {
        if let Some(source) = &diagnostic.source {
            object.insert("source".to_owned(), Value::String(source.clone()));
        }
        if let Some(code) = &diagnostic.code {
            object.insert("code".to_owned(), Value::String(code.clone()));
        }
    }
    Some(value)
}

fn language_server_for_path(path: &Path) -> Option<LanguageServerConfig> {
    if let Some(config) = env_language_server(path) {
        return Some(config);
    }

    let extension = path.extension().and_then(|extension| extension.to_str())?;
    let extension = extension.to_ascii_lowercase();
    match extension.as_str() {
        "rs" => Some(LanguageServerConfig::new("rust-analyzer", &[], "rust")),
        "py" | "pyw" => Some(LanguageServerConfig::new(
            "pyright-langserver",
            &["--stdio"],
            "python",
        )),
        "ts" => Some(LanguageServerConfig::new(
            "typescript-language-server",
            &["--stdio"],
            "typescript",
        )),
        "tsx" => Some(LanguageServerConfig::new(
            "typescript-language-server",
            &["--stdio"],
            "typescriptreact",
        )),
        "js" | "mjs" | "cjs" => Some(LanguageServerConfig::new(
            "typescript-language-server",
            &["--stdio"],
            "javascript",
        )),
        "jsx" => Some(LanguageServerConfig::new(
            "typescript-language-server",
            &["--stdio"],
            "javascriptreact",
        )),
        "go" => Some(LanguageServerConfig::new("gopls", &[], "go")),
        "c" => Some(LanguageServerConfig::new("clangd", &[], "c")),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "h" => {
            Some(LanguageServerConfig::new("clangd", &[], "cpp"))
        }
        _ => None,
    }
}

impl LanguageServerConfig {
    fn new(command: &str, args: &[&str], language_id: &str) -> Self {
        Self {
            name: command.to_owned(),
            command: command.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            language_id: language_id.to_owned(),
        }
    }
}

fn env_language_server(path: &Path) -> Option<LanguageServerConfig> {
    let raw = env::var("TSCODE_LSP_COMMAND").ok()?;
    let mut parts = split_command_words(&raw);
    let command = parts.next()?;
    let args = parts.collect::<Vec<_>>();
    let language_id = env::var("TSCODE_LSP_LANGUAGE_ID")
        .ok()
        .or_else(|| default_language_id(path).map(str::to_owned))
        .unwrap_or_else(|| "plaintext".to_owned());
    let name = env::var("TSCODE_LSP_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| command.clone());
    Some(LanguageServerConfig {
        name,
        command,
        args,
        language_id,
    })
}

fn split_command_words(command: &str) -> impl Iterator<Item = String> + '_ {
    command
        .split_whitespace()
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
}

fn default_language_id(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "rs" => Some("rust"),
        "py" | "pyw" => Some("python"),
        "ts" => Some("typescript"),
        "tsx" => Some("typescriptreact"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "jsx" => Some("javascriptreact"),
        "go" => Some("go"),
        "c" => Some("c"),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "h" => Some("cpp"),
        _ => None,
    }
}

fn hover_contents(result: Option<&Value>) -> Option<String> {
    let result = result?;
    if result.is_null() {
        return None;
    }
    let contents = result.get("contents").unwrap_or(result);
    let text = marked_text(contents);
    (!text.trim().is_empty()).then(|| text.trim().to_owned())
}

fn marked_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(marked_text)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        Value::Object(object) => object
            .get("value")
            .or_else(|| object.get("contents"))
            .map(marked_text)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn parse_locations(
    result: Option<&Value>,
    position: &DocumentPosition,
    server: &str,
) -> Vec<LspLocation> {
    let Some(result) = result else {
        return Vec::new();
    };
    let values = match result {
        Value::Array(items) => items.iter().collect::<Vec<_>>(),
        Value::Null => Vec::new(),
        value => vec![value],
    };

    values
        .into_iter()
        .filter_map(|value| {
            let uri = value
                .get("uri")
                .or_else(|| value.get("targetUri"))
                .and_then(Value::as_str)?;
            let range = value
                .get("range")
                .or_else(|| value.get("targetSelectionRange"))
                .or_else(|| value.get("targetRange"))?;
            let start = range.get("start")?;
            let line = start.get("line")?.as_u64()? as usize;
            let utf16_col = start.get("character")?.as_u64()? as usize;
            let path = file_uri_to_path(uri)?;
            let line_text = line_text_for_path(&path, line, position).unwrap_or_default();
            let col = utf16_col_to_char_col(&line_text, utf16_col);
            let preview = (!line_text.trim().is_empty()).then(|| line_text.trim().to_owned());
            Some(LspLocation {
                path,
                line,
                col,
                preview,
                server: server.to_owned(),
            })
        })
        .collect()
}

fn parse_completions(result: Option<&Value>, server: &str) -> Vec<LspCompletion> {
    let Some(result) = result else {
        return Vec::new();
    };
    let items = match result {
        Value::Array(items) => items.as_slice(),
        Value::Object(object) => object
            .get("items")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        _ => &[],
    };

    items
        .iter()
        .filter_map(|item| {
            let label = item.get("label")?.as_str()?.to_owned();
            let detail = item
                .get("detail")
                .and_then(Value::as_str)
                .or_else(|| {
                    item.get("documentation")
                        .and_then(|documentation| match documentation {
                            Value::String(text) => Some(text.as_str()),
                            Value::Object(object) => object.get("value").and_then(Value::as_str),
                            _ => None,
                        })
                })
                .map(str::to_owned);
            let insert_text = item
                .get("textEdit")
                .and_then(|edit| edit.get("newText"))
                .and_then(Value::as_str)
                .or_else(|| item.get("insertText").and_then(Value::as_str))
                .map(str::to_owned);
            Some(LspCompletion {
                label,
                detail,
                insert_text,
                server: server.to_owned(),
            })
        })
        .collect()
}

fn parse_publish_diagnostics(
    value: &Value,
    position: &DocumentPosition,
    server: &str,
) -> Option<Vec<LspDiagnostic>> {
    if value.get("method").and_then(Value::as_str) != Some("textDocument/publishDiagnostics") {
        return None;
    }

    let params = value.get("params")?;
    let uri = params.get("uri").and_then(Value::as_str)?;
    let path = file_uri_to_path(uri)?;
    let diagnostics = params.get("diagnostics").and_then(Value::as_array)?;
    let items = diagnostics
        .iter()
        .filter_map(|diagnostic| {
            let range = diagnostic.get("range")?;
            let start = range.get("start")?;
            let line = start.get("line")?.as_u64()? as usize;
            let utf16_col = start.get("character")?.as_u64()? as usize;
            let message = diagnostic.get("message")?.as_str()?.trim();
            if message.is_empty() {
                return None;
            }
            let line_text = line_text_for_path(&path, line, position).unwrap_or_default();
            Some(LspDiagnostic {
                path: path.clone(),
                line,
                col: utf16_col_to_char_col(&line_text, utf16_col),
                severity: LspDiagnosticSeverity::from_lsp_value(diagnostic.get("severity")),
                message: message.to_owned(),
                source: diagnostic
                    .get("source")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                code: diagnostic_code(diagnostic.get("code")),
                server: server.to_owned(),
            })
        })
        .collect::<Vec<_>>();
    Some(items)
}

impl LspDiagnosticSeverity {
    fn from_lsp_value(value: Option<&Value>) -> Self {
        match value.and_then(Value::as_u64) {
            Some(1) => Self::Error,
            Some(2) => Self::Warning,
            Some(3) => Self::Information,
            Some(4) => Self::Hint,
            _ => Self::Unknown,
        }
    }

    fn as_lsp_number(self) -> u64 {
        match self {
            Self::Error => 1,
            Self::Warning => 2,
            Self::Information => 3,
            Self::Hint => 4,
            Self::Unknown => 3,
        }
    }
}

fn diagnostic_code(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn parse_code_actions(result: Option<&Value>, server: &str) -> Vec<LspCodeAction> {
    let Some(result) = result else {
        return Vec::new();
    };
    let Some(actions) = result.as_array() else {
        return Vec::new();
    };

    actions
        .iter()
        .filter_map(|action| {
            let title = action.get("title")?.as_str()?.trim();
            if title.is_empty() {
                return None;
            }

            let edit = parse_workspace_edit(action.get("edit"), server);
            let command = parse_lsp_command(action, title);
            let command_title = command.as_ref().map(|command| command.title.clone());
            Some(LspCodeAction {
                title: title.to_owned(),
                kind: action
                    .get("kind")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                is_preferred: action
                    .get("isPreferred")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                edit,
                command_title,
                command,
                server: server.to_owned(),
            })
        })
        .collect()
}

fn parse_lsp_command(action: &Value, fallback_title: &str) -> Option<LspCommand> {
    let raw_command = action.get("command")?;
    if let Some(command) = raw_command.as_str() {
        let command = command.trim();
        if command.is_empty() {
            return None;
        }
        return Some(LspCommand {
            title: fallback_title.to_owned(),
            command: command.to_owned(),
            arguments: action
                .get("arguments")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        });
    }

    let command = raw_command.get("command")?.as_str()?.trim();
    if command.is_empty() {
        return None;
    }
    let title = raw_command
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or(fallback_title);
    Some(LspCommand {
        title: title.to_owned(),
        command: command.to_owned(),
        arguments: raw_command
            .get("arguments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    })
}

fn parse_workspace_edit(result: Option<&Value>, server: &str) -> Option<LspWorkspaceEdit> {
    let result = result?;
    if result.is_null() {
        return None;
    }

    let mut edits = Vec::new();
    if let Some(changes) = result.get("changes").and_then(Value::as_object) {
        for (uri, raw_edits) in changes {
            parse_text_edits_for_uri(uri, raw_edits, &mut edits);
        }
    }

    if let Some(document_changes) = result.get("documentChanges").and_then(Value::as_array) {
        for change in document_changes {
            let Some(uri) = change
                .get("textDocument")
                .and_then(|text_document| text_document.get("uri"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            if let Some(raw_edits) = change.get("edits") {
                parse_text_edits_for_uri(uri, raw_edits, &mut edits);
            }
        }
    }

    (!edits.is_empty()).then(|| LspWorkspaceEdit {
        edits,
        server: server.to_owned(),
    })
}

fn parse_formatting_edits(
    result: Option<&Value>,
    position: &DocumentPosition,
    server: &str,
) -> Option<LspWorkspaceEdit> {
    let mut edits = Vec::new();
    parse_text_edits_for_uri(&path_to_file_uri(&position.path), result?, &mut edits);
    (!edits.is_empty()).then(|| LspWorkspaceEdit {
        edits,
        server: server.to_owned(),
    })
}

fn parse_text_edits_for_uri(uri: &str, raw_edits: &Value, output: &mut Vec<LspTextEdit>) {
    let Some(path) = file_uri_to_path(uri) else {
        return;
    };
    let Some(edits) = raw_edits.as_array() else {
        return;
    };
    for edit in edits {
        let Some(range) = edit.get("range") else {
            continue;
        };
        let Some(start) = range.get("start") else {
            continue;
        };
        let Some(end) = range.get("end") else {
            continue;
        };
        let Some(new_text) = edit.get("newText").and_then(Value::as_str) else {
            continue;
        };
        let Some(start_line) = start.get("line").and_then(Value::as_u64) else {
            continue;
        };
        let Some(start_utf16_col) = start.get("character").and_then(Value::as_u64) else {
            continue;
        };
        let Some(end_line) = end.get("line").and_then(Value::as_u64) else {
            continue;
        };
        let Some(end_utf16_col) = end.get("character").and_then(Value::as_u64) else {
            continue;
        };
        output.push(LspTextEdit {
            path: path.clone(),
            start_line: start_line as usize,
            start_utf16_col: start_utf16_col as usize,
            end_line: end_line as usize,
            end_utf16_col: end_utf16_col as usize,
            new_text: new_text.to_owned(),
        });
    }
}

fn line_text_for_path(path: &Path, line: usize, position: &DocumentPosition) -> Option<String> {
    if same_path(path, &position.path) {
        return position.text.lines().nth(line).map(str::to_owned);
    }
    fs::read_to_string(path)
        .ok()
        .and_then(|text| text.lines().nth(line).map(str::to_owned))
}

fn same_path(left: &Path, right: &Path) -> bool {
    left == right
        || left
            .canonicalize()
            .ok()
            .zip(right.canonicalize().ok())
            .is_some_and(|(left, right)| left == right)
}

fn char_col_to_utf16(line: &str, col: usize) -> usize {
    line.chars().take(col).map(char::len_utf16).sum::<usize>()
}

fn utf16_col_to_char_col(line: &str, utf16_col: usize) -> usize {
    let mut total = 0;
    for (index, ch) in line.chars().enumerate() {
        let next = total + ch.len_utf16();
        if next > utf16_col {
            return index;
        }
        total = next;
    }
    line.chars().count()
}

fn path_to_file_uri(path: &Path) -> String {
    let path = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/");
    format!("file://{}", percent_encode_path(&path))
}

fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let value = uri.strip_prefix("file://")?;
    #[cfg(windows)]
    {
        let value = value.strip_prefix('/').unwrap_or(value);
        Some(PathBuf::from(percent_decode(value)?))
    }
    #[cfg(not(windows))]
    {
        Some(PathBuf::from(percent_decode(value)?))
    }
}

fn percent_encode_path(path: &str) -> String {
    let mut encoded = String::new();
    for byte in path.bytes() {
        let ch = byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '/' | '-' | '_' | '.' | '~' | ':') {
            encoded.push(ch);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_value(*bytes.get(index + 1)?)?;
            let low = hex_value(*bytes.get(index + 2)?)?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uri_round_trips_paths_with_spaces() {
        let path = env::temp_dir().join("tscode lsp uri");
        let uri = path_to_file_uri(&path);
        assert!(uri.contains("tscode%20lsp%20uri"));
        assert_eq!(file_uri_to_path(&uri).unwrap(), path);
    }

    #[test]
    fn parses_hover_markdown_contents() {
        let value = json!({
            "contents": [
                "plain",
                { "language": "rust", "value": "fn main()" },
                { "kind": "markdown", "value": "**docs**" }
            ]
        });
        assert_eq!(
            hover_contents(Some(&value)).unwrap(),
            "plain\n\nfn main()\n\n**docs**"
        );
    }

    #[test]
    fn parses_workspace_edit_changes_and_document_changes() {
        let root = env::temp_dir().join(format!("tscode-lsp-edit-{}", std::process::id()));
        let file_a = root.join("main.rs");
        let file_b = root.join("lib.rs");
        let edit = json!({
            "changes": {
                path_to_file_uri(&file_a): [
                    {
                        "range": {
                            "start": { "line": 0, "character": 4 },
                            "end": { "line": 0, "character": 9 }
                        },
                        "newText": "renamed"
                    }
                ]
            },
            "documentChanges": [
                {
                    "textDocument": { "uri": path_to_file_uri(&file_b), "version": null },
                    "edits": [
                        {
                            "range": {
                                "start": { "line": 2, "character": 1 },
                                "end": { "line": 2, "character": 6 }
                            },
                            "newText": "renamed"
                        }
                    ]
                },
                {
                    "kind": "rename",
                    "oldUri": path_to_file_uri(&file_a),
                    "newUri": path_to_file_uri(&file_b)
                }
            ]
        });

        let parsed = parse_workspace_edit(Some(&edit), "mock-rename").expect("workspace edit");
        assert_eq!(parsed.server, "mock-rename");
        assert_eq!(parsed.edits.len(), 2);
        assert_eq!(parsed.edits[0].path, file_a);
        assert_eq!(parsed.edits[0].start_line, 0);
        assert_eq!(parsed.edits[0].start_utf16_col, 4);
        assert_eq!(parsed.edits[1].path, file_b);
        assert_eq!(parsed.edits[1].end_line, 2);
        assert_eq!(parsed.edits[1].new_text, "renamed");
    }

    #[test]
    fn parses_code_actions_with_workspace_edits_and_commands() {
        let root = env::temp_dir().join(format!("tscode-lsp-code-action-{}", std::process::id()));
        let file = root.join("main.rs");
        let actions = json!([
            {
                "title": "Import missing item",
                "kind": "quickfix",
                "isPreferred": true,
                "edit": {
                    "changes": {
                        path_to_file_uri(&file): [
                            {
                                "range": {
                                    "start": { "line": 0, "character": 0 },
                                    "end": { "line": 0, "character": 0 }
                                },
                                "newText": "use crate::thing;\n"
                            }
                        ]
                    }
                }
            },
            {
                "title": "Run command action",
                "command": "mock.runAction",
                "arguments": [
                    { "uri": path_to_file_uri(&file) }
                ]
            },
            {
                "title": "Run object command action",
                "command": {
                    "title": "Apply object command",
                    "command": "mock.applyObjectAction",
                    "arguments": [
                        { "uri": path_to_file_uri(&file), "line": 0 }
                    ]
                }
            }
        ]);

        let parsed = parse_code_actions(Some(&actions), "mock-actions");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].title, "Import missing item");
        assert_eq!(parsed[0].kind.as_deref(), Some("quickfix"));
        assert!(parsed[0].is_preferred);
        let edit = parsed[0].edit.as_ref().expect("workspace edit");
        assert_eq!(edit.server, "mock-actions");
        assert_eq!(edit.edits.len(), 1);
        assert_eq!(edit.edits[0].path, file);
        assert_eq!(edit.edits[0].new_text, "use crate::thing;\n");
        assert_eq!(
            parsed[1].command_title.as_deref(),
            Some("Run command action")
        );
        let command = parsed[1].command.as_ref().expect("command");
        assert_eq!(command.title, "Run command action");
        assert_eq!(command.command, "mock.runAction");
        assert_eq!(
            command.arguments,
            vec![json!({ "uri": path_to_file_uri(&file) })]
        );
        assert!(parsed[1].edit.is_none());
        assert_eq!(
            parsed[2].command_title.as_deref(),
            Some("Apply object command")
        );
        let command = parsed[2].command.as_ref().expect("object command");
        assert_eq!(command.command, "mock.applyObjectAction");
        assert_eq!(
            command.arguments,
            vec![json!({ "uri": path_to_file_uri(&file), "line": 0 })]
        );
    }

    #[test]
    fn parses_publish_diagnostics_notification() {
        let root = env::temp_dir().join(format!("tscode-lsp-diagnostics-{}", std::process::id()));
        let file = root.join("main.rs");
        let text = "fn main() {\n    let icon = \"🦀\";\n}\n";
        let position = DocumentPosition {
            root,
            path: file.clone(),
            text: text.to_owned(),
            line: 1,
            col: 0,
        };
        let value = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": path_to_file_uri(&file),
                "diagnostics": [
                    {
                        "range": {
                            "start": {
                                "line": 1,
                                "character": "    let icon = \"🦀\"".encode_utf16().count()
                            },
                            "end": { "line": 1, "character": 99 }
                        },
                        "severity": 1,
                        "source": "mock",
                        "code": 123,
                        "message": "expected semicolon"
                    },
                    {
                        "range": {
                            "start": { "line": 0, "character": 0 },
                            "end": { "line": 0, "character": 2 }
                        },
                        "severity": 4,
                        "message": "hint text"
                    }
                ]
            }
        });

        let diagnostics =
            parse_publish_diagnostics(&value, &position, "mock-lsp").expect("diagnostics");
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].path, file);
        assert_eq!(diagnostics[0].line, 1);
        assert_eq!(diagnostics[0].col, "    let icon = \"🦀\"".chars().count());
        assert_eq!(diagnostics[0].severity, LspDiagnosticSeverity::Error);
        assert_eq!(diagnostics[0].source.as_deref(), Some("mock"));
        assert_eq!(diagnostics[0].code.as_deref(), Some("123"));
        assert_eq!(diagnostics[1].severity, LspDiagnosticSeverity::Hint);
    }

    #[cfg(unix)]
    #[test]
    fn reads_publish_diagnostics_from_mock_stdio_language_server() {
        use std::os::unix::fs::PermissionsExt;

        let root = env::temp_dir().join(format!("tscode-lsp-diagnostics-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("main.rs");
        fs::write(&file, "fn main() {\n    missing();\n}\n").unwrap();
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": path_to_file_uri(&file),
                "diagnostics": [
                    {
                        "range": {
                            "start": { "line": 1, "character": 4 },
                            "end": { "line": 1, "character": 11 }
                        },
                        "severity": 1,
                        "source": "mock-checker",
                        "code": "E0425",
                        "message": "cannot find function `missing`"
                    }
                ]
            }
        })
        .to_string();
        let server = root.join("mock-diagnostics-lsp.sh");
        fs::write(
            &server,
            format!(
                r#"#!/bin/sh
read_msg() {{
  len=""
  while IFS= read -r line; do
    line=$(printf '%s' "$line" | tr -d '\r')
    [ -z "$line" ] && break
    case "$line" in
      Content-Length:*) len=${{line#Content-Length: }} ;;
    esac
  done
  [ -z "$len" ] && exit 0
  dd bs=1 count="$len" 2>/dev/null
}}
send_msg() {{
  body="$1"
  printf 'Content-Length: %s\r\n\r\n%s' "${{#body}}" "$body"
}}
read_msg >/dev/null
send_msg '{{"jsonrpc":"2.0","id":1,"result":{{"capabilities":{{"textDocumentSync":1}}}}}}'
read_msg >/dev/null
read_msg >/dev/null
send_msg '{}'
read_msg >/dev/null
send_msg '{{"jsonrpc":"2.0","id":3,"result":null}}'
read_msg >/dev/null
"#,
                notification
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&server).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&server, permissions).unwrap();

        let config = LanguageServerConfig {
            name: "mock-diagnostics".to_owned(),
            command: server.to_string_lossy().into_owned(),
            args: Vec::new(),
            language_id: "rust".to_owned(),
        };
        let position = DocumentPosition {
            root: root.clone(),
            path: file,
            text: "fn main() {\n    missing();\n}\n".to_owned(),
            line: 1,
            col: 4,
        };

        let diagnostics = request_diagnostics_with_config(&position, &config).unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].server, "mock-diagnostics");
        assert_eq!(diagnostics[0].severity, LspDiagnosticSeverity::Error);
        assert_eq!(diagnostics[0].message, "cannot find function `missing`");
        assert_eq!(diagnostics[0].source.as_deref(), Some("mock-checker"));
        assert_eq!(diagnostics[0].code.as_deref(), Some("E0425"));

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn reads_code_actions_from_mock_stdio_language_server() {
        use std::os::unix::fs::PermissionsExt;

        let root = env::temp_dir().join(format!("tscode-lsp-code-action-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("main.rs");
        fs::write(&file, "fn main() {\n    old_name();\n}\n").unwrap();
        let response = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": [
                {
                    "title": "Replace old_name with new_name",
                    "kind": "quickfix",
                    "isPreferred": true,
                    "edit": {
                        "changes": {
                            path_to_file_uri(&file): [
                                {
                                    "range": {
                                        "start": { "line": 1, "character": 4 },
                                        "end": { "line": 1, "character": 12 }
                                    },
                                    "newText": "new_name"
                                }
                            ]
                        }
                    }
                }
            ]
        })
        .to_string();
        let server = root.join("mock-code-action-lsp.sh");
        fs::write(
            &server,
            format!(
                r#"#!/bin/sh
read_msg() {{
  len=""
  while IFS= read -r line; do
    line=$(printf '%s' "$line" | tr -d '\r')
    [ -z "$line" ] && break
    case "$line" in
      Content-Length:*) len=${{line#Content-Length: }} ;;
    esac
  done
  [ -z "$len" ] && exit 0
  dd bs=1 count="$len" 2>/dev/null
}}
send_msg() {{
  body="$1"
  printf 'Content-Length: %s\r\n\r\n%s' "${{#body}}" "$body"
}}
read_msg >/dev/null
send_msg '{{"jsonrpc":"2.0","id":1,"result":{{"capabilities":{{"codeActionProvider":true}}}}}}'
read_msg >/dev/null
read_msg >/dev/null
body=$(read_msg)
case "$body" in
  *textDocument/codeAction*) send_msg '{}' ;;
  *) send_msg '{{"jsonrpc":"2.0","id":2,"result":[]}}' ;;
esac
read_msg >/dev/null
send_msg '{{"jsonrpc":"2.0","id":3,"result":null}}'
read_msg >/dev/null
"#,
                response
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&server).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&server, permissions).unwrap();

        let config = LanguageServerConfig {
            name: "mock-code-action".to_owned(),
            command: server.to_string_lossy().into_owned(),
            args: Vec::new(),
            language_id: "rust".to_owned(),
        };
        let position = DocumentPosition {
            root: root.clone(),
            path: file.clone(),
            text: fs::read_to_string(&file).unwrap(),
            line: 1,
            col: 4,
        };

        let actions = request_code_actions_with_config(&position, &[], &config).unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].title, "Replace old_name with new_name");
        assert_eq!(actions[0].server, "mock-code-action");
        let edit = actions[0].edit.as_ref().expect("workspace edit");
        assert_eq!(edit.edits.len(), 1);
        assert_eq!(edit.edits[0].path, file.canonicalize().unwrap());
        assert_eq!(edit.edits[0].new_text, "new_name");

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn reads_formatting_edits_from_mock_stdio_language_server() {
        use std::os::unix::fs::PermissionsExt;

        let root = env::temp_dir().join(format!("tscode-lsp-format-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("main.rs");
        fs::write(&file, "fn main(){println!(\"hi\");}\n").unwrap();
        let response = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": [
                {
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 1, "character": 0 }
                    },
                    "newText": "fn main() {\n    println!(\"hi\");\n}\n"
                }
            ]
        })
        .to_string();
        let server = root.join("mock-format-lsp.sh");
        fs::write(
            &server,
            format!(
                r#"#!/bin/sh
read_msg() {{
  len=""
  while IFS= read -r line; do
    line=$(printf '%s' "$line" | tr -d '\r')
    [ -z "$line" ] && break
    case "$line" in
      Content-Length:*) len=${{line#Content-Length: }} ;;
    esac
  done
  [ -z "$len" ] && exit 0
  dd bs=1 count="$len" 2>/dev/null
}}
send_msg() {{
  body="$1"
  printf 'Content-Length: %s\r\n\r\n%s' "${{#body}}" "$body"
}}
read_msg >/dev/null
send_msg '{{"jsonrpc":"2.0","id":1,"result":{{"capabilities":{{"documentFormattingProvider":true}}}}}}'
read_msg >/dev/null
read_msg >/dev/null
body=$(read_msg)
case "$body" in
  *textDocument/formatting*) send_msg '{}' ;;
  *) send_msg '{{"jsonrpc":"2.0","id":2,"result":[]}}' ;;
esac
read_msg >/dev/null
send_msg '{{"jsonrpc":"2.0","id":3,"result":null}}'
read_msg >/dev/null
"#,
                response
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&server).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&server, permissions).unwrap();

        let config = LanguageServerConfig {
            name: "mock-format".to_owned(),
            command: server.to_string_lossy().into_owned(),
            args: Vec::new(),
            language_id: "rust".to_owned(),
        };
        let position = DocumentPosition {
            root: root.clone(),
            path: file.clone(),
            text: fs::read_to_string(&file).unwrap(),
            line: 0,
            col: 0,
        };

        let edit = request_formatting_with_config(&position, &config)
            .unwrap()
            .expect("formatting edit");
        assert_eq!(edit.server, "mock-format");
        assert_eq!(edit.edits.len(), 1);
        assert_eq!(edit.edits[0].path, file.canonicalize().unwrap());
        assert_eq!(
            edit.edits[0].new_text,
            "fn main() {\n    println!(\"hi\");\n}\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn executes_code_action_command_and_collects_workspace_apply_edit() {
        use std::os::unix::fs::PermissionsExt;

        let root = env::temp_dir().join(format!(
            "tscode-lsp-code-action-command-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("main.rs");
        fs::write(&file, "fn main() {\n    old_name();\n}\n").unwrap();
        let apply_edit = json!({
            "jsonrpc": "2.0",
            "id": 50,
            "method": "workspace/applyEdit",
            "params": {
                "label": "Apply command quick fix",
                "edit": {
                    "changes": {
                        path_to_file_uri(&file): [
                            {
                                "range": {
                                    "start": { "line": 1, "character": 4 },
                                    "end": { "line": 1, "character": 12 }
                                },
                                "newText": "new_name"
                            }
                        ]
                    }
                }
            }
        })
        .to_string();
        let execute_response = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": null
        })
        .to_string();
        let error_response = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "error": {
                "code": -32000,
                "message": "apply edit was not acknowledged"
            }
        })
        .to_string();
        let server = root.join("mock-code-action-command-lsp.sh");
        fs::write(
            &server,
            format!(
                r#"#!/bin/sh
read_msg() {{
  len=""
  while IFS= read -r line; do
    line=$(printf '%s' "$line" | tr -d '\r')
    [ -z "$line" ] && break
    case "$line" in
      Content-Length:*) len=${{line#Content-Length: }} ;;
    esac
  done
  [ -z "$len" ] && exit 0
  dd bs=1 count="$len" 2>/dev/null
}}
send_msg() {{
  body="$1"
  printf 'Content-Length: %s\r\n\r\n%s' "${{#body}}" "$body"
}}
read_msg >/dev/null
send_msg '{{"jsonrpc":"2.0","id":1,"result":{{"capabilities":{{"executeCommandProvider":{{"commands":["mock.applyFix"]}}}}}}}}'
read_msg >/dev/null
read_msg >/dev/null
body=$(read_msg)
case "$body" in
  *workspace/executeCommand*)
    send_msg '{}'
    apply_response=$(read_msg)
    case "$apply_response" in
      *'"applied":true'*) send_msg '{}' ;;
      *) send_msg '{}' ;;
    esac
    ;;
  *) send_msg '{}' ;;
esac
read_msg >/dev/null
send_msg '{{"jsonrpc":"2.0","id":3,"result":null}}'
read_msg >/dev/null
"#,
                apply_edit, execute_response, error_response, error_response
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&server).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&server, permissions).unwrap();

        let config = LanguageServerConfig {
            name: "mock-command-action".to_owned(),
            command: server.to_string_lossy().into_owned(),
            args: Vec::new(),
            language_id: "rust".to_owned(),
        };
        let position = DocumentPosition {
            root: root.clone(),
            path: file.clone(),
            text: fs::read_to_string(&file).unwrap(),
            line: 1,
            col: 4,
        };
        let command = LspCommand {
            title: "Apply command quick fix".to_owned(),
            command: "mock.applyFix".to_owned(),
            arguments: vec![json!({ "uri": path_to_file_uri(&file) })],
        };

        let edit = request_execute_command_with_config(&position, &command, &config)
            .unwrap()
            .expect("workspace edit");
        assert_eq!(edit.server, "mock-command-action");
        assert_eq!(edit.edits.len(), 1);
        assert_eq!(edit.edits[0].path, file.canonicalize().unwrap());
        assert_eq!(edit.edits[0].new_text, "new_name");

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn reads_rename_workspace_edit_from_mock_stdio_language_server() {
        use std::os::unix::fs::PermissionsExt;

        let root = env::temp_dir().join(format!("tscode-lsp-rename-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("main.rs");
        fs::write(&file, "fn old_name() {}\n").unwrap();
        let response = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "changes": {
                    path_to_file_uri(&file): [
                        {
                            "range": {
                                "start": { "line": 0, "character": 3 },
                                "end": { "line": 0, "character": 11 }
                            },
                            "newText": "new_name"
                        }
                    ]
                }
            }
        })
        .to_string();
        let server = root.join("mock-rename-lsp.sh");
        fs::write(
            &server,
            format!(
                r#"#!/bin/sh
read_msg() {{
  len=""
  while IFS= read -r line; do
    line=$(printf '%s' "$line" | tr -d '\r')
    [ -z "$line" ] && break
    case "$line" in
      Content-Length:*) len=${{line#Content-Length: }} ;;
    esac
  done
  [ -z "$len" ] && exit 0
  dd bs=1 count="$len" 2>/dev/null
}}
send_msg() {{
  body="$1"
  printf 'Content-Length: %s\r\n\r\n%s' "${{#body}}" "$body"
}}
read_msg >/dev/null
send_msg '{{"jsonrpc":"2.0","id":1,"result":{{"capabilities":{{"renameProvider":true}}}}}}'
read_msg >/dev/null
read_msg >/dev/null
body=$(read_msg)
case "$body" in
  *textDocument/rename*) send_msg '{}' ;;
  *) send_msg '{{"jsonrpc":"2.0","id":2,"result":null}}' ;;
esac
read_msg >/dev/null
send_msg '{{"jsonrpc":"2.0","id":3,"result":null}}'
read_msg >/dev/null
"#,
                response
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&server).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&server, permissions).unwrap();

        let config = LanguageServerConfig {
            name: "mock-rename-lsp".to_owned(),
            command: server.to_string_lossy().into_owned(),
            args: Vec::new(),
            language_id: "rust".to_owned(),
        };
        let position = DocumentPosition {
            root: root.clone(),
            path: file.clone(),
            text: fs::read_to_string(&file).unwrap(),
            line: 0,
            col: 4,
        };

        let edit = request_rename_with_config(&position, "new_name", &config)
            .unwrap()
            .expect("rename edit");
        assert_eq!(edit.server, "mock-rename-lsp");
        assert_eq!(edit.edits.len(), 1);
        assert_eq!(edit.edits[0].path, file.canonicalize().unwrap());
        assert_eq!(edit.edits[0].start_utf16_col, 3);
        assert_eq!(edit.edits[0].new_text, "new_name");

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn reads_references_from_mock_stdio_language_server() {
        use std::os::unix::fs::PermissionsExt;

        let root = env::temp_dir().join(format!("tscode-lsp-refs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("main.rs");
        fs::write(&file, "hello();\nfn hello() {}\n").unwrap();
        let uri = path_to_file_uri(&file);
        let response = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": [
                {
                    "uri": uri,
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 5 }
                    }
                },
                {
                    "uri": uri,
                    "range": {
                        "start": { "line": 1, "character": 3 },
                        "end": { "line": 1, "character": 8 }
                    }
                }
            ]
        })
        .to_string();
        let server = root.join("mock-refs-lsp.sh");
        fs::write(
            &server,
            format!(
                r#"#!/bin/sh
read_msg() {{
  len=""
  while IFS= read -r line; do
    line=$(printf '%s' "$line" | tr -d '\r')
    [ -z "$line" ] && break
    case "$line" in
      Content-Length:*) len=${{line#Content-Length: }} ;;
    esac
  done
  [ -z "$len" ] && exit 0
  dd bs=1 count="$len" 2>/dev/null
}}
send_msg() {{
  body="$1"
  printf 'Content-Length: %s\r\n\r\n%s' "${{#body}}" "$body"
}}
read_msg >/dev/null
send_msg '{{"jsonrpc":"2.0","id":1,"result":{{"capabilities":{{"referencesProvider":true}}}}}}'
read_msg >/dev/null
read_msg >/dev/null
body=$(read_msg)
case "$body" in
  *textDocument/references*) send_msg '{}' ;;
  *) send_msg '{{"jsonrpc":"2.0","id":2,"result":null}}' ;;
esac
read_msg >/dev/null
send_msg '{{"jsonrpc":"2.0","id":3,"result":null}}'
read_msg >/dev/null
"#,
                response
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&server).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&server, permissions).unwrap();

        let config = LanguageServerConfig {
            name: "mock-refs-lsp".to_owned(),
            command: server.to_string_lossy().into_owned(),
            args: Vec::new(),
            language_id: "rust".to_owned(),
        };
        let position = DocumentPosition {
            root: root.clone(),
            path: file.clone(),
            text: fs::read_to_string(&file).unwrap(),
            line: 0,
            col: 1,
        };

        let references = request_references_with_config(&position, &config).unwrap();
        assert_eq!(references.len(), 2);
        assert_eq!(references[0].line, 0);
        assert_eq!(references[0].col, 0);
        assert_eq!(references[1].line, 1);
        assert_eq!(references[1].col, 3);
        assert_eq!(references[1].preview.as_deref(), Some("fn hello() {}"));

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn talks_to_mock_stdio_language_server() {
        use std::os::unix::fs::PermissionsExt;

        let root = env::temp_dir().join(format!("tscode-lsp-mock-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("main.rs");
        fs::write(&file, "fn main() {\n    hello();\n}\n").unwrap();
        let server = root.join("mock-lsp.sh");
        fs::write(
            &server,
            r#"#!/bin/sh
read_msg() {
  len=""
  while IFS= read -r line; do
    line=$(printf '%s' "$line" | tr -d '\r')
    [ -z "$line" ] && break
    case "$line" in
      Content-Length:*) len=${line#Content-Length: } ;;
    esac
  done
  [ -z "$len" ] && exit 0
  dd bs=1 count="$len" 2>/dev/null
}
send_msg() {
  body="$1"
  printf 'Content-Length: %s\r\n\r\n%s' "${#body}" "$body"
}
read_msg >/dev/null
send_msg '{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"hoverProvider":true}}}'
read_msg >/dev/null
read_msg >/dev/null
read_msg >/dev/null
send_msg '{"jsonrpc":"2.0","id":2,"result":{"contents":{"kind":"markdown","value":"**hello** from mock LSP"}}}'
read_msg >/dev/null
send_msg '{"jsonrpc":"2.0","id":3,"result":null}'
read_msg >/dev/null
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&server).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&server, permissions).unwrap();

        let config = LanguageServerConfig {
            name: "mock-lsp".to_owned(),
            command: server.to_string_lossy().into_owned(),
            args: Vec::new(),
            language_id: "rust".to_owned(),
        };
        let position = DocumentPosition {
            root: root.clone(),
            path: file.clone(),
            text: fs::read_to_string(&file).unwrap(),
            line: 1,
            col: 5,
        };

        let hover = request_hover_with_config(&position, &config)
            .unwrap()
            .expect("mock hover");
        assert_eq!(hover.server, "mock-lsp");
        assert_eq!(hover.contents, "**hello** from mock LSP");

        let _ = fs::remove_dir_all(root);
    }
}
