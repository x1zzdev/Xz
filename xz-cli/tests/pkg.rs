use xz_cli::ast::Program;
use xz_cli::lexer::lex;
use xz_cli::parser::parse;
use xz_cli::pkg;

fn parse_interface(src: &str) -> Program {
    let tokens = match lex(src.to_string(), "lib.xzint".to_string()) {
        Ok(t) => t,
        Err(e) => panic!("lex error: {}", e.message),
    };
    match parse(tokens) {
        Ok(p) => p,
        Err(e) => panic!("parse error: {}", e.message),
    }
}

#[test]
fn pkg_gen_binds_extern_interface() {
    // `xz pkg gen --lang python` transcribes a `.xzint` interface: every
    // `extern` is bound against the named C library (not the sibling
    // `libXz.so`), and every `@cstruct` becomes a `ctypes.Structure`.
    let src = r#"@interface foreign
extern func curl_easy_init() -> Ptr
extern func curl_easy_setopt(handle: Ptr, option: Int, param: Ptr) -> Int
extern func curl_easy_cleanup(handle: Ptr)

@cstruct record curl_slist {
    data: Str
    next: Ptr
}
"#;
    let program = parse_interface(src);
    let out = pkg::generate_python(&program, "libcurl.so").expect("generate");

    assert!(
        out.contains("ctypes.CDLL(\"libcurl.so\")"),
        "library must be loaded by name:\n{}",
        out
    );
    assert!(
        out.contains("_lib.curl_easy_init.argtypes = []"),
        "missing zero-arg argtypes:\n{}",
        out
    );
    assert!(
        out.contains("_lib.curl_easy_init.restype = ctypes.c_void_p"),
        "Ptr must map to c_void_p:\n{}",
        out
    );
    assert!(
        out.contains(
            "_lib.curl_easy_setopt.argtypes = [ctypes.c_void_p, ctypes.c_int64, ctypes.c_void_p]"
        ),
        "missing typed argtypes:\n{}",
        out
    );
    assert!(
        out.contains("curl_easy_cleanup = _lib.curl_easy_cleanup"),
        "missing binding:\n{}",
        out
    );
    assert!(
        out.contains("class curl_slist(ctypes.Structure):"),
        "missing cstruct class:\n{}",
        out
    );
    assert!(
        out.contains("(\"data\", XzStr)"),
        "Str field must map to XzStr:\n{}",
        out
    );
}

#[test]
fn pkg_gen_marshals_str_params() {
    // An `extern` that takes `Str` is wrapped to accept a Python `str` and
    // build the `XzStr` ABI struct (docs/10-ffi-interop.md).
    let src = r#"@interface foreign
extern func puts(s: Str) -> Int
extern func noop()
"#;
    let program = parse_interface(src);
    let out = pkg::generate_python(&program, "libc.so").expect("generate");

    assert!(
        out.contains("def puts(s):"),
        "missing Str wrapper:\n{}",
        out
    );
    assert!(
        out.contains("_xz_s_data = s.encode(\"utf-8\")"),
        "missing Str encode:\n{}",
        out
    );
    assert!(
        out.contains("XzStr(ctypes.cast(_xz_s_buf, ctypes.c_void_p), len(_xz_s_data))"),
        "missing Str arg construction:\n{}",
        out
    );
    assert!(
        out.contains("noop = _lib.noop"),
        "a scalar-only extern must keep the direct alias:\n{}",
        out
    );
}

#[test]
fn pkg_gen_escapes_library_name() {
    let program = parse_interface("@interface foreign\nextern func noop()\n");
    let out = pkg::generate_python(&program, "weird\"lib.so").expect("generate");
    assert!(
        out.contains("ctypes.CDLL(\"weird\\\"lib.so\")"),
        "library name must be escaped:\n{}",
        out
    );
}

#[test]
fn pkg_gen_rejects_function_bodies() {
    let program = parse_interface(
        r#"@interface foreign
extern func puts(s: Str) -> Int
func helper() -> Int {
    1
}
"#,
    );
    let err = pkg::generate_python(&program, "libc.so").unwrap_err();
    assert!(err.contains("func 'helper'"), "unexpected error: {}", err);
}

