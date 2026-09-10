// Phase 4 backend tests: compile a program through the full pipeline and
// execute it in the JIT engine. These catch codegen regressions (verifier
// failures, wrong lowering) that the front-end-only suite in check.rs cannot.
//
// Run with `cargo test` from xz-cli/ (LLVM env is pinned in .cargo/config.toml).

use xz_cli::backend::llvm_backend::compile;
use xz_cli::backend::runtime::run;
use xz_cli::intent::check_intent;
use xz_cli::lexer::lex;
use xz_cli::parser::parse;
use xz_cli::resolve::resolve;
use xz_cli::typecheck::typecheck;

/// Compile and execute a whole program. Returns Err if any phase fails.
fn exec(src: &str) -> Result<(), String> {
    let tokens = lex(src.to_string(), "test.xz".to_string()).map_err(|e| e.message)?;
    let program = parse(tokens).map_err(|e| e.message)?;
    resolve(&program).map_err(|_| "resolve failed".to_string())?;
    typecheck(&program).map_err(|e| format!("typecheck: {} errors", e.len()))?;
    check_intent(&program).map_err(|e| e[0].code.clone())?;
    let backend = compile(&program)?;
    run(backend.module)?;
    Ok(())
}

fn expect_exec(src: &str, label: &str) {
    match exec(src) {
        Ok(()) => {}
        Err(e) => println!("FAIL {}: {}", label, e),
    }
}

#[test]
fn hello_prints_and_returns() {
    expect_exec(
        r#"/// Prints a greeting.
/// @intent  Writes "Hello, Xz!" and returns its length.
/// @ensures result == 10
/// @effects io
func greet() -> Int
    post result == 10
{
    let msg: Str = "Hello, Xz!"
    print(msg)
    msg.len()
}

func main() {
    let n = greet()
    print("length: " + n.to_str())
}"#,
        "hello",
    );
}

#[test]
fn arithmetic_and_contracts_run() {
    expect_exec(
        r#"/// Adds two numbers.
/// @intent  Returns a + b.
/// @effects none
func add(a: Float, b: Float) -> Float {
    a + b
}

func main() {
    print(add(2.5, 3.25).to_str())
    print("\n")
}"#,
        "arithmetic",
    );
}

#[test]
fn if_elif_else_runs() {
    expect_exec(
        r#"/// Classifies an integer by sign.
/// @intent  Returns "neg", "pos", or "zero".
/// @effects none
func classify(x: Int) -> Str {
    if x < 0 {
        "neg"
    } elif x > 0 {
        "pos"
    } else {
        "zero"
    }
}

func main() {
    print(classify(-1))
    print(classify(0))
    print(classify(3))
}"#,
        "elif",
    );
}

#[test]
fn option_narrowing_runs() {
    expect_exec(
        r#"/// Reads an optional integer, 42 when present, 0 otherwise.
/// @intent  Unwraps the option.
/// @effects none
func read(o: Option[Int]) -> Int {
    if o is some {
        o
    } else {
        0
    }
}

func main() {
    let a: Option[Int] = some(42)
    let b: Option[Int] = none
    print(read(a).to_str())
    print(read(b).to_str())
}"#,
        "option narrow",
    );
}

#[test]
fn result_match_runs() {
    expect_exec(
        r#"/// Maps a result to a label.
/// @intent  Returns "ok:<v>" or "err".
/// @effects none
func label(r: Result[Int, Err]) -> Str {
    match r {
        ok(v) -> "ok:" + v.to_str()
        err(_) -> "err"
    }
}

func main() {
    let a: Result[Int, Err] = ok(7)
    let b: Result[Int, Err] = err(DomainError("bad"))
    print(label(a))
    print(label(b))
}"#,
        "result match",
    );
}

#[test]
fn question_mark_early_return_runs() {
    expect_exec(
        r#"/// Always fails.
/// @intent  Returns an error.
/// @effects none
func fail() -> Result[Int, Err] {
    err(DomainError("boom"))
}

func main() -> Result[Unit, Err] {
    let v = fail()?
    print(v.to_str())
    ok()
}"#,
        "? early return",
    );
}

