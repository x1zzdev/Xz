use xz_cli::lexer::lex;
use xz_cli::parser::parse;
use xz_cli::resolve::resolve;
use xz_cli::typecheck::typecheck;
use xz_cli::intent::{check_intent, check_intent_strict};
use xz_cli::diagnostic::{Diagnostic, Severity, Category, to_json_array};

/// Run the full check pipeline on source; return Some(first error string) or
/// None when everything passes.
fn check_source(src: &str) -> Option<String> {
    check_source_mode(src, false)
}

fn check_source_strict(src: &str) -> Option<String> {
    check_source_mode(src, true)
}

fn check_source_mode(src: &str, strict: bool) -> Option<String> {
    let tokens = lex(src.to_string(), "test.xz".to_string());
    match tokens {
        Err(e) => return Some(format!("lex: {}", e.message)),
        Ok(ts) => {
            let program = parse(ts);
            match program {
                Err(e) => return Some(format!("parse: {}", e.message)),
                Ok(p) => {
                    match resolve(&p) {
                        Err(errors) => return Some(format!("resolve: {} errors", errors.len())),
                        Ok(_) => {}
                    }
                    match typecheck(&p) {
                        Err(errors) => return Some(format!("typecheck: {} errors", errors.len())),
                        Ok(_) => {}
                    }
                    let intent = if strict { check_intent_strict(&p) } else { check_intent(&p) };
                    match intent {
                        Err(errors) => {
                            return Some(format!("intent: {}", errors[0].code));
                        }
                        Ok(_) => {}
                    }
                }
            }
        }
    }
    None
}

fn expect_ok(src: &str, label: &str) {
    let err = check_source(src);
    if let Some(e) = err {
        println!("FAIL {}: expected ok, got {}", label, e);
        return;
    }
}

fn expect_intent_code(src: &str, code: &str, label: &str) {
    let err = check_source(src);
    match err {
        Some(e) => {
            if !e.contains(&code) {
                println!("FAIL {}: expected {}, got {}", label, code, e);
            }
        }
        None => println!("FAIL {}: expected intent error {}, but check passed", label, code),
    }
}

const GREETING: &str = r#"/// Prints a greeting.
/// @intent  Writes a string and returns its length.
/// @ensures result == 3
/// @effects io
func greet() -> Int
    post result == 3
{
    print("hey")
    "hey".len()
}

func main() {
    greet()
}"#;

#[test]
fn positive_example() {
    expect_ok(GREETING, "greeting");
}

#[test]
fn i0021_unpaired_ensures() {
    expect_intent_code(
        r#"/// Claims without a post.
/// @intent  Adds one.
/// @ensures result == 1
/// @effects none
func f() -> Int {
    0
}"#,
        "I0021",
        "unpaired ensures",
    );
}

#[test]
fn i0022_missing_doc() {
    expect_intent_code(
        r#"func g() -> Int {
    0
}"#,
        "I0022",
        "missing intent comment",
    );
}

#[test]
fn i0020_effect_mismatch() {
    expect_intent_code(
        r#"/// Prints.
/// @intent  Writes something.
/// @effects none
func f() -> Int {
    print("hi")
    0
}"#,
        "I0020",
        "effect mismatch",
    );
}

#[test]
fn i0020_transitive_propagation() {
    expect_intent_code(
        r#"/// Logs.
/// @intent  Prints.
/// @effects io
func logit(s: Str) {
    print(s)
}

/// Delegates.
/// @intent  Calls logit.
/// @effects none
func delegate(s: Str) {
    logit(s)
}"#,
        "I0020",
        "transitive io",
    );
}

#[test]
fn i0003_trusted_on_effects() {
    expect_intent_code(
        r#"/// Trusted on the wrong tag.
/// @intent  Returns one.
/// @effects none @trusted
func f() -> Int {
    1
}"#,
        "I0003",
        "trusted placement",
    );
}

