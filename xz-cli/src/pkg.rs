//! `xz pkg` — package/interface tooling. The first slice is
//! `xz pkg gen --lang python`: a `ctypes` wrapper generated from a `.xzint`
//! interface file (docs/10-ffi-interop.md).
use crate::ast::{Item, Program};
use crate::lexer::lex;
use crate::parser::parse;
use crate::resolve::resolve;
use crate::typecheck::typecheck;

/// A `.xzint` interface file is a declaration-only Xz source (docs/10): only
/// `extern func` signatures and `@cstruct record` declarations are allowed, so
/// the generated wrapper stays a faithful, complete description of a C ABI.
pub fn validate_interface(program: &Program) -> Result<(), String> {
    for item in &program.items {
        let (kind, name) = match item {
            Item::Extern(_) => continue,
            Item::Record(r) if r.cstruct => continue,
            Item::Func(f) => ("func", f.name.as_str()),
            Item::Task(t) => ("task", t.name.as_str()),
            Item::Chan(c) => ("chan", c.name.as_str()),
            Item::Record(r) => ("record", r.name.as_str()),
            Item::Enum(e) => ("enum", e.name.as_str()),
        };
        return Err(format!(
            "'.xzint' interface files may only declare 'extern func' and '@cstruct record'; found {} '{}'",
            kind, name
        ));
    }
    Ok(())
}

/// Validate an interface file and generate its Python `ctypes` wrapper, bound
/// to the C library `lib` (docs/10-ffi-interop.md).
pub fn generate_python(program: &Program, lib: &str) -> Result<String, String> {
    validate_interface(program)?;
    // The ctypes wrapper copies `Str`/`Bytes` into Python values, so it cannot
    // honor a `transfer` parameter; reject rather than degrade silently
    // (docs/10-ffi-interop.md).
    for item in &program.items {
        if let Item::Extern(e) = item {
            for p in &e.params {
                if p.transfer {
                    return Err(format!(
                        "extern func '{}' parameter '{}': 'transfer' is not supported by the generated Python wrapper; it copies the buffer and cannot hand off ownership",
                        e.name, p.name
                    ));
                }
            }
            if e.transfer_ret {
                return Err(format!(
                    "extern func '{}' return: 'transfer' is not supported by the generated Python wrapper; it copies the returned buffer and cannot take ownership",
                    e.name
                ));
            }
        }
    }
    Ok(crate::backend::python::generate_python_interface_bindings(
        program, lib,
    ))
}

/// The registry base URL for `xz pkg add`: `--registry` wins, then
/// `XZ_REGISTRY`; with neither set the command fails rather than guessing a
/// host (docs/10-ffi-interop.md).
pub fn registry_base(explicit: Option<&str>) -> Result<String, String> {
    if let Some(base) = explicit.filter(|b| !b.trim().is_empty()) {
        return Ok(base.trim_end_matches('/').to_string());
    }
    match std::env::var("XZ_REGISTRY") {
        Ok(base) if !base.trim().is_empty() => Ok(base.trim_end_matches('/').to_string()),
        _ => Err("no registry configured: pass --registry <url> or set XZ_REGISTRY".to_string()),
    }
}

/// The URL of interface `name` in `base`: `<base>/<name>.xzint`.
pub fn interface_url(base: &str, name: &str) -> String {
    format!("{}/{}.xzint", base.trim_end_matches('/'), name)
}

/// The file `xz pkg add` writes into the current directory.
pub fn interface_file(name: &str) -> String {
    format!("{}.xzint", name)
}

/// `xz pkg add` names must be plain identifiers (letters, digits, `.`, `_`,
/// `-`, not starting with `.` or `-`), so a name can neither escape the
/// registry path nor be read as a command-line flag.
pub fn validate_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let first_ok = matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric() || c == '_');
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if first_ok && rest_ok {
        Ok(())
    } else {
        Err(format!(
            "invalid interface name '{}': use letters, digits, '.', '_', '-' (not starting with '.' or '-')",
            name
        ))
    }
}

/// Lex, parse, and semantically verify `.xzint` interface source. This is the
/// same pipeline `xz pkg gen` runs; `pkg add` applies it to fetched text before
/// writing, so a malformed or hostile interface never lands (docs/10).
pub fn verify_interface_source(source: &str, path: &str) -> Result<Program, String> {
    let tokens = lex(source.to_string(), path.to_string()).map_err(|e| {
        format!(
            "{} at {}:{}:{}",
            e.message, e.span.file, e.span.start.0, e.span.start.1
        )
    })?;
    let program = parse(tokens).map_err(|e| {
        format!(
            "{} at {}:{}:{}",
            e.message, e.span.file, e.span.start.0, e.span.start.1
        )
    })?;
    validate_interface(&program)?;
    if let Err(errors) = resolve(&program) {
        return Err(format!(
            "{} resolution error(s): {}",
            errors.len(),
            errors[0].message
        ));
    }
    if let Err(errors) = typecheck(&program) {
        return Err(format!(
            "{} type error(s): {}",
            errors.len(),
            errors[0].message
        ));
    }
    Ok(program)
}

/// Fetch an interface over the host `curl` (falling back to `wget` when curl
/// is absent); no HTTP client is linked into the compiler (docs/10).
pub fn fetch_interface(url: &str) -> Result<String, String> {
    if let Some(result) = run_fetcher("curl", &["-fsSL", "--", url]) {
        return result;
    }
    if let Some(result) = run_fetcher("wget", &["-qO-", url]) {
        return result;
    }
    Err(format!(
        "no fetcher available: install 'curl' or 'wget' to fetch {}",
        url
    ))
}

/// Run one fetcher. `None` means the program is not installed (try the next);
/// `Some(Ok)` is the body, `Some(Err)` is a fetch that ran and failed.
fn run_fetcher(program: &str, args: &[&str]) -> Option<Result<String, String>> {
    match std::process::Command::new(program).args(args).output() {
        Err(_) => None,
        Ok(out) if out.status.success() => Some(
            String::from_utf8(out.stdout)
                .map_err(|_| "the fetched interface is not valid UTF-8".to_string()),
        ),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let detail = stderr.trim();
            let message = if detail.is_empty() {
                format!("{} failed to fetch", program)
            } else {
                format!("{} failed to fetch: {}", program, detail)
            };
            Some(Err(message))
        }
    }
}
