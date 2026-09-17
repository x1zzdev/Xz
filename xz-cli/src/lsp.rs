//! Phase 8 language server (first slice): a synchronous LSP server over stdio
//! that re-runs the front-end pipeline on open/changed documents and publishes
//! the same structured diagnostics as `xz check-json` (docs/07-compiler.md).
//!
//! Supported methods: `initialize`, `initialized`, `shutdown`, `exit`,
//! `textDocument/didOpen`, `textDocument/didChange`, `textDocument/didClose`,
//! `textDocument/hover`, `textDocument/completion`, `textDocument/definition`,
//! `textDocument/formatting`.
//! Document sync is "full" (`TextDocumentSyncKind.Full`
//! = 1): every `didChange` carries the whole document, so the server keeps no
//! incremental edit state. Positions are emitted as UTF-16 code units (the LSP
//! default).
use crate::ast;
use crate::diagnostic::{Diagnostic, Severity};
use crate::driver;
use crate::format;
use crate::lexer::lex;
use crate::parser::parse;
use crate::token::{DocTag, Span, TokKind};
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
                        "textDocumentSync": 1,
                        "hoverProvider": true,
                        "completionProvider": { "resolveProvider": false },
                        "definitionProvider": true,
                        "documentFormattingProvider": true
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
            "textDocument/hover" => {
                let params = msg.get("params")?;
                let uri = params.pointer("/textDocument/uri")?.as_str()?.to_string();
                let line = params.pointer("/position/line")?.as_u64()? as usize;
                let character = params.pointer("/position/character")?.as_u64()? as usize;
                Some(response(id, self.hover(&uri, line, character)))
            }
            "textDocument/completion" => {
                let params = msg.get("params")?;
                let uri = params.pointer("/textDocument/uri")?.as_str()?.to_string();
                Some(response(id, self.completion(&uri)))
            }
            "textDocument/definition" => {
                let params = msg.get("params")?;
                let uri = params.pointer("/textDocument/uri")?.as_str()?.to_string();
                let line = params.pointer("/position/line")?.as_u64()? as usize;
                let character = params.pointer("/position/character")?.as_u64()? as usize;
                Some(response(id, self.definition(&uri, line, character)))
            }
            "textDocument/formatting" => {
                let params = msg.get("params")?;
                let uri = params.pointer("/textDocument/uri")?.as_str()?.to_string();
                Some(response(id, self.formatting(&uri)))
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

    /// Hover at an LSP position: when the cursor is on an identifier naming a
    /// top-level symbol, return its rendered signature (and doc claims) as
    /// markdown, else `null`. Requires a clean parse of the document.
    fn hover(&self, uri: &str, line: usize, character: usize) -> Value {
        let text = match self.docs.get(uri) {
            Some(t) => t,
            None => return Value::Null,
        };
        let tokens = match lex(text.to_string(), uri.to_string()) {
            Ok(t) => t,
            Err(_) => return Value::Null,
        };
        let pos = from_lsp_position(text, line, character);
        let (name, span) = match ident_at(&tokens, pos) {
            Some(found) => found,
            None => return Value::Null,
        };
        let program = match parse(tokens) {
            Ok(p) => p,
            Err(_) => return Value::Null,
        };
        let markdown = match find_symbol(&program, &name) {
            Some(m) => m,
            None => return Value::Null,
        };
        let start = to_lsp_position(text, span.start.0, span.start.1);
        let end = to_lsp_position(text, span.end.0, span.end.1);
        json!({
            "contents": { "kind": "markdown", "value": markdown },
            "range": {
                "start": { "line": start.0, "character": start.1 },
                "end": { "line": end.0, "character": end.1 }
            }
        })
    }

    /// Completion for a document: the top-level symbols it declares plus the
    /// language vocabulary (keywords, built-in types, stdlib globals). The
    /// static vocabulary is offered even when the document does not parse, so
    /// completion keeps working while an edit is in flight. The client owns
    /// prefix filtering; the list is complete (`isIncomplete: false`).
    fn completion(&self, uri: &str) -> Value {
        let text = match self.docs.get(uri) {
            Some(t) => t,
            None => return json!({ "isIncomplete": false, "items": [] }),
        };
        let program = lex(text.to_string(), uri.to_string())
            .ok()
            .and_then(|tokens| parse(tokens).ok());
        json!({ "isIncomplete": false, "items": completion_items(program.as_ref()) })
    }

    /// Go-to-definition at an LSP position: when the cursor is on an identifier
    /// naming a top-level symbol, return the `Location` of its declaration,
    /// else `null`. Same resolution contract as hover: by name within the
    /// current document, requiring a clean parse.
    fn definition(&self, uri: &str, line: usize, character: usize) -> Value {
        let text = match self.docs.get(uri) {
            Some(t) => t,
            None => return Value::Null,
        };
        let tokens = match lex(text.to_string(), uri.to_string()) {
            Ok(t) => t,
            Err(_) => return Value::Null,
        };
        let pos = from_lsp_position(text, line, character);
        let (name, _) = match ident_at(&tokens, pos) {
            Some(found) => found,
            None => return Value::Null,
        };
        let program = match parse(tokens) {
            Ok(p) => p,
            Err(_) => return Value::Null,
        };
        let span = match find_symbol_span(&program, &name) {
            Some(s) => s,
            None => return Value::Null,
        };
        let start = to_lsp_position(text, span.start.0, span.start.1);
        let end = to_lsp_position(text, span.end.0, span.end.1);
        json!({
            "uri": uri,
            "range": {
                "start": { "line": start.0, "character": start.1 },
                "end": { "line": end.0, "character": end.1 }
            }
        })
    }

    /// Whole-document formatting over the `xz fmt` engine: returns a single
    /// `TextEdit` that replaces the document with its canonical layout, or
    /// `null` when the document is not open or does not parse (the formatter
    /// never emits output for a file it cannot parse).
    fn formatting(&self, uri: &str) -> Value {
        let text = match self.docs.get(uri) {
            Some(t) => t,
            None => return Value::Null,
        };
        let formatted = match format::format_source(text, uri) {
            Ok(out) => out,
            Err(_) => return Value::Null,
        };
        let (end_line, end_char) = document_end(text);
        json!([{
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": end_line, "character": end_char }
            },
            "newText": formatted
        }])
    }
}

