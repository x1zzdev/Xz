//! `xz pkg` — package/interface tooling. The first slice is
//! `xz pkg gen --lang python`: a `ctypes` wrapper generated from a `.xzint`
//! interface file (docs/10-ffi-interop.md).
use crate::ast::{Item, Program};

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
    Ok(crate::backend::python::generate_python_interface_bindings(
        program, lib,
    ))
}
