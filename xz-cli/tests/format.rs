use xz_cli::format::format_source;

fn fmt(src: &str) -> String {
    format_source(src, "test.xz").expect("expected the source to parse")
}

#[test]
fn canonical_spacing_and_redundant_parens() {
    let src = "func main() {\n    let x: Int = 1+2\n    let y = (x*3)\n}\n";
    let expected = "func main() {\n    let x: Int = 1 + 2\n    let y = x * 3\n}\n";
    assert_eq!(fmt(src), expected);
}

#[test]
fn needed_parens_are_reinserted() {
    let src = "func main() {\n    let x = (1 + 2) * 3\n}\n";
    let expected = "func main() {\n    let x = (1 + 2) * 3\n}\n";
    assert_eq!(fmt(src), expected);
}

#[test]
fn comments_and_doc_claims_are_preserved_verbatim() {
    let src = "// file header\n\n\
/// Adds two values.\n\
/// @intent  Returns a + b.\n\
/// @effects none\n\
func add(a: Int, b: Int) -> Int {\n\
    // the sum\n\
    a+b\n\
}\n";
    let out = fmt(src);
    assert!(out.contains("// file header"));
    assert!(out.contains("/// Adds two values."));
    assert!(out.contains("/// @intent  Returns a + b."));
    assert!(out.contains("/// @effects none"));
    assert!(out.contains("    // the sum"));
    assert!(out.contains("a + b"));
}

#[test]
fn trailing_comment_stays_on_its_line() {
    let src = "func main() {\n    print(\"a\")  // note\n}\n";
    let expected = "func main() {\n    print(\"a\")  // note\n}\n";
    assert_eq!(fmt(src), expected);
}

#[test]
fn blank_lines_are_capped_at_one_and_preserved() {
    let src = "func a() {\n    1\n}\n\n\n\nfunc b() {\n    2\n}\n";
    let expected = "func a() {\n    1\n}\n\nfunc b() {\n    2\n}\n";
    assert_eq!(fmt(src), expected);
}

#[test]
fn contracts_go_on_their_own_lines_before_the_brace() {
    let src = "func f(x: Int) -> Int pre  x > 0 { x }\n";
    let expected = "func f(x: Int) -> Int\n    pre x > 0\n{\n    x\n}\n";
    assert_eq!(fmt(src), expected);
}

#[test]
fn block_expressions_are_multiline() {
    let src = "func main() {\n    if true { print(\"y\") } else { print(\"n\") }\n}\n";
    let expected = "func main() {\n    if true {\n        print(\"y\")\n    } else {\n        print(\"n\")\n    }\n}\n";
    assert_eq!(fmt(src), expected);
}

#[test]
fn formatting_is_idempotent() {
    let src = "/// Doc.\n/// @intent  Does a thing.\n/// @effects none\nfunc main() {\n\n    // lead\n    let x=1+2 // trail\n\n\n    if x>2 { print(\"big\") } else { print(\"small\") }\n}\n";
    let once = fmt(src);
    let twice = fmt(&once);
    assert_eq!(once, twice);
}

#[test]
fn formatted_output_still_checks() {
    let src = "func main() -> Result[Unit, Err] {\n    let n = 1+2\n    print(n.to_str())\n    ok()\n}\n";
    let formatted = fmt(src);
    let tokens = match xz_cli::lexer::lex(formatted, "test.xz".to_string()) {
        Ok(t) => t,
        Err(e) => panic!("lex: {}", e.message),
    };
    let program = xz_cli::parser::parse(tokens).expect("parse");
    assert!(xz_cli::resolve::resolve(&program).is_ok());
    assert!(xz_cli::typecheck::typecheck(&program).is_ok());
    assert!(xz_cli::intent::check_intent(&program).is_ok());
}

#[test]
fn block_comments_are_preserved() {
    let src = "func main() {\n    /* note */\n    let x = 1\n}\n";
    let out = fmt(src);
    assert!(out.contains("/* note */"), "block comment missing: {}", out);
}

#[test]
fn transfer_param_modifier_is_preserved() {
    let src = "extern func write(transfer frame: Bytes, count: Int) -> Int\n";
    let expected = "extern func write(transfer frame: Bytes, count: Int) -> Int\n";
    assert_eq!(fmt(src), expected);
}

#[test]
fn parse_error_is_reported() {
    let err = format_source("func main( {\n", "test.xz").unwrap_err();
    assert!(err.contains("test.xz"), "error should carry the file: {}", err);
}