#[test]
fn pkg_gen_rejects_plain_record() {
    let program = parse_interface(
        r#"@interface foreign
record Buffer {
    ptr: Ptr
}
"#,
    );
    let err = pkg::generate_python(&program, "libx.so").unwrap_err();
    assert!(err.contains("record 'Buffer'"), "unexpected error: {}", err);
}

#[test]
fn pkg_gen_rejects_transfer_param() {
    // The ctypes wrapper copies Str/Bytes into Python values, so it cannot
    // honor a `transfer` parameter; it must reject rather than degrade it
    // silently (docs/10-ffi-interop.md).
    let program = parse_interface("@interface foreign\nextern func write(transfer frame: Bytes) -> Int\n");
    let err = pkg::generate_python(&program, "libx.so").unwrap_err();
    assert!(err.contains("'transfer'"), "unexpected error: {}", err);
    assert!(err.contains("frame"), "error should name the parameter: {}", err);
}

#[test]
fn pkg_gen_rejects_transfer_return() {
    // The ctypes wrapper copies the returned buffer into a Python value and
    // cannot take ownership of it, so a `transfer` return must be rejected
    // rather than leak the buffer (docs/10-ffi-interop.md).
    let program = parse_interface("@interface foreign\nextern func read(path: Str) -> transfer Str\n");
    let err = pkg::generate_python(&program, "libx.so").unwrap_err();
    assert!(err.contains("'transfer'"), "unexpected error: {}", err);
    assert!(err.contains("return"), "error should name the return: {}", err);
}

#[test]
fn pkg_gen_requires_an_interface_kind_marker() {
    let program = parse_interface("extern func noop()\n");
    let err = pkg::generate_python(&program, "libx.so").unwrap_err();
    assert!(
        err.contains("@interface export"),
        "unexpected error: {}",
        err
    );
}

#[test]
fn pkg_gen_rejects_transfer_on_an_export_interface() {
    // An `@interface export` describes an Xz `@export` surface, which cannot
    // accept ownership from its C caller (docs/10-ffi-interop.md).
    let program = parse_interface("@interface export\nextern func write(transfer frame: Bytes) -> Int\n");
    let err = pkg::generate_python(&program, "libx.so").unwrap_err();
    assert!(err.contains("frame"), "error should name the parameter: {}", err);
    assert!(err.contains("@export"), "unexpected error: {}", err);
}

#[test]
fn pkg_add_builds_registry_url() {
    assert_eq!(
        pkg::interface_url("https://xz.example/interfaces/", "libcurl"),
        "https://xz.example/interfaces/libcurl.xzint"
    );
    assert_eq!(pkg::interface_file("libcurl"), "libcurl.xzint");
    assert_eq!(
        pkg::registry_base(Some("https://xz.example/")).unwrap(),
        "https://xz.example"
    );
}

#[test]
fn pkg_add_rejects_unsafe_names() {
    assert!(pkg::validate_name("libcurl").is_ok());
    assert!(pkg::validate_name("libcurl-8").is_ok());
    assert!(pkg::validate_name("../evil").is_err());
    assert!(pkg::validate_name("a/b").is_err());
    assert!(pkg::validate_name("-rf").is_err());
    assert!(pkg::validate_name(".hidden").is_err());
    assert!(pkg::validate_name("").is_err());
}

#[test]
fn pkg_add_verifies_fetched_interface() {
    // A fetched interface is untrusted until it passes the same checks as
    // `xz pkg gen`: only `extern func` and `@cstruct record` survive.
    let src = "@interface foreign\nextern func puts(s: Str) -> Int\n";
    assert!(pkg::verify_interface_source(src, "libc.xzint").is_ok());
    let with_body = "@interface foreign\nextern func puts(s: Str) -> Int\nfunc helper() -> Int {\n    1\n}\n";
    let err = match pkg::verify_interface_source(with_body, "libc.xzint") {
        Ok(_) => panic!("a function body must be rejected"),
        Err(e) => e,
    };
    assert!(err.contains("'helper'"), "unexpected error: {}", err);

    let plain = "@interface foreign\nrecord Buffer {\n    ptr: Ptr\n}\n";
    let err = match pkg::verify_interface_source(plain, "libx.xzint") {
        Ok(_) => panic!("a plain record must be rejected"),
        Err(e) => e,
    };
    assert!(err.contains("'Buffer'"), "unexpected error: {}", err);
}
