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

fn hover(s: &mut Server, uri: &str, line: usize, character: usize) -> Value {
    let msg = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "textDocument/hover",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }
    })
    .to_string();
    serde_json::from_str(&s.handle(&msg).unwrap()).unwrap()
}

fn completion(s: &mut Server, uri: &str) -> Value {
    let msg = json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "textDocument/completion",
        "params": { "textDocument": { "uri": uri } }
    })
    .to_string();
    serde_json::from_str(&s.handle(&msg).unwrap()).unwrap()
}

fn items<'a>(v: &'a Value) -> &'a Vec<Value> {
    v["result"]["items"].as_array().unwrap()
}

fn item<'a>(items: &'a [Value], label: &str) -> Option<&'a Value> {
    items.iter().find(|i| i["label"] == label)
}

fn definition(s: &mut Server, uri: &str, line: usize, character: usize) -> Value {
    let msg = json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "textDocument/definition",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }
    })
    .to_string();
    serde_json::from_str(&s.handle(&msg).unwrap()).unwrap()
}

fn formatting(s: &mut Server, uri: &str) -> Value {
    let msg = json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "textDocument/formatting",
        "params": {
            "textDocument": { "uri": uri },
            "options": { "tabSize": 4, "insertSpaces": true }
        }
    })
    .to_string();
    serde_json::from_str(&s.handle(&msg).unwrap()).unwrap()
}

