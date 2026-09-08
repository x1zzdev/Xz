/// Structured diagnostics emitted by the compiler — the machine-readable
/// feedback loop for LLM self-correction (docs/07-compiler.md).
///
/// JSON shape follows the spec:
///   { "version": 1, "severity": "error", "code": "I0020",
///     "message": "...", "category": "intent",
///     "span": { "file": "...", "start": [l, c], "end": [l, c] } }
///
/// Initial code scheme (stable, never renumbered):
///   L0001  lexer error
///   P0001  parse error
///   R0001  name-resolution error
///   T0001  type error
///   Ixxxx  intent-verification errors (see docs/09)

#[derive(Clone, Debug, PartialEq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Category {
    Lex,
    Parse,
    Resolve,
    Type,
    Intent,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub file: String,
    pub start: (usize, usize),
    pub end: (usize, usize),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub version: u32,
    pub severity: Severity,
    pub code: String,
    pub message: String,
    pub category: Category,
    pub span: Span,
}

pub fn json_escape(s: &str) -> String {
    let mut out: String = "".to_string();
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}

impl Span {
    fn to_json(&self) -> String {
        let (sl, sc) = self.start;
        let (el, ec) = self.end;
        format!(
            "{{\"file\":\"{}\",\"start\":[{sl},{sc}],\"end\":[{el},{ec}]}}",
            json_escape(&self.file)
        )
    }
}

impl Diagnostic {
    pub fn to_json(&self) -> String {
        let sev = match &self.severity {
            Severity::Error => "error".to_string(),
            Severity::Warning => "warning".to_string(),
        };
        let cat = match &self.category {
            Category::Lex => "lex".to_string(),
            Category::Parse => "parse".to_string(),
            Category::Resolve => "resolve".to_string(),
            Category::Type => "type".to_string(),
            Category::Intent => "intent".to_string(),
        };
        format!(
            "{{\"version\":{},\"severity\":\"{sev}\",\"code\":\"{}\",\"message\":\"{}\",\"category\":\"{cat}\",\"span\":{}}}",
            self.version, self.code, json_escape(&self.message), self.span.to_json()
        )
    }
}

/// Serialize a list of diagnostics as a JSON array (one object per line is
/// not required; we emit a compact array).
pub fn to_json_array(diags: &Vec<Diagnostic>) -> String {
    let mut parts: Vec<String> = vec![];
    for d in diags {
        parts.push(d.to_json());
    }
    format!("[{}]", parts.join(","))
}