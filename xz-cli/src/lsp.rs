//! Phase 8 language server (first slice): a synchronous LSP server over stdio
//! that re-runs the front-end pipeline on open/changed documents and publishes
//! the same structured diagnostics as `xz check-json` (docs/07-compiler.md).
//!
//! Supported methods: `initialize`, `initialized`, `shutdown`, `exit`,
//! `textDocument/didOpen`, `textDocument/didChange`, `textDocument/didClose`.
//! Document sync is "full" (`TextDocumentSyncKind.Full` = 1): every
//! `didChange` carries the whole document, so the server keeps no incremental
//! edit state. Positions are emitted as UTF-16 code units (the LSP default).
use crate::diagnostic::{Diagnostic, Severity};
use crate::driver;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};

pub struct Server {
    docs: HashMap<String, String>,
    shutdown: bool,
    exit: bool,
}

impl Default for Server {
    fn default() -> Server {
        Server::new()
    }
}

impl Server {
    pub fn new() -> Server {
        Server { docs: HashMap::new(), shutdown: false, exit: false }
    }

    pub fn should_exit(&self) -> bool {
        self.exit
    }

    pub fn shutdown_received(&self) -> bool {
        self.shutdown
    }

    /// Handle one decoded JSON-RPC message; return the JSON payload to send
    /// back (a response or a `publishDiagnostics` notification), if any.
    pub fn handle(&mut self, raw: &str) -> Option<String> {
        let msg: Value = serde_json::from_str(raw).ok()?;
        let method = msg.get("method").and_then(Value::as_str)?.to_string();
        let id = msg.get("id").cloned();
        match method.as_str() {
            "initialize" => {
                let result = json!({
                    "capabilities": {
                        "positionEncoding": "utf-16",
                        "textDocumentSync": 1
                    },
                    "serverInfo": { "name": "xz", "version": env!("CARGO_PKG_VERSION") }
                });
                Some(response(id, result))
            }
            "initialized" => None,
            "shutdown" => {
                self.shutdown = true;
                Some(response(id, Value::Null))
            }
            "exit" => {
                self.exit = true;
                None
            }
            "textDocument/didOpen" => {
                let params = msg.get("params")?;
                let uri = params.pointer("/textDocument/uri")?.as_str()?.to_string();
                let text = params.pointer("/textDocument/text")?.as_str()?.to_string();
                self.docs.insert(uri.clone(), text);
                Some(self.publish(&uri))
            }
            "textDocument/didChange" => {
                let params = msg.get("params")?;
                let uri = params.pointer("/textDocument/uri")?.as_str()?.to_string();
                let text = params.pointer("/contentChanges/0/text")?.as_str()?.to_string();
                self.docs.insert(uri.clone(), text);
                Some(self.publish(&uri))
            }
            "textDocument/didClose" => {
                let params = msg.get("params")?;
                let uri = params.pointer("/textDocument/uri")?.as_str()?.to_string();
                self.docs.remove(&uri);
                Some(clear(&uri))
            }
            _ => {
                id.as_ref()?;
                Some(error_response(id, -32601, "method not found"))
            }
        }
    }

    fn publish(&self, uri: &str) -> String {
        let text = match self.docs.get(uri) {
            Some(t) => t,
            None => return clear(uri),
        };
        let items: Vec<Value> = driver::check(text, uri, false)
            .iter()
            .map(|d| diagnostic_json(d, text))
            .collect();
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": uri, "diagnostics": items }
        })
        .to_string()
    }
}

/// Read/decode JSON-RPC messages from `stdin` and write responses to `stdout`
/// until `exit` (or EOF). Returns the LSP-prescribed exit code: 0 if a
/// `shutdown` request preceded `exit`, 1 otherwise.
pub fn run_stdio() -> i32 {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    let mut server = Server::new();
    while let Ok(Some(body)) = read_message(&mut reader) {
        if let Some(resp) = server.handle(&body)
            && write_message(&mut writer, &resp).is_err()
        {
            break;
        }
        if server.should_exit() {
            break;
        }
    }
    if server.shutdown_received() { 0 } else { 1 }
}

fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut content_length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = value.trim().parse::<usize>().ok();
        }
    }
    let len = match content_length {
        Some(l) => l,
        None => return Ok(None),
    };
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

fn write_message<W: Write>(writer: &mut W, body: &str) -> io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    writer.flush()
}

fn response(id: Option<Value>, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn error_response(id: Option<Value>, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

fn clear(uri: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": { "uri": uri, "diagnostics": [] }
    })
    .to_string()
}

fn diagnostic_json(d: &Diagnostic, text: &str) -> Value {
    let start = to_lsp_position(text, d.span.start.0, d.span.start.1);
    let end = to_lsp_position(text, d.span.end.0, d.span.end.1);
    let severity = match d.severity {
        Severity::Error => 1,
        Severity::Warning => 2,
    };
    json!({
        "range": {
            "start": { "line": start.0, "character": start.1 },
            "end": { "line": end.0, "character": end.1 }
        },
        "severity": severity,
        "code": &d.code,
        "source": "xz",
        "message": &d.message
    })
}

/// Convert a 1-based Xz span position (line, Unicode scalar column) to a
/// 0-based LSP position measured in UTF-16 code units.
fn to_lsp_position(text: &str, line: usize, col: usize) -> (usize, usize) {
    let line0 = line.saturating_sub(1);
    let char_col = col.saturating_sub(1);
    let line_text = text.split('\n').nth(line0).unwrap_or("");
    let mut utf16 = 0usize;
    for (i, ch) in line_text.chars().enumerate() {
        if i >= char_col {
            break;
        }
        utf16 += ch.len_utf16();
    }
    (line0, utf16)
}
