// Phase 4 backend tests: compile a program through the full pipeline and
// execute it in the JIT engine. These catch codegen regressions (verifier
// failures, wrong lowering) that the front-end-only suite in check.rs cannot.
//
// Run with `cargo test` from xz-cli/ (LLVM env is pinned in .cargo/config.toml).

use xz_cli::backend::llvm_backend::compile;
use xz_cli::backend::llvm_backend::compile_shared;
use xz_cli::backend::llvm_backend::emit_native_runtime;
use xz_cli::backend::llvm_backend::hide_runtime_symbols;
use xz_cli::backend::header::generate_c_header;
use xz_cli::backend::python::generate_python_bindings;
use xz_cli::backend::python_wrapper_path;
use xz_cli::backend::runtime::run;
use xz_cli::backend::runtime::run_capturing;
use xz_cli::backend::shared_output_paths;
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

/// Compile and execute a whole program, returning its captured stdout.
fn exec_capture(src: &str) -> Result<String, String> {
    let tokens = lex(src.to_string(), "test.xz".to_string()).map_err(|e| e.message)?;
    let program = parse(tokens).map_err(|e| e.message)?;
    resolve(&program).map_err(|_| "resolve failed".to_string())?;
    typecheck(&program).map_err(|e| format!("typecheck: {} errors", e.len()))?;
    check_intent(&program).map_err(|e| e[0].code.clone())?;
    let backend = compile(&program)?;
    let (_code, out) = run_capturing(backend.module)?;
    Ok(out)
}