#[test]
fn initialize_advertises_full_sync() {
    let mut s = Server::new();
    let v: Value =
        serde_json::from_str(&s.handle(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#).unwrap()).unwrap();
    assert_eq!(v["result"]["capabilities"]["textDocumentSync"], 1);
    assert_eq!(v["result"]["capabilities"]["hoverProvider"], true);
    assert_eq!(v["result"]["capabilities"]["completionProvider"]["resolveProvider"], false);
    assert_eq!(v["result"]["capabilities"]["definitionProvider"], true);
    assert_eq!(v["result"]["capabilities"]["documentFormattingProvider"], true);
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
fn did_open_type_error_publishes_real_span() {
    let mut s = Server::new();
    let publish = did_open(
        &mut s,
        "file:///type.xz",
        "func main() {\n    let x: Int = \"s\"\n    print(x.to_str())\n}",
    );
    let diags = diagnostics(&publish);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0]["code"], "T0001");
    // The `let` on 1-based line 2 must map to LSP line 1, not the (0,0) fallback.
    assert_eq!(diags[0]["range"]["start"]["line"], 1);
    assert_eq!(diags[0]["range"]["start"]["character"], 4);
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
        serde_json::from_str(&s.handle(r#"{"jsonrpc":"2.0","id":7,"method":"textDocument/rename","params":{}}"#).unwrap()).unwrap();
    assert_eq!(v["id"], 7);
    assert_eq!(error_code(&v), -32601);
}

#[test]
fn hover_on_function_use_returns_signature_and_doc() {
    let mut s = Server::new();
    did_open(&mut s, "file:///hover.xz", VALID);
    let v = hover(&mut s, "file:///hover.xz", 11, 4);
    assert_eq!(v["result"]["contents"]["kind"], "markdown");
    let value = v["result"]["contents"]["value"].as_str().unwrap();
    assert!(value.contains("func one() -> Int"), "value was {value:?}");
    assert!(value.contains("@intent"), "value was {value:?}");
    assert_eq!(v["result"]["range"]["start"]["line"], 11);
    assert_eq!(v["result"]["range"]["start"]["character"], 4);
}

#[test]
fn hover_off_identifier_returns_null() {
    let mut s = Server::new();
    did_open(&mut s, "file:///hover.xz", VALID);
    let v = hover(&mut s, "file:///hover.xz", 11, 9);
    assert!(v["result"].is_null());
}

#[test]
fn hover_on_unknown_identifier_returns_null() {
    let mut s = Server::new();
    did_open(&mut s, "file:///unknown.xz", "func main() {\n    nope()\n}");
    let v = hover(&mut s, "file:///unknown.xz", 1, 4);
    assert!(v["result"].is_null());
}

#[test]
fn hover_on_unopened_document_returns_null() {
    let mut s = Server::new();
    let v = hover(&mut s, "file:///missing.xz", 0, 0);
    assert!(v["result"].is_null());
}

#[test]
fn completion_lists_document_symbols_and_vocabulary() {
    let mut s = Server::new();
    did_open(&mut s, "file:///complete.xz", VALID);
    let v = completion(&mut s, "file:///complete.xz");
    assert_eq!(v["result"]["isIncomplete"], false);
    let items = items(&v);
    let one = item(items, "one").expect("document func missing");
    assert_eq!(one["kind"], 3);
    assert_eq!(one["detail"], "func one() -> Int");
    assert!(one["documentation"]["value"].as_str().unwrap().contains("@intent"));
    assert_eq!(item(items, "func").unwrap()["kind"], 14);
    assert_eq!(item(items, "Int").unwrap()["kind"], 7);
    assert_eq!(item(items, "read_file").unwrap()["kind"], 3);
    assert_eq!(item(items, "PI").unwrap()["kind"], 21);
    assert_eq!(item(items, "IoError").unwrap()["kind"], 22);
}

#[test]
fn completion_works_while_document_has_parse_error() {
    let mut s = Server::new();
    did_open(&mut s, "file:///broken.xz", "func main( {");
    let v = completion(&mut s, "file:///broken.xz");
    let items = items(&v);
    assert!(item(items, "func").is_some());
    assert!(item(items, "main").is_none());
}

#[test]
fn completion_on_unopened_document_is_empty() {
    let mut s = Server::new();
    let v = completion(&mut s, "file:///missing.xz");
    assert_eq!(v["result"]["isIncomplete"], false);
    assert!(items(&v).is_empty());
}

#[test]
fn definition_on_function_use_points_at_declaration() {
    let mut s = Server::new();
    did_open(&mut s, "file:///def.xz", VALID);
    let v = definition(&mut s, "file:///def.xz", 11, 4);
    assert_eq!(v["result"]["uri"], "file:///def.xz");
    assert_eq!(v["result"]["range"]["start"]["line"], 4);
    assert_eq!(v["result"]["range"]["start"]["character"], 5);
}

#[test]
fn definition_on_enum_variant_points_at_variant() {
    let mut s = Server::new();
    did_open(&mut s, "file:///def.xz", "enum Color {\n    red()\n}\n\nfunc main() {\n    let c = Color.red()\n}");
    let v = definition(&mut s, "file:///def.xz", 5, 18);
    assert_eq!(v["result"]["range"]["start"]["line"], 1);
    assert_eq!(v["result"]["range"]["start"]["character"], 4);
}

#[test]
fn definition_off_identifier_returns_null() {
    let mut s = Server::new();
    did_open(&mut s, "file:///def.xz", VALID);
    let v = definition(&mut s, "file:///def.xz", 11, 9);
    assert!(v["result"].is_null());
}

#[test]
fn definition_on_unknown_identifier_returns_null() {
    let mut s = Server::new();
    did_open(&mut s, "file:///unknown.xz", "func main() {\n    nope()\n}");
    let v = definition(&mut s, "file:///unknown.xz", 1, 4);
    assert!(v["result"].is_null());
}

#[test]
fn definition_on_unopened_document_returns_null() {
    let mut s = Server::new();
    let v = definition(&mut s, "file:///missing.xz", 0, 0);
    assert!(v["result"].is_null());
}

#[test]
fn formatting_replaces_whole_document_with_canonical_layout() {
    let mut s = Server::new();
    let text = "func main(){print(1)}";
    did_open(&mut s, "file:///fmt.xz", text);
    let v = formatting(&mut s, "file:///fmt.xz");
    let edits = v["result"].as_array().unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0]["newText"], "func main() {\n    print(1)\n}\n");
    assert_eq!(edits[0]["range"]["start"]["line"], 0);
    assert_eq!(edits[0]["range"]["start"]["character"], 0);
    assert_eq!(edits[0]["range"]["end"]["line"], 0);
    assert_eq!(edits[0]["range"]["end"]["character"], text.chars().count());
}

#[test]
fn formatting_on_parse_error_returns_null() {
    let mut s = Server::new();
    did_open(&mut s, "file:///fmt.xz", "func main( {");
    let v = formatting(&mut s, "file:///fmt.xz");
    assert!(v["result"].is_null());
}

#[test]
fn formatting_on_unopened_document_returns_null() {
    let mut s = Server::new();
    let v = formatting(&mut s, "file:///missing.xz");
    assert!(v["result"].is_null());
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