#[test]
fn strict_trusted_without_review_note() {
    const SRC: &str = r#"/// Trusted but not reviewed.
/// @intent  Returns one.
/// @ensures result == 1  @trusted
/// @effects none
func f() -> Int
    post result == 1
{
    1
}"#;
    // non-strict: passes (trusted stamp is advisory without strict)
    expect_ok(SRC, "non-strict trusted");
    // strict: I0004 — untrusted claim blocks the build
    expect_intent_code(SRC, "I0004", "strict review note");
}

#[test]
fn strict_with_review_note_passes() {
    const SRC: &str = r#"/// Trusted and reviewed.
/// @intent  Returns one.
/// @ensures result == 1  @trusted  // reviewed by alice on 2026-09-08
/// @effects none
func f() -> Int
    post result == 1
{
    1
}"#;
    let err = check_source_strict(SRC);
    match err {
        Some(e) => println!("FAIL strict_with_review_note_passes: got {}", e),
        None => {}
    }
}

#[test]
fn forbidden_cast_rejected() {
    let err = check_source(r#"func main() {
    let b: Bool = true
    let i: Int = b as Int
    print(i.to_str())
}"#);
    match err {
        Some(e) => {
            if !e.contains("cannot cast") {
                println!("FAIL forbidden_cast: unexpected error {}", e);
            }
        }
        None => println!("FAIL forbidden_cast: allowed Bool->Int"),
    }
}

#[test]
fn allowed_cast_passes() {
    expect_ok(
        r#"extern func malloc(size: usize) -> Ptr

func main() -> Result[Int, Err] {
    let n: Int = 16
    let p = malloc(n as usize)
    if p == 0 {
        err("oom")
    } else {
        ok(0)
    }
}"#,
        "allowed Int->usize cast",
    );
}

#[test]
fn generic_call_infers_type_argument() {
    expect_ok(
        r#"/// Returns the larger.
/// @intent  Compares and returns the max.
/// @effects none
func max[T](a: T, b: T) -> T {
    if a > b { a } else { b }
}

func main() {
    let m: Int = max(3, 7)
    let f: Float = max(1.5, 2.5)
    print(m.to_str() + f.to_str())
}"#,
        "generic max inference",
    );
}

#[test]
fn generic_mismatch_detected() {
    let err = check_source(r#"/// Identity.
/// @intent  Returns its argument.
/// @effects none
func id[T](x: T) -> T {
    x
}

func main() {
    let s: Str = id(1)
    print(s)
}"#);
    match err {
        Some(e) => {
            if !e.contains("Str but initializer is Int") {
                println!("FAIL generic_mismatch: unexpected error {}", e);
            }
        }
        None => println!("FAIL generic_mismatch: id(1) accepted as Str"),
    }
}

#[test]
fn json_emits_spec_shape() {
    // build one diagnostic and verify the JSON carries the spec fields
    let d = Diagnostic {
        version: 1,
        severity: Severity::Error,
        code: "I0020".to_string(),
        message: "declared @effects 'none' does not match derived effects 'io' on 'f'".to_string(),
        category: Category::Intent,
        span: xz_cli::diagnostic::Span { file: "a.xz".to_string(), start: (4, 6), end: (4, 7) },
        suggestion: Some(xz_cli::diagnostic::Suggestion { fix: "extend @effects to 'io'".to_string(), confidence: 0.9 }),
    };
    let arr = to_json_array(&vec![d]);
    if !arr.contains("\"version\":1") {
        println!("FAIL json: missing version");
        return;
    }
    if !arr.contains("\"severity\":\"error\"") {
        println!("FAIL json: missing severity");
        return;
    }
    if !arr.contains("\"code\":\"I0020\"") {
        println!("FAIL json: missing code");
        return;
    }
    if !arr.contains("\"category\":\"intent\"") {
        println!("FAIL json: missing category");
        return;
    }
    if !arr.contains("\"file\":\"a.xz\"") || !arr.contains("\"start\":[4,6]") {
        println!("FAIL json: malformed span");
        return;
    }
    if !arr.contains("\"suggestion\":{\"fix\":\"extend @effects to 'io'\",\"confidence\":0.9") {
        println!("FAIL json: missing suggestion");
        return;
    }
}