/// Assert the program's stdout byte-for-byte. This is what pins down the
/// deterministic output order of channels, Map, and Set (docs/05, 12).
fn expect_output(src: &str, expected: &str, label: &str) {
    match exec_capture(src) {
        Ok(out) if out == expected => {}
        Ok(out) => println!("FAIL {}: expected {:?}, got {:?}", label, expected, out),
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
    mut a = "x" + "y"
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
fn map_literal_get_insert_and_iteration_run() {
    // Map[K, V]: literal construction, Option lookup, non-mutating insert
    // (replace preserves position), len/is_empty, and insertion-order keys/values.
    expect_output(
        r#"func main() -> Result[Unit, Err] {
    let counts: Map[Str, Int] = {"a": 1, "b": 2, "a": 3}
    print(counts.len().to_str())
    print(" ")
    let grown = counts.insert("c", 4)
    let o = grown.get("a")
    if o is some {
        print(o.to_str())
    } else {
        print("none")
    }
    print(" ")
    let missing = grown.get("z")
    if missing is none {
        print("absent")
    } else {
        print("unexpected")
    }
    print(" ")
    for k in grown.keys() {
        print(k)
    }
    print(" ")
    for v in grown.values() {
        print(v.to_str())
    }
    print("\n")
    ok()
}"#,
        "2 3 absent abc 324\n",
        "map literal, get, insert, iteration",
    );
}

#[test]
fn string_methods_run() {
    // Str.to_upper / to_lower (new heap buffer), Str.at (bounds-checked
    // Result[Char, IndexError]), and Str.to_bytes (layout identity).
    expect_exec(
        r#"func main() -> Result[Unit, Err] {
    print("hello".to_upper())
    print(" ")
    print("WORLD".to_lower())
    print(" ")
    let c = "abc".at(1)?
    print(c.to_str())
    print(" ")
    let bad = "abc".at(99)
    if bad is ok {
        print("unexpected")
    } else {
        print("bounds")
    }
    print("\n")
    ok()
}"#,
        "string methods",
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
fn mutable_param_copy_in_copy_out_runs() {
    // A `mut` parameter is copy-in/copy-out: the callee mutates its own copy
    // and the caller's variable is updated at return. Passing the same
    // variable to two `mut` parameters stays alias-free: each gets an
    // independent copy, copied back in parameter order (docs/04, docs/13).
    expect_output(
        r#"func add_to(mut acc: Int, n: Int) {
    acc += n
}

func bump(mut a: Int, mut b: Int) {
    a = a + 1
    b = b + 10
}

func main() {
    mut x: Int = 10
    add_to(x, 5)
    print(x.to_str())
    print(" ")
    mut y: Int = 1
    bump(y, y)
    print(y.to_str())
    print("\n")
}"#,
        "15 11\n",
        "mut parameter copy-in/copy-out",
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
fn shared_export_linkage_and_header() -> Result<(), String> {
    // `@export` functions keep external linkage (so they survive globaldce
    // into the .so symbol table); everything else, including `main`, stays
    // internal. The generated header mirrors the exported C ABI.
    let src = r#"@cstruct record Inner {
    a: Int
    b: Int
}

@cstruct record Outer {
    inner: Inner
    flag: Bool
}

/// Adds two numbers.
/// @intent  Returns a + b.
/// @effects none
@export func add(a: Int, b: Int) -> Int {
    a + b
}

/// Kept private.
/// @intent  Returns a constant.
/// @effects none
func hidden() -> Int {
    7
}

func main() {
    print(add(1, 2).to_str())
}"#;
    let tokens = lex(src.to_string(), "shared.xz".to_string()).map_err(|e| e.message)?;
    let program = parse(tokens).map_err(|e| e.message)?;
    resolve(&program).map_err(|_| "resolve failed".to_string())?;
    typecheck(&program).map_err(|e| format!("typecheck: {} errors", e.len()))?;
    check_intent(&program).map_err(|e| e[0].code.clone())?;
    let backend = compile_shared(&program)?;
    let ir = backend.module.print_to_string().to_string();
    assert!(ir.contains("define i64 @add"), "exported add must stay external: {}", ir);
    assert!(!ir.contains("define internal i64 @add"), "exported add must not be internal");
    assert!(ir.contains("define internal i64 @hidden"), "private helper must be internal");
    assert!(ir.contains("define internal i32 @main"), "main must be internal in a shared build");

    let header = generate_c_header(&program);
    assert!(header.contains("int64_t add(int64_t a, int64_t b);"), "missing add prototype:\n{}", header);
    assert!(!header.contains("hidden"), "private helper must not be in the header");
    assert!(header.contains("typedef struct XzStr"), "missing XzStr");
    assert!(header.contains("bool flag;"), "Bool must map to C bool:\n{}", header);
    let inner_pos = header.find("typedef struct Inner").ok_or("Inner record missing")?;
    let outer_pos = header.find("typedef struct Outer").ok_or("Outer record missing")?;
    assert!(inner_pos < outer_pos, "nested record must be declared before its user:\n{}", header);
    Ok(())
}

#[test]
fn shared_hides_str_eq_runtime_symbol() -> Result<(), String> {
    // A shared library must not export the `xz_*` runtime. `xz_str_eq` backs
    // `Map[Str, _]`/`Set[Str]` key equality, so it must be internalized along
    // with the other runtime definitions.
    let src = r#"func has_key(m: Map[Str, Int], k: Str) -> Bool {
    let found = m.get(k)
    if found is some {
        true
    } else {
        false
    }
}

@export func add(a: Int, b: Int) -> Int {
    a + b
}"#;
    let tokens = lex(src.to_string(), "shared.xz".to_string()).map_err(|e| e.message)?;
    let program = parse(tokens).map_err(|e| e.message)?;
    resolve(&program).map_err(|_| "resolve failed".to_string())?;
    typecheck(&program).map_err(|e| format!("typecheck: {} errors", e.len()))?;
    let mut backend = compile_shared(&program)?;
    emit_native_runtime(&mut backend)?;
    hide_runtime_symbols(&backend);
    let ir = backend.module.print_to_string().to_string();
    assert!(
        ir.contains("define internal i1 @xz_str_eq"),
        "xz_str_eq must be internal in a shared build:\n{}",
        ir
    );
    Ok(())
}

#[test]
fn shared_output_paths_honor_out_flag() {
    // `xz build --shared` defaults to libXz.so/libXz.h; `--out <path>` names
    // the shared object and the header follows beside it.
    let (lib, header) = shared_output_paths(None);
    assert_eq!(lib.to_str(), Some("libXz.so"));
    assert_eq!(header.to_str(), Some("libXz.h"));

    let (lib, header) = shared_output_paths(Some("dist/libfoo.so"));
    assert_eq!(lib.to_str(), Some("dist/libfoo.so"));
    assert_eq!(header.to_str(), Some("dist/libfoo.h"));
}

#[test]
fn python_wrapper_path_matches_source_stem() {
    // `xz bind --lang python` and `xz build --shared --bind python` name the
    // wrapper after the source file's stem, in the current directory, so it
    // does not shadow the shared object as an extension module.
    assert_eq!(python_wrapper_path("foo.xz").to_str(), Some("foo.py"));
    assert_eq!(python_wrapper_path("dist/foo.xz").to_str(), Some("foo.py"));
    assert_eq!(python_wrapper_path("noext").to_str(), Some("noext.py"));
}

#[test]
fn python_bindings_mirror_exported_abi() -> Result<(), String> {
    // `xz bind --lang python` generates a ctypes module from the same
    // interface the C header describes: typed bindings for the `@export`
    // functions, `ctypes.Structure` classes for the `@cstruct` records, and
    // nothing for private functions.
    let src = r#"@cstruct record Inner {
    a: Int
    b: Int
}

@cstruct record Outer {
    inner: Inner
    flag: Bool
}

/// Adds two numbers.
/// @intent  Returns a + b.
/// @effects none
@export func add(a: Int, b: Int) -> Int {
    a + b
}

/// Kept private.
/// @intent  Returns a constant.
/// @effects none
func hidden() -> Int {
    7
}

func main() {
    print(add(1, 2).to_str())
}"#;
    let tokens = lex(src.to_string(), "shared.xz".to_string()).map_err(|e| e.message)?;
    let program = parse(tokens).map_err(|e| e.message)?;
    resolve(&program).map_err(|_| "resolve failed".to_string())?;
    typecheck(&program).map_err(|e| format!("typecheck: {} errors", e.len()))?;
    check_intent(&program).map_err(|e| e[0].code.clone())?;

    let bindings = generate_python_bindings(&program, "libXz.so");
    assert!(
        bindings.contains("_lib.add.argtypes = [ctypes.c_int64, ctypes.c_int64]"),
        "missing add argtypes:\n{}",
        bindings
    );
    assert!(bindings.contains("_lib.add.restype = ctypes.c_int64"), "missing add restype:\n{}", bindings);
    assert!(bindings.contains("add = _lib.add"), "missing add binding:\n{}", bindings);
    assert!(!bindings.contains("_lib.hidden"), "private helper must not be bound:\n{}", bindings);
    assert!(bindings.contains("class XzStr(ctypes.Structure):"), "missing XzStr:\n{}", bindings);
    assert!(bindings.contains("(\"flag\", ctypes.c_bool)"), "Bool must map to c_bool:\n{}", bindings);
    let inner_pos = bindings.find("class Inner(ctypes.Structure):").ok_or("Inner class missing")?;
    let outer_pos = bindings.find("class Outer(ctypes.Structure):").ok_or("Outer class missing")?;
    assert!(inner_pos < outer_pos, "nested record must be defined before its user:\n{}", bindings);
    Ok(())
}