#[test]
fn record_and_enum_runs() {
    expect_exec(
        r#"enum Shape {
    circle(radius: Float)
    rect(width: Float, height: Float)
}

/// Computes the area of a shape.
/// @intent  Returns the area of a circle or rectangle.
/// @effects none
func area(shape: Shape) -> Float {
    match shape {
        circle(r) -> 3.14159 * r * r
        rect(w, h) -> w * h
    }
}

func main() {
    print(area(circle(2.0)).to_str())
    print(" ")
    print(area(rect(3.0, 4.0)).to_str())
}"#,
        "record/enum",
    );
}

#[test]
fn scalar_to_str_runs() {
    expect_exec(
        r#"func main() {
    let i = -7
    print(i.to_str())
    print(" ")
    let f = 5.5
    print(f.to_str())
    print(" ")
    let c = 'A'
    print(c.to_str())
    print(" ")
    print(true.to_str())
    print(" ")
    print(false.to_str())
}"#,
        "to_str",
    );
}

#[test]
fn str_reclamation_runs_without_use_after_free() {
    // Exercise the conservative Str reclamation rules: temp-through-print
    // frees, alias downgrades, overwrite of an owned binding, `?` early
    // return with a live buffer, and a Str-returning function (leak-safe).
    // The runtime registry turns a mistaken double-free into a no-op, so the
    // main failure mode (use-after-free) would crash the process.
    expect_exec(
        r#"/// Adds a marker around a value.
/// @intent  Returns "[n]".
/// @effects none
func labeled(n: Int) -> Str {
    "[" + n.to_str() + "]"
}

/// Always fails.
/// @intent  Returns an error.
/// @effects none
func fail() -> Result[Int, Err] {
    err(DomainError("boom"))
}

func main() -> Result[Unit, Err] {
    let a = "x" + "y"
    let b = a
    print(a)
    print(" ")
    a = "z"
    print(a)
    print(" ")
    print(b)
    print(" ")
    print(labeled(7))
    print(" ")
    let v = fail()?
    print(v.to_str())
    ok()
}"#,
        "str reclamation",
    );
}

#[test]
fn optimization_pipeline_inlines_and_dces() -> Result<(), String> {
    // The JIT path runs `llvm_backend::optimize` (default<O3>) before codegen.
    // After the pass pipeline the trivial `sq` helper must be inlined away (its
    // call site gone) and the result constant-folded — proving the backend no
    // longer executes raw `OptimizationLevel::None` IR.
    let src = r#"/// Squares a number.
/// @intent  Returns x * x.
/// @effects none
func sq(x: Int) -> Int {
    x * x
}

func main() {
    print(sq(7).to_str())
    print("\n")
}"#;
    let tokens = lex(src.to_string(), "opt.xz".to_string()).map_err(|e| e.message)?;
    let program = parse(tokens).map_err(|e| e.message)?;
    resolve(&program).map_err(|_| "resolve failed".to_string())?;
    typecheck(&program).map_err(|e| format!("typecheck: {} errors", e.len()))?;
    check_intent(&program).map_err(|e| e[0].code.clone())?;
    let backend = compile(&program)?;
    let ir_before = backend.module.print_to_string().to_string();
    assert!(ir_before.contains("call i64 @sq"), "test program must call sq before passes");

    let _ = xz_cli::backend::llvm_backend::optimize(&backend.module)?;
    let ir_after = backend.module.print_to_string().to_string();
    assert!(!ir_after.contains("call i64 @sq"), "sq call site must be inlined away by O3");
    assert!(!ir_after.contains("define i64 @sq"), "sq body must be DCE'd after inlining");
    assert!(ir_after.contains("xz_i64_to_str(i64 49)"), "sq(7) must be constant-folded to 49");
    let _ = run(backend.module)?;
    Ok(())
}