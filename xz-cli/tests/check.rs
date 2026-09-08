use xz_cli::lexer::lex;
use xz_cli::parser::parse;
use xz_cli::resolve::resolve;
use xz_cli::typecheck::typecheck;
use xz_cli::intent::check_intent;

/// Run the full check pipeline on source; return Some(first error string) or
/// None when everything passes.
fn check_source(source: &str) -> Option<String> {
    let tokens = lex(source.to_string(), "test.xz".to_string());
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
                    match check_intent(&p) {
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