#[test]
fn python_bindings_load_named_library() -> Result<(), String> {
    // `xz bind --lang python --lib <name>` loads that sibling shared object
    // instead of the default `libXz.so`, so the wrapper can follow the name
    // given to `xz build --shared --out` (docs/10-ffi-interop.md).
    let src = r#"/// Adds two numbers.
/// @intent  Returns a + b.
/// @effects none
@export func add(a: Int, b: Int) -> Int {
    a + b
}"#;
    let tokens = lex(src.to_string(), "shared.xz".to_string()).map_err(|e| e.message)?;
    let program = parse(tokens).map_err(|e| e.message)?;
    resolve(&program).map_err(|_| "resolve failed".to_string())?;
    typecheck(&program).map_err(|e| format!("typecheck: {} errors", e.len()))?;
    check_intent(&program).map_err(|e| e[0].code.clone())?;

    let bindings = generate_python_bindings(&program, "libfoo.so");
    assert!(
        bindings.contains(
            "_lib = ctypes.CDLL(os.path.join(os.path.dirname(os.path.abspath(__file__)), \"libfoo.so\"))"
        ),
        "wrapper must load the named sibling library:\n{}",
        bindings
    );
    assert!(
        !bindings.contains("libXz.so"),
        "the default library name must not leak when overridden:\n{}",
        bindings
    );
    Ok(())
}