/// The identifier token covering an Xz `(line, col)` position, if any.
fn ident_at(tokens: &[crate::token::Token], pos: (usize, usize)) -> Option<(String, Span)> {
    for tok in tokens {
        if let TokKind::Ident(name) = &tok.kind
            && covers(&tok.span, pos)
        {
            return Some((name.clone(), tok.span.clone()));
        }
    }
    None
}

fn covers(span: &Span, pos: (usize, usize)) -> bool {
    (span.start.0, span.start.1) <= pos && pos < (span.end.0, span.end.1)
}

/// Markdown for the signature and doc claims of the top-level symbol `name`.
fn find_symbol(program: &ast::Program, name: &str) -> Option<String> {
    for item in &program.items {
        match item {
            ast::Item::Func(f) if f.name == name => return Some(with_doc(render_func(f), f.doc.as_ref())),
            ast::Item::Task(t) if t.name == name => {
                return Some(with_doc(format!("task {}", t.name), t.doc.as_ref()));
            }
            ast::Item::Chan(c) if c.name == name => return Some(render_chan(c)),
            ast::Item::Extern(e) if e.name == name => return Some(render_extern(e)),
            ast::Item::Record(r) if r.name == name => return Some(render_record(r)),
            ast::Item::Enum(en) if en.name == name => return Some(render_enum(en)),
            ast::Item::Enum(en) => {
                for v in &en.variants {
                    if v.name == name {
                        return Some(render_variant(v));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// The declaration span of the top-level symbol `name`, if the document
/// declares it. Mirrors [`find_symbol`] for go-to-definition.
fn find_symbol_span(program: &ast::Program, name: &str) -> Option<Span> {
    for item in &program.items {
        match item {
            ast::Item::Func(f) if f.name == name => return Some(f.span.clone()),
            ast::Item::Task(t) if t.name == name => return Some(t.span.clone()),
            ast::Item::Chan(c) if c.name == name => return Some(c.span.clone()),
            ast::Item::Extern(e) if e.name == name => return Some(e.span.clone()),
            ast::Item::Record(r) if r.name == name => return Some(r.span.clone()),
            ast::Item::Enum(en) if en.name == name => return Some(en.span.clone()),
            ast::Item::Enum(en) => {
                for v in &en.variants {
                    if v.name == name {
                        return Some(v.span.clone());
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// LSP CompletionItemKind values (LSP 3.17).
const KIND_FUNCTION: i64 = 3;
const KIND_VARIABLE: i64 = 6;
const KIND_CLASS: i64 = 7;
const KIND_ENUM: i64 = 13;
const KIND_KEYWORD: i64 = 14;
const KIND_ENUM_MEMBER: i64 = 20;
const KIND_CONSTANT: i64 = 21;
const KIND_STRUCT: i64 = 22;

/// Reserved words from the grammar (`docs/11-grammar.md`).
const KEYWORDS: &[&str] = &[
    "and", "as", "async", "await", "break", "chan", "continue", "elif", "else", "enum", "err",
    "extern", "false", "for", "func", "if", "implies", "in", "invariant", "is", "let", "loop",
    "match", "mut", "none", "not", "ok", "or", "post", "pre", "recv", "record", "send", "some",
    "task", "transfer", "true",
];

/// Built-in type names (`docs/03-type-system.md`).
const BUILTIN_TYPES: &[&str] = &[
    "Bool", "Int", "usize", "Float", "Char", "Str", "Bytes", "Unit", "Ptr", "Option", "Result",
    "List", "Map", "Set", "Chan",
];

/// Global stdlib names: free functions and immutable constants (`docs/12-stdlib.md`).
const STDLIB_GLOBALS: &[(&str, i64)] = &[
    ("print", KIND_FUNCTION),
    ("read_file", KIND_FUNCTION),
    ("approx_sqrt", KIND_FUNCTION),
    ("now", KIND_FUNCTION),
    ("monotonic", KIND_FUNCTION),
    ("PI", KIND_CONSTANT),
    ("E", KIND_CONSTANT),
];

/// stdlib error records usable as value constructors (`docs/12-stdlib.md`).
const STDLIB_ERRORS: &[&str] = &[
    "Err", "IoError", "DomainError", "ParseError", "AllocError", "IndexError", "DecodeError",
    "HttpError",
];

/// The completion vocabulary: symbols declared by the document (when it
/// parses), then the static language surface.
fn completion_items(program: Option<&ast::Program>) -> Vec<Value> {
    let mut items: Vec<Value> = vec![];
    if let Some(p) = program {
        for item in &p.items {
            match item {
                ast::Item::Func(f) => items.push(symbol_item(&f.name, KIND_FUNCTION, render_func(f), f.doc.as_ref())),
                ast::Item::Task(t) => {
                    items.push(symbol_item(&t.name, KIND_FUNCTION, format!("task {}", t.name), t.doc.as_ref()))
                }
                ast::Item::Chan(c) => items.push(symbol_item(&c.name, KIND_VARIABLE, render_chan(c), None)),
                ast::Item::Extern(e) => items.push(symbol_item(&e.name, KIND_FUNCTION, render_extern(e), None)),
                ast::Item::Record(r) => items.push(symbol_item(&r.name, KIND_STRUCT, render_record(r), None)),
                ast::Item::Enum(en) => {
                    items.push(symbol_item(&en.name, KIND_ENUM, render_enum(en), None));
                    for v in &en.variants {
                        items.push(symbol_item(&v.name, KIND_ENUM_MEMBER, render_variant(v), None));
                    }
                }
            }
        }
    }
    for (name, kind) in STDLIB_GLOBALS {
        items.push(symbol_item(name, *kind, String::new(), None));
    }
    for name in STDLIB_ERRORS {
        items.push(symbol_item(name, KIND_STRUCT, String::new(), None));
    }
    for name in BUILTIN_TYPES {
        items.push(symbol_item(name, KIND_CLASS, String::new(), None));
    }
    for name in KEYWORDS {
        items.push(symbol_item(name, KIND_KEYWORD, String::new(), None));
    }
    items
}

fn symbol_item(label: &str, kind: i64, detail: String, doc: Option<&ast::DocComment>) -> Value {
    let mut item = json!({ "label": label, "kind": kind });
    if !detail.is_empty() {
        item["detail"] = Value::String(detail);
    }
    if let Some(md) = doc_markdown(doc) {
        item["documentation"] = json!({ "kind": "markdown", "value": md });
    }
    item
}

fn render_extern(e: &ast::ExternDecl) -> String {
    let mut sig = String::from("extern func ");
    sig.push_str(&e.name);
    sig.push_str(&render_type_params(&e.type_params));
    sig.push('(');
    sig.push_str(&render_params(&e.params));
    sig.push(')');
    if let Some(ret) = &e.ret {
        sig.push_str(" -> ");
        sig.push_str(&render_type(ret));
    }
    sig
}

fn render_chan(c: &ast::ChanDecl) -> String {
    format!("chan {}: Chan[{}]", c.name, render_type(&c.payload))
}

fn render_record(r: &ast::RecordDecl) -> String {
    let attr = if r.cstruct { "@cstruct " } else { "" };
    let fields: Vec<String> = r.fields.iter().map(|f| format!("{}: {}", f.name, render_type(&f.ty))).collect();
    format!("{}record {} {{ {} }}", attr, r.name, fields.join(", "))
}

fn render_enum(en: &ast::EnumDecl) -> String {
    let variants: Vec<String> = en.variants.iter().map(render_variant).collect();
    format!("enum {} {{ {} }}", en.name, variants.join(", "))
}

fn render_variant(v: &ast::Variant) -> String {
    let fields: Vec<String> = v.fields.iter().map(|f| format!("{}: {}", f.name, render_type(&f.ty))).collect();
    if fields.is_empty() { v.name.clone() } else { format!("{}({})", v.name, fields.join(", ")) }
}

fn render_func(f: &ast::FuncDecl) -> String {
    let mut sig = String::new();
    if f.exported {
        sig.push_str("@export ");
    }
    if f.is_async {
        sig.push_str("async ");
    }
    sig.push_str("func ");
    sig.push_str(&f.name);
    sig.push_str(&render_type_params(&f.type_params));
    sig.push('(');
    sig.push_str(&render_params(&f.params));
    sig.push(')');
    if let Some(ret) = &f.ret {
        sig.push_str(" -> ");
        sig.push_str(&render_type(ret));
    }
    sig
}

fn render_type_params(params: &[ast::TypeParam]) -> String {
    if params.is_empty() {
        return String::new();
    }
    let items: Vec<String> = params.iter()
        .map(|p| match &p.constraint {
            Some(c) => format!("{}: {}", p.name, c),
            None => p.name.clone(),
        })
        .collect();
    format!("[{}]", items.join(", "))
}

fn render_params(params: &[ast::Param]) -> String {
    let items: Vec<String> = params.iter()
        .map(|p| {
            let m = if p.mutable { "mut " } else { "" };
            format!("{}{}: {}", m, p.name, render_type(&p.ty))
        })
        .collect();
    items.join(", ")
}

fn render_type(ty: &ast::Type) -> String {
    match ty {
        ast::Type::Named(name, args) if args.is_empty() => name.clone(),
        ast::Type::Named(name, args) => {
            let args: Vec<String> = args.iter().map(render_type).collect();
            format!("{}[{}]", name, args.join(", "))
        }
        ast::Type::NamedPlain(name) => name.clone(),
        ast::Type::Union(members) => {
            let members: Vec<String> = members.iter().map(render_type).collect();
            members.join(" | ")
        }
    }
}

fn with_doc(markdown: String, doc: Option<&ast::DocComment>) -> String {
    match doc_markdown(doc) {
        Some(md) => format!("{markdown}\n\n{md}"),
        None => markdown,
    }
}

/// The doc claims of `doc` as markdown lines, or `None` when there are none.
fn doc_markdown(doc: Option<&ast::DocComment>) -> Option<String> {
    let d = doc?;
    if d.claims.is_empty() {
        return None;
    }
    let lines: Vec<String> = d
        .claims
        .iter()
        .map(|c| {
            let tag = match c.tag {
                DocTag::Intent => "@intent",
                DocTag::Requires => "@requires",
                DocTag::Ensures => "@ensures",
                DocTag::Effects => "@effects",
                DocTag::Trusted => "@trusted",
            };
            format!("{} {}", tag, c.text)
        })
        .collect();
    Some(lines.join("\n\n"))
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

/// The 0-based UTF-16 position just past the last character of `text`: the
/// end of the whole document for a formatting `TextEdit`.
fn document_end(text: &str) -> (usize, usize) {
    let lines: Vec<&str> = text.split('\n').collect();
    let line = lines.len() - 1;
    let character = lines[line].chars().map(char::len_utf16).sum();
    (line, character)
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

/// Inverse of [`to_lsp_position`]: a 0-based UTF-16 LSP position back to the
/// 1-based Unicode scalar position the lexer records.
fn from_lsp_position(text: &str, line0: usize, char16: usize) -> (usize, usize) {
    let line_text = text.split('\n').nth(line0).unwrap_or("");
    let mut utf16 = 0usize;
    let mut col = 0usize;
    for ch in line_text.chars() {
        if utf16 >= char16 {
            break;
        }
        utf16 += ch.len_utf16();
        col += 1;
    }
    (line0 + 1, col + 1)
}
