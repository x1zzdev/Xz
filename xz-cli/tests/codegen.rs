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

#[test]
fn abs_and_sqrt_intrinsics_run() {
    // abs() and approx_sqrt() lower to llvm.abs / llvm.fabs / llvm.sqrt
    // intrinsics (native, inlineable) instead of host C ABI calls. This just
    // verifies the lowering produces correct results.
    expect_exec(
        r#"func main() {
    let i = -7
    print(i.abs().to_str())
    print(" ")
    let f = -2.5
    print(f.abs().to_str())
    print(" ")
    let g = approx_sqrt(9.0)
    print(g.to_str())
    print("\n")
}"#,
        "abs/sqrt intrinsics",
    );
}

#[test]
fn ffi_null_check_and_transfer_run() {
    // Phase 5 FFI path: extern C calls, Ptr null checks (`p == 0`), and
    // `transfer` (identity value move) must compile and execute. The JIT
    // resolves `malloc`/`free` from the process (libc).
    expect_exec(
        r#"extern func malloc(size: usize) -> Ptr
extern func free(ptr: Ptr)

record Buffer {
    ptr: Ptr
    size: Int
}

/// Allocates a buffer; ok on success, err on null.
/// @intent  Allocates size bytes and returns the buffer.
/// @ensures result is ok implies result.value.ptr != 0
/// @effects extern
func alloc_buffer(size: Int) -> Result[Buffer, Err]
    post result is ok implies result.value.ptr != 0
{
    let p = malloc(size as usize)
    if p == 0 {
        err(DomainError("oom"))
    } else {
        ok(Buffer(p, size))
    }
}

/// Releases the buffer; consumes the handle.
/// @intent  Frees the allocation.
/// @effects extern
func release_buffer(buf: Buffer) {
    free(buf.ptr)
}

func main() -> Result[Unit, Err] {
    let buf = alloc_buffer(16)?
    print("capacity: " + buf.size.to_str())
    release_buffer(transfer(buf))
    ok()
}"#,
        "ffi null check/transfer",
    );
}

#[test]
fn list_literal_index_and_iteration_run() {
    // List[T]: literal construction, bounds-checked indexing returning
    // Result[T, IndexError], non-mutating append, len/is_empty, and
    // for-in iteration over elements.
    expect_exec(
        r#"func main() -> Result[Unit, Err] {
    let xs: List[Int] = [10, 20, 30]
    print(xs.len().to_str())
    print(" ")
    let first = xs[0]?
    print(first.to_str())
    print(" ")
    let grown = xs.append(40)
    print(grown.len().to_str())
    print(" ")
    mut sum: Int = 0
    for x in grown {
        sum = sum + x
    }
    print(sum.to_str())
    print(" ")
    let bad = xs[9]
    if bad is ok {
        print("unexpected")
    } else {
        print("bounds")
    }
    print("\n")
    ok()
}"#,
        "list",
    );
}

#[test]
fn mutable_record_field_assignment_runs() {
    // Field assignment (p.x = ...) mutates only the target binding; a copy is
    // independent (value semantics, docs/04).
    expect_exec(
        r#"record Point {
    x: Int
    y: Int
}

func main() {
    let a = Point(1, 2)
    mut b = a
    b.x = 99
    b.y = b.y + 5
    print(a.x.to_str())
    print(" ")
    print(b.x.to_str())
    print(" ")
    print(b.y.to_str())
    print("\n")
}"#,
        "field assignment",
    );
}

#[test]
fn generic_functions_monomorphize_and_run() {
    // Generic functions are monomorphized per concrete argument type at the
    // call site (a distinct LLVM function per type-argument tuple).
    expect_exec(
        r#"/// Identity.
/// @intent  Returns its argument.
/// @effects none
func id[T](x: T) -> T {
    x
}

/// Returns the larger value.
/// @intent  Returns the maximum of a and b.
/// @effects none
func max[T: Ordered](a: T, b: T) -> T {
    if a > b {
        a
    } else {
        b
    }
}

func main() {
    print(id(42).to_str())
    print(" ")
    print(id("hi"))
    print(" ")
    print(max(3, 7).to_str())
    print(" ")
    print(max(2.5, 1.5).to_str())
    print("\n")
}"#,
        "generics",
    );
}

#[test]
fn native_runtime_emits_valid_module() -> Result<(), String> {
    // The native build path emits IR bodies for the xz_* runtime (libc-based)
    // and declares `main` returning i32. Verify the resulting module (this
    // catches malformed runtime IR without needing llc/ld, which may be absent
    // on some hosts).
    let src = r#"enum Shape {
    circle(radius: Float)
    rect(width: Float, height: Float)
}

/// Computes the area.
/// @intent  Returns the area of a shape.
/// @effects none
func area(shape: Shape) -> Float {
    match shape {
        circle(r) -> 3.14159 * r * r
        rect(w, h) -> w * h
    }
}

func main() -> Result[Unit, Err] {
    let msg = "area: " + area(circle(2.0)).to_str()
    print(msg)
    print("\n")
    ok()
}"#;
    let tokens = lex(src.to_string(), "native.xz".to_string()).map_err(|e| e.message)?;
    let program = parse(tokens).map_err(|e| e.message)?;
    resolve(&program).map_err(|_| "resolve failed".to_string())?;
    typecheck(&program).map_err(|e| format!("typecheck: {} errors", e.len()))?;
    check_intent(&program).map_err(|e| e[0].code.clone())?;
    let mut backend = compile(&program)?;
    xz_cli::backend::llvm_backend::emit_native_runtime(&mut backend)?;
    backend.module.verify().map_err(|e| format!("verify: {:?}", e))?;
    let ir = backend.module.print_to_string().to_string();
    assert!(ir.contains("define i32 @main()"), "native main must return i32");
    assert!(ir.contains("define") && ir.contains("@xz_print"), "runtime bodies must be defined");
    Ok(())
}

#[test]
fn loop_and_for_run() {
    // loop/for with break/continue must compile and execute. `for i in n`
    // iterates the Int range 0..n (exclusive).
    expect_exec(
        r#"func main() {
    let total = 0
    for i in 10 {
        if i == 3 {
            continue
        }
        if i == 6 {
            break
        }
        total = total + i
    }
    print(total.to_str())
    print(" ")

    let n = 0
    loop {
        n = n + 1
        if n == 5 {
            break
        }
    }
    print(n.to_str())
    print("\n")
}"#,
        "loop/for",
    );
}