#[test]
fn python_bindings_marshal_str_and_bytes() -> Result<(), String> {
    // `Str`/`Bytes` cross the C ABI as `XzStr`/`XzBytes`, but the generated
    // wrapper presents Python `str`/`bytes` (docs/10-ffi-interop.md). A
    // signature with only scalar types keeps the direct `_lib` alias, since
    // `ctypes` already returns the proper Python scalar for those.
    let src = r#"/// Echoes a name.
/// @intent  Returns the name with a bang.
/// @effects none
@export func greet(name: Str) -> Str {
    name + "!"
}

/// Returns the same bytes.
/// @intent  Returns data unchanged.
/// @effects none
@export func echo(data: Bytes) -> Bytes {
    data
}

/// Adds two numbers.
/// @intent  Returns a + b.
/// @effects none
@export func add(a: Int, b: Int) -> Int {
    a + b
}"#;
    let tokens = lex(src.to_string(), "shared.xz".to_string()).map_err(|e| e.message)?;
    let program = parse(tokens).map_err(|e| e.message)?;
    resolve(&program).map_err(|_| "resolve failed".to_string())?;
    typecheck(&program).map_err(|e| format!("typecheck: {} errors", e.len()))?;
    check_intent(&program).map_err(|e| e[0].code.clone())?;

    let bindings = generate_python_bindings(&program, "libXz.so");
    assert!(
        bindings.contains("def greet(name):"),
        "missing Str wrapper:\n{}",
        bindings
    );
    assert!(
        bindings.contains("_xz_name_data = name.encode(\"utf-8\")"),
        "missing Str encode:\n{}",
        bindings
    );
    assert!(
        bindings.contains("XzStr(ctypes.cast(_xz_name_buf, ctypes.c_void_p), len(_xz_name_data))"),
        "missing Str arg construction:\n{}",
        bindings
    );
    assert!(
        bindings.contains("return ctypes.string_at(_xz_ret.ptr, _xz_ret.len).decode(\"utf-8\")"),
        "missing Str decode:\n{}",
        bindings
    );
    assert!(
        bindings.contains("def echo(data):"),
        "missing Bytes wrapper:\n{}",
        bindings
    );
    assert!(
        bindings.contains(
            "XzBytes(ctypes.cast(_xz_data_buf, ctypes.POINTER(ctypes.c_uint8)), len(_xz_data_data))"
        ),
        "missing Bytes arg construction:\n{}",
        bindings
    );
    assert!(
        bindings.contains("return ctypes.string_at(_xz_ret.ptr, _xz_ret.len)\n"),
        "missing Bytes return:\n{}",
        bindings
    );
    assert!(
        bindings.contains("add = _lib.add"),
        "a scalar-only signature must keep the direct alias:\n{}",
        bindings
    );
    Ok(())
}

#[test]
fn record_bool_fields_round_trip() {
    // A `Bool` record field is stored as one byte (`i8`), matching C's `bool`
    // (docs/13-codegen.md). Construction zero-extends the value `i1` and a
    // field read narrows it back, including across a by-value function
    // argument and a `mut` field assignment. A `Char` field (also `i8`) must
    // not be narrowed by that rule.
    expect_output(
        r#"record Flags {
    on: Bool
    ch: Char
    n: Int
}

/// Builds a Flags.
/// @intent  Returns Flags(on, ch, n).
/// @effects none
func make(on: Bool, ch: Char, n: Int) -> Flags {
    Flags(on, ch, n)
}

/// Reads the flag.
/// @intent  Returns f.on.
/// @effects none
func flag_of(f: Flags) -> Bool {
    f.on
}

func main() {
    let f = make(true, 'A', 7)
    print(flag_of(f).to_str())
    print(" ")
    print(f.ch.to_str())
    print(" ")
    print(f.n.to_str())
    print("\n")
    let off = make(false, 'Z', 3)
    print(off.on.to_str())
    print(" ")
    print(off.ch.to_str())
    print("\n")
    mut g: Flags = Flags(false, 1, 2)
    g.on = true
    print(g.on.to_str())
    print("\n")
}"#,
        "true A 7\nfalse Z\ntrue\n",
        "record_bool_fields",
    );
}

