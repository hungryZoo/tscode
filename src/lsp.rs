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
