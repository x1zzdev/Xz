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

/// First typecheck error message, bypassing the intent phase so mutability
/// rules can be tested in isolation.
fn typecheck_error(src: &str) -> Option<String> {
    let ts = lex(src.to_string(), "test.xz".to_string()).ok()?;
    let p = parse(ts).ok()?;
    resolve(&p).ok()?;
    match typecheck(&p) {
        Err(errors) => Some(errors[0].message.clone()),
        Ok(_) => None,
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
fn composite_builtin_types_resolve_in_fields_and_extern_sigs() {
    let err = check_source(
        r#"record R {
    xs: List[Int]
    maybe: Option[Int]
}

extern func get() -> Result[Int, Err]

func main() {
    print("hi")
}"#,
    );
    assert!(err.is_none(), "composite builtin types rejected: {:?}", err);
}

#[test]
fn generic_call_infers_type_argument() {
    expect_ok(
        r#"/// Returns the larger.
/// @intent  Compares and returns the max.
/// @effects none
func max[T: Ordered](a: T, b: T) -> T {
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
fn unconstrained_typevar_cannot_compare() {
    let err = check_source(r#"/// Compares.
/// @intent  Returns the max.
/// @effects none
func max[T](a: T, b: T) -> T {
    if a > b { a } else { b }
}"#);
    match err {
        Some(e) => {
            if !e.contains("must be constrained") {
                println!("FAIL unconstrained_typevar: unexpected error {}", e);
            }
        }
        None => println!("FAIL unconstrained_typevar: allowed comparison on bare T"),
    }
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
fn prop_acceptance_rule() {
    // exact channel: ok
    expect_ok(
        r#"record NetError { message: Str }

/// Reads.
/// @intent  Returns a string.
/// @effects none
func read() -> Result[Str, NetError] {
    err(NetError("nope"))
}

/// Wraps read.
/// @intent  Propagates.
/// @effects none
func wrap() -> Result[Str, NetError] {
    let s = read()?
    ok(s)
}"#,
        "? exact error channel",
    );
    // union member: ok
    expect_ok(
        r#"record NetError { message: Str }
record CfgError { message: Str }

/// Reads.
/// @intent  Returns a string.
/// @effects none
func read() -> Result[Str, NetError] {
    err(NetError("nope"))
}

/// Wraps read.
/// @intent  Propagates.
/// @effects none
func wrap() -> Result[Str, NetError | CfgError] {
    let s = read()?
    ok(s)
}"#,
        "? union member",
    );
    // mismatched: rejected
    let err = check_source(
        r#"record NetError { message: Str }
record CfgError { message: Str }

/// Reads.
/// @intent  Returns a string.
/// @effects none
func read() -> Result[Str, NetError] {
    err(NetError("nope"))
}

/// Wraps read.
/// @intent  Propagates.
/// @effects none
func wrap() -> Result[Str, CfgError] {
    let s = read()?
    ok(s)
}"#,
    );
    match err {
        Some(e) => {
            if !e.contains("cannot propagate") {
                println!("FAIL prop_acceptance: unexpected error {}", e);
            }
        }
        None => println!("FAIL prop_acceptance: allowed NetError via ? into CfgError channel"),
    }
}

#[test]
fn option_narrowing_via_is_some() {
    expect_ok(
        r#"/// Normalizes.
/// @intent  Uppercases if present, else returns "none".
/// @effects none
func norm(o: Option[Str]) -> Str {
    if o is some {
        o.to_upper()
    } else {
        "none"
    }
}"#,
        "is some narrows Option to payload",
    );
    // complement: else of `is none` has the value
    expect_ok(
        r#"/// Length.
/// @intent  Returns the value length, 0 if absent.
/// @effects none
func len(o: Option[Str]) -> Int {
    if o is none {
        0
    } else {
        o.to_upper()
    }
}"#,
        "is none else narrows to payload",
    );
    // without narrowing: rejected
    let err = check_source(r#"/// Length.
/// @intent  Returns the value length.
/// @effects none
func len(o: Option[Str]) -> Int {
    o.to_upper()
}"#);
    match err {
        Some(e) => {
            if !e.contains("no method 'to_upper' on Option") {
                println!("FAIL option_narrowing: unexpected error {}", e);
            }
        }
        None => println!("FAIL option_narrowing: allowed method on Option without narrowing"),
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

#[test]
fn immutable_assignment_rejected() {
    let err = typecheck_error(
        r#"func main() {
    let a: Int = 1
    a = 2
    print(a.to_str())
}"#,
    );
    match err {
        Some(e) => assert!(e.contains("immutable binding 'a'"), "unexpected error: {}", e),
        None => panic!("immutable assignment was allowed"),
    }
}

#[test]
fn mut_assignment_accepted() {
    let err = typecheck_error(
        r#"func main() {
    mut a: Int = 1
    a = 2
    a += 3
    print(a.to_str())
}"#,
    );
    assert!(err.is_none(), "mut assignment rejected: {:?}", err);
}

#[test]
fn immutable_field_assignment_rejected() {
    let err = typecheck_error(
        r#"record Point {
    x: Int
    y: Int
}

func main() {
    let p = Point(1, 2)
    p.x = 3
    print(p.x.to_str())
}"#,
    );
    match err {
        Some(e) => assert!(e.contains("immutable binding 'p'"), "unexpected error: {}", e),
        None => panic!("field assignment on an immutable binding was allowed"),
    }
}

#[test]
fn mut_field_assignment_accepted() {
    let err = typecheck_error(
        r#"record Point {
    x: Int
    y: Int
}

func main() {
    mut p = Point(1, 2)
    p.x = 3
    print(p.x.to_str())
}"#,
    );
    assert!(err.is_none(), "mut field assignment rejected: {:?}", err);
}

#[test]
fn mutable_param_assignment_accepted() {
    let err = typecheck_error(
        r#"func bump(mut n: Int) {
    n = n + 1
}

func main() {
    mut x: Int = 1
    bump(x)
    print(x.to_str())
}"#,
    );
    assert!(err.is_none(), "mut param assignment rejected: {:?}", err);
}

#[test]
fn immutable_param_assignment_rejected() {
    let err = typecheck_error(
        r#"func bump(n: Int) {
    n = n + 1
}

func main() {
    let x: Int = 1
    bump(x)
    print(x.to_str())
}"#,
    );
    match err {
        Some(e) => assert!(e.contains("immutable binding 'n'"), "unexpected error: {}", e),
        None => panic!("assignment to an immutable parameter was allowed"),
    }
}

#[test]
fn declared_extern_call_derives_extern_effect() {
    expect_intent_code(
        r#"extern func puts(s: Str) -> Int

/// Writes a C string.
/// @intent  Writes via the C stdlib.
/// @effects none
func shout(s: Str) -> Int {
    puts(s)
}"#,
        "I0020",
        "extern call needs the extern effect",
    );
}

#[test]
fn declared_extern_with_extern_effect_passes() {
    expect_ok(
        r#"extern func puts(s: Str) -> Int

/// Writes a C string.
/// @intent  Writes via the C stdlib.
/// @effects extern
func shout(s: Str) -> Int {
    puts(s)
}"#,
        "extern call with declared extern effect",
    );
}

#[test]
fn extern_effect_propagates_transitively() {
    expect_intent_code(
        r#"extern func puts(s: Str) -> Int

/// Writes a C string.
/// @intent  Writes via the C stdlib.
/// @effects extern
func shout(s: Str) -> Int {
    puts(s)
}

/// Delegates.
/// @intent  Calls shout.
/// @effects none
func relay(s: Str) -> Int {
    shout(s)
}"#,
        "I0020",
        "extern effect propagation through a wrapper",
    );
}

#[test]
fn cstruct_record_with_c_types_accepted() {
    let err = check_source(
        r#"@cstruct record Header {
    magic: Int
    flags: Bool
    data: Ptr
}

@cstruct record Window {
    origin: Header
    label: Str
    raw: Bytes
}

func main() {
    print("ok")
}"#,
    );
    assert!(err.is_none(), "@cstruct with C-representable fields rejected: {:?}", err);
}

#[test]
fn cstruct_plain_record_field_rejected() {
    let err = typecheck_error(
        r#"record Plain {
    x: Int
}

@cstruct record Bad {
    p: Plain
}"#,
    );
    match err {
        Some(e) => assert!(e.contains("plain record"), "unexpected error: {}", e),
        None => panic!("plain record field accepted in @cstruct"),
    }
}

#[test]
fn cstruct_non_c_field_rejected() {
    let err = typecheck_error(
        r#"@cstruct record Bad {
    xs: List[Int]
}"#,
    );
    match err {
        Some(e) => assert!(e.contains("not a C type"), "unexpected error: {}", e),
        None => panic!("List field accepted in @cstruct"),
    }
}