#[test]
fn cstruct_bool_field_uses_c_memory_layout() -> Result<(), String> {
    // A `@cstruct` `Bool` field must be one byte in the LLVM struct, so the
    // struct's field order, padding, and ABI size match the C declaration the
    // generated header promises (docs/10-ffi-interop.md, docs/13-codegen.md).
    let src = r#"@cstruct record Header {
    tag: Char
    active: Bool
    count: Int
}

@cstruct record Pair {
    first: Bool
    second: Bool
}

/// Returns the count.
/// @intent  Returns h.count.
/// @effects none
@export func count(h: Header) -> Int {
    h.count
}

/// Returns the second flag.
/// @intent  Returns p.second.
/// @effects none
@export func second(p: Pair) -> Bool {
    p.second
}

func main() {
    print("x")
}"#;
    let tokens = lex(src.to_string(), "shared.xz".to_string()).map_err(|e| e.message)?;
    let program = parse(tokens).map_err(|e| e.message)?;
    resolve(&program).map_err(|_| "resolve failed".to_string())?;
    typecheck(&program).map_err(|e| format!("typecheck: {} errors", e.len()))?;
    check_intent(&program).map_err(|e| e[0].code.clone())?;
    let backend = compile_shared(&program)?;
    let ir = backend.module.print_to_string().to_string();
    assert!(ir.contains("%Header = type { i8, i8, i64 }"), "Bool @cstruct field must be one byte:\n{}", ir);
    assert!(ir.contains("%Pair = type { i8, i8 }"), "two Bool @cstruct fields must be two bytes:\n{}", ir);

    let td = backend.target_data.ok_or("host target data unavailable")?;
    let header_ty = backend.record_types.get("Header").copied().ok_or("Header type missing")?;
    // C layout: char(1) + bool(1) + pad(6) + int64(8) = 16.
    assert_eq!(td.get_abi_size(&header_ty), 16, "Header must match the C ABI layout");
    let pair_ty = backend.record_types.get("Pair").copied().ok_or("Pair type missing")?;
    assert_eq!(td.get_abi_size(&pair_ty), 2, "Pair must match the C ABI layout");
    Ok(())
}

#[test]
fn mut_param_maps_to_pointer_in_bindings() -> Result<(), String> {
    // A `mut` parameter is in/out and crosses as `T*` in the C header and
    // Python bindings (docs/04, docs/10, docs/13).
    let src = r#"/// Increments in place.
/// @intent  Adds one to x.
/// @effects mut
@export func inc(mut x: Int) {
    x = x + 1
}

func main() {
    mut n: Int = 0
    inc(n)
    print(n.to_str())
}"#;
    let tokens = lex(src.to_string(), "shared.xz".to_string()).map_err(|e| e.message)?;
    let program = parse(tokens).map_err(|e| e.message)?;
    resolve(&program).map_err(|_| "resolve failed".to_string())?;
    typecheck(&program).map_err(|e| format!("typecheck: {} errors", e.len()))?;
    check_intent(&program).map_err(|e| e[0].code.clone())?;

    let header = generate_c_header(&program);
    assert!(header.contains("void inc(int64_t* x);"), "mut param must map to a pointer:\n{}", header);
    let bindings = generate_python_bindings(&program, "libXz.so");
    assert!(
        bindings.contains("_lib.inc.argtypes = [ctypes.POINTER(ctypes.c_int64)]"),
        "mut param must map to a pointer:\n{}",
        bindings
    );
    Ok(())
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
    assert!(ir.contains("define i1 @xz_read_file"), "read_file runtime body must be defined:\n{}", ir);
    assert!(ir.contains("define double @xz_time_now"), "time_now runtime body must be defined:\n{}", ir);
    assert!(
        ir.contains("define double @xz_time_monotonic"),
        "time_monotonic runtime body must be defined:\n{}",
        ir
    );
    Ok(())
}

