use serde_json::{json, Value};
use xz_cli::lsp::Server;

const VALID: &str = r#"/// Returns one.
/// @intent  Returns the constant one.
/// @ensures result == 1
/// @effects none
func one() -> Int
    post result == 1
{
    1
}

func main() {
    one()
}"#;

fn error_code(v: &Value) -> i64 {
    v["error"]["code"].as_i64().unwrap()
}

fn did_open(s: &mut Server, uri: &str, text: &str) -> Value {
    let msg = json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": { "textDocument": { "uri": uri, "text": text } }
    })
    .to_string();
    serde_json::from_str(&s.handle(&msg).unwrap()).unwrap()
}

fn diagnostics(publish: &Value) -> &Vec<Value> {
    publish["params"]["diagnostics"].as_array().unwrap()
}

#[test]
fn initialize_advertises_full_sync() {
    let mut s = Server::new();
    let v: Value =
        serde_json::from_str(&s.handle(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#).unwrap()).unwrap();
    assert_eq!(v["result"]["capabilities"]["textDocumentSync"], 1);
    assert_eq!(v["result"]["serverInfo"]["name"], "xz");
}

#[test]
fn did_open_valid_program_publishes_no_diagnostics() {
    let mut s = Server::new();
    let publish = did_open(&mut s, "file:///valid.xz", VALID);
    assert_eq!(publish["method"], "textDocument/publishDiagnostics");
    assert!(diagnostics(&publish).is_empty());
}

#[test]
fn did_open_parse_error_is_published() {
    let mut s = Server::new();
    let publish = did_open(&mut s, "file:///bad.xz", "func main( {");
    let diags = diagnostics(&publish);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0]["code"], "P0001");
    assert_eq!(diags[0]["source"], "xz");
    assert_eq!(diags[0]["severity"], 1);
}

#[test]
fn did_open_lex_error_is_published_as_l0001() {
    let mut s = Server::new();
    let publish = did_open(&mut s, "file:///lex.xz", "let x: Str = $");
    let diags = diagnostics(&publish);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0]["code"], "L0001");
}

#[test]
fn did_change_recomputes_diagnostics() {
    let mut s = Server::new();
    did_open(&mut s, "file:///doc.xz", "func main( {");
    let msg = json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": "file:///doc.xz" },
            "contentChanges": [ { "text": VALID } ]
        }
    })
    .to_string();
    let publish: Value = serde_json::from_str(&s.handle(&msg).unwrap()).unwrap();
    assert!(diagnostics(&publish).is_empty());
}

#[test]
fn did_close_clears_diagnostics() {
    let mut s = Server::new();
    did_open(&mut s, "file:///doc.xz", "func main( {");
    let msg = r#"{"jsonrpc":"2.0","method":"textDocument/didClose","params":{"textDocument":{"uri":"file:///doc.xz"}}}"#;
    let publish: Value = serde_json::from_str(&s.handle(msg).unwrap()).unwrap();
    assert!(diagnostics(&publish).is_empty());
}

#[test]
fn unknown_request_returns_method_not_found() {
    let mut s = Server::new();
    let v: Value =
        serde_json::from_str(&s.handle(r#"{"jsonrpc":"2.0","id":7,"method":"textDocument/hover","params":{}}"#).unwrap()).unwrap();
    assert_eq!(v["id"], 7);
    assert_eq!(error_code(&v), -32601);
}

#[test]
fn shutdown_then_exit_sets_flags() {
    let mut s = Server::new();
    let v: Value = serde_json::from_str(&s.handle(r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#).unwrap()).unwrap();
    assert!(v["error"].is_null());
    assert!(s.shutdown_received());
    assert!(s.handle(r#"{"jsonrpc":"2.0","method":"exit"}"#).is_none());
    assert!(s.should_exit());
}