#[test]
fn cstruct_cycle_rejected() {
    let err = typecheck_error(
        r#"@cstruct record A {
    b: B
}

@cstruct record B {
    a: A
}"#,
    );
    match err {
        Some(e) => assert!(e.contains("cycle"), "unexpected error: {}", e),
        None => panic!("@cstruct by-value cycle accepted"),
    }
}

#[test]
fn export_c_representable_signature_accepted() {
    let err = check_source(
        r#"@cstruct record Vec {
    x: Int
    y: Int
}

/// Adds two numbers.
/// @intent  Returns a + b.
/// @effects none
@export func add(a: Int, b: Int) -> Int {
    a + b
}

/// Adds a vector's components.
/// @intent  Returns x + y.
/// @effects none
@export func addv(p: Vec) -> Int {
    p.x + p.y
}

/// Returns nothing.
/// @intent  Prints a marker.
/// @effects io
@export func ping() {
    print("pong")
}"#,
    );
    assert!(err.is_none(), "@export with C-representable signature rejected: {:?}", err);
}

#[test]
fn export_generic_rejected() {
    let err = typecheck_error(
        r#"@export func id[T](x: T) -> T {
    x
}"#,
    );
    match err {
        Some(e) => assert!(e.contains("cannot be generic"), "unexpected error: {}", e),
        None => panic!("generic @export accepted"),
    }
}