#[test]
fn loop_and_for_run() {
    // loop/for with break/continue must compile and execute. `for i in n`
    // iterates the Int range 0..n (exclusive).
    expect_exec(
        r#"func main() {
    mut total = 0
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

    mut n = 0
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

#[test]
fn async_await_lowering_runs() -> Result<(), String> {
    // Phase 6: `await f(args)` runs the async callee as a scheduled child
    // coroutine and suspends the caller until it completes (docs/05-concurrency.md
    // rule 6). The child is spawned via `xz_task_spawn_arg` and returns its
    // result on a synthetic completion channel, so await composes with `?`,
    // nested await, and interleaves with `task`s under the deterministic policy.
    let src = r#"chan ping: Chan[Int]

/// Adds one.
/// @intent  Returns x + 1.
/// @effects none
async func base(x: Int) -> Int {
    x + 1
}

/// Awaits base twice.
/// @intent  Returns base(x) * 2.
/// @effects none
async func nested(x: Int) -> Int {
    let a = await base(x)
    let b = await base(x)
    a + b
}

/// Triples or fails.
/// @intent  Returns 3x for x >= 0, otherwise an error.
/// @effects none
async func triple(x: Int) -> Result[Int, Err] {
    if x < 0 {
        err(Err("neg"))
    } else {
        ok(x * 3)
    }
}

/// Consumes one ping.
/// @intent  Prints the pinged value.
/// @effects io, chan
task worker {
    let v <- recv(ping)
    print("worker:" + v.to_str() + "\n")
}

func main() -> Result[Unit, Err] {
    send(ping, 1)
    let r = await nested(10)
    print("main:" + r.to_str() + "\n")
    let t = await triple(2)?
    print(t.to_str())
    print("\n")
    ok()
}"#;
    let tokens = lex(src.to_string(), "await.xz".to_string()).map_err(|e| e.message)?;
    let program = parse(tokens).map_err(|e| e.message)?;
    resolve(&program).map_err(|_| "resolve failed".to_string())?;
    typecheck(&program).map_err(|e| format!("typecheck: {} errors", e.len()))?;
    check_intent(&program).map_err(|e| e[0].code.clone())?;
    let backend = compile(&program)?;
    let ir = backend.module.print_to_string().to_string();
    assert!(ir.contains("call void @xz_task_spawn_arg"), "await must spawn the child:\n{}", ir);
    assert!(ir.contains("define internal void @__await_"), "await needs a trampoline:\n{}", ir);
    assert!(ir.contains("@xz_chan_send"), "the child must send its result:\n{}", ir);
    assert!(ir.contains("@xz_chan_recv"), "the caller must block on the result:\n{}", ir);
    let (_code, out) = run_capturing(backend.module)?;
    assert_eq!(out, "worker:1\nmain:22\n6\n", "await interleaving must be deterministic");
    Ok(())
}

#[test]
fn deterministic_tasks_and_channels_run() -> Result<(), String> {
    // Phase 6: `task` bodies are spawned by `main` in source order, and
    // `send`/`recv` move typed values through the deterministic scheduler
    // (docs/05-concurrency.md). The task ends via an ack, so no thread is left
    // parked after `main` returns.
    let src = r#"chan req: Chan[Int]
chan rep: Chan[Int]
chan ack: Chan[Int]

/// Doubles values until a sentinel arrives.
/// @intent  Receives Ints, sends twice each value, and acknowledges the end.
/// @effects chan
task doubler {
    loop {
        let x <- recv(req)
        if x < 0 {
            send(ack, 0)
            break
        }
        send(rep, x * 2)
    }
}

func main() -> Result[Unit, Err] {
    send(req, 21)
    let r <- recv(rep)
    print(r.to_str())
    print("\n")
    send(req, -1)
    let _done <- recv(ack)
    ok()
}"#;
    let tokens = lex(src.to_string(), "conc.xz".to_string()).map_err(|e| e.message)?;
    let program = parse(tokens).map_err(|e| e.message)?;
    resolve(&program).map_err(|_| "resolve failed".to_string())?;
    typecheck(&program).map_err(|e| format!("typecheck: {} errors", e.len()))?;
    check_intent(&program).map_err(|e| e[0].code.clone())?;
    let backend = compile(&program)?;
    let ir = backend.module.print_to_string().to_string();
    assert!(ir.contains("define internal void @doubler"), "task must be lowered:\n{}", ir);
    assert!(ir.contains("call void @xz_task_spawn"), "main must spawn tasks:\n{}", ir);
    assert!(ir.contains("@xz_chan_send"), "send must call the runtime:\n{}", ir);
    assert!(ir.contains("@xz_chan_recv"), "recv must call the runtime:\n{}", ir);
    let (_code, out) = run_capturing(backend.module)?;
    assert_eq!(out, "42\n", "channel output order/value must be deterministic");
    Ok(())
}

#[test]
fn set_literal_insert_contains_and_iteration_run() {
    // Set[T]: literal dedup (first position kept), non-mutating insert
    // (a present element is a no-op), contains hit/miss, len/is_empty, and
    // insertion-order iteration over elements and an empty set.
    expect_output(
        r#"func main() -> Result[Unit, Err] {
    let tags: Set[Str] = {"a", "b", "a"}
    print(tags.len().to_str())
    print(" ")
    let grown = tags.insert("c")
    print(grown.len().to_str())
    print(" ")
    let again = grown.insert("a")
    print(again.len().to_str())
    print(" ")
    if grown.contains("b") {
        print("has-b")
    } else {
        print("missing-b")
    }
    print(" ")
    if grown.contains("z") {
        print("has-z")
    } else {
        print("missing-z")
    }
    print(" ")
    for t in grown {
        print(t)
    }
    print(" ")
    let empty: Set[Int] = {}
    print(empty.len().to_str())
    print(" ")
    let one = empty.insert(7)
    print(one.contains(7).to_str())
    print("\n")
    ok()
}"#,
        "2 3 3 has-b missing-z abc 0 true\n",
        "set literal, insert, contains, iteration",
    );
}

#[test]
fn read_file_reads_and_missing_is_err() -> Result<(), String> {
    // `read_file` reads a real temp file through the JIT host (fresh heap Str
    // payload wrapped in Result[Str, Err]); a missing path yields err.
    let pid = std::process::id();
    let path = std::env::temp_dir().join(format!("xz_read_file_{}.txt", pid));
    let missing = std::env::temp_dir().join(format!("xz_read_file_{}_missing.txt", pid));
    std::fs::write(&path, b"hello\n").map_err(|e| e.to_string())?;
    let src = format!(
        r#"func main() -> Result[Unit, Err] {{
    let contents = read_file("{p}")?
    print(contents)
    print(contents.len().to_str())
    print(" ")
    let gone = read_file("{m}")
    if gone is err {{
        print("err")
    }} else {{
        print("unexpected")
    }}
    print("\n")
    ok()
}}"#,
        p = path.display(),
        m = missing.display()
    );
    let r = exec(&src);
    let _ = std::fs::remove_file(&path);
    r
}