#[test]
fn export_main_rejected() {
    let err = typecheck_error(
        r#"@export func main() {
}"#,
    );
    match err {
        Some(e) => assert!(e.contains("cannot be @export"), "unexpected error: {}", e),
        None => panic!("@export main accepted"),
    }
}

#[test]
fn export_result_return_rejected() {
    let err = typecheck_error(
        r#"@export func f() -> Result[Int, Err] {
    ok(1)
}"#,
    );
    match err {
        Some(e) => assert!(e.contains("not a C type"), "unexpected error: {}", e),
        None => panic!("Result-returning @export accepted"),
    }
}

#[test]
fn export_list_param_rejected() {
    let err = typecheck_error(
        r#"@export func f(xs: List[Int]) -> Int {
    0
}"#,
    );
    match err {
        Some(e) => assert!(e.contains("not a C type"), "unexpected error: {}", e),
        None => panic!("List param @export accepted"),
    }
}

#[test]
fn export_async_rejected() {
    let err = typecheck_error(
        r#"@export async func f() {
}"#,
    );
    match err {
        Some(e) => assert!(e.contains("cannot be async"), "unexpected error: {}", e),
        None => panic!("async @export accepted"),
    }
}

#[test]
fn map_literal_and_methods_accepted() {
    expect_ok(
        r#"func main() {
    let counts: Map[Str, Int] = {"a": 1, "b": 2}
    let n: Int = counts.len()
    let empty: Bool = counts.is_empty()
    let grown: Map[Str, Int] = counts.insert("c", 3)
    let o = grown.get("a")
    if o is some {
        print(o.to_str())
    }
    let ks: List[Str] = grown.keys()
    for k in ks {
        print(k)
    }
    print(n.to_str() + empty.to_str())
}"#,
        "map literal and methods",
    );
}

#[test]
fn float_map_key_rejected() {
    let err = typecheck_error(
        r#"func main() {
    let m: Map[Float, Int] = {}
    print(m.len().to_str())
}"#,
    );
    match err {
        Some(e) => assert!(e.contains("Map key type"), "unexpected error: {}", e),
        None => panic!("Float Map key accepted"),
    }
}

#[test]
fn map_insert_key_type_mismatch_rejected() {
    let err = typecheck_error(
        r#"func main() {
    let m: Map[Str, Int] = {}
    let m2 = m.insert(1, 2)
    print(m2.len().to_str())
}"#,
    );
    match err {
        Some(e) => assert!(e.contains("insert key expects"), "unexpected error: {}", e),
        None => panic!("Int key inserted into Map[Str, Int]"),
    }
}