#[test]
fn time_now_and_monotonic_run() {
    // Both clock host functions lower to `xz_time_now` / `xz_time_monotonic`
    // returning f64. The values are non-deterministic, so this asserts the
    // monotonic pair does not go backwards (the captured branch) and that
    // `now()` renders as a parseable float.
    let out = match exec_capture(
        r#"func main() {
    let before = monotonic()
    let wall = now()
    let after = monotonic()
    if after < before {
        print("backwards")
    } else {
        print("ok ")
    }
    print(wall.to_str())
    print("\n")
}"#,
    ) {
        Ok(out) => out,
        Err(e) => {
            println!("FAIL time now/monotonic: {}", e);
            return;
        }
    };
    let wall = match out.strip_prefix("ok ") {
        Some(rest) => rest.trim_end_matches('\n'),
        None => {
            println!("FAIL time now/monotonic: monotonic went backwards: {:?}", out);
            return;
        }
    };
    assert!(wall.parse::<f64>().is_ok(), "now() must render as a float, got {:?}", wall);
}

#[test]
fn time_example_elapsed_is_a_float() -> Result<(), String> {
    // examples/time.xz: `sum` is deterministic and now asserted exactly; the
    // elapsed/now readings are wall-clock dependent, so only their float shape
    // is asserted. Uses the capture sink so the run leaves no stray stdout.
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../examples/time.xz"))
        .map_err(|e| e.to_string())?;
    let out = exec_capture(&src)?;
    let mut lines = out.lines();
    assert_eq!(lines.next(), Some("sum: 499999500000"));
    let elapsed = lines.next().and_then(|l| l.strip_prefix("elapsed: ")).ok_or("missing elapsed line")?;
    elapsed.parse::<f64>().map_err(|_| format!("elapsed not a float: {elapsed:?}"))?;
    let now = lines.next().and_then(|l| l.strip_prefix("now: ")).ok_or("missing now line")?;
    now.parse::<f64>().map_err(|_| format!("now not a float: {now:?}"))?;
    Ok(())
}
