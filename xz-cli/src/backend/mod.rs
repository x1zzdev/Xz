pub mod codegen;
pub mod header;
pub mod llvm_backend;
pub mod python;
pub mod runtime;

use std::path::PathBuf;

/// Resolve the shared-library and C-header output paths for
/// `xz build --shared`. `out` names the shared object (`--out`); the header is
/// written beside it with the same stem and a `.h` extension. Without `out`,
/// both keep the historical `libXz.so`/`libXz.h` names.
pub fn shared_output_paths(out: Option<&str>) -> (PathBuf, PathBuf) {
    match out {
        Some(path) => {
            let lib = PathBuf::from(path);
            let header = lib.with_extension("h");
            (lib, header)
        }
        None => (PathBuf::from("libXz.so"), PathBuf::from("libXz.h")),
    }
}

/// Resolve the Python wrapper path for `xz build --shared --bind python` and
/// `xz bind --lang python`. The wrapper is named after the source file's stem
/// (`foo.xz` -> `foo.py`) and written in the current directory. It must not
/// reuse the shared object's name: a `.py` module named `libXz` would be
/// shadowed by `libXz.so`, which Python treats as an extension module
/// (docs/10-ffi-interop.md).
pub fn python_wrapper_path(source: &str) -> PathBuf {
    let stem = std::path::Path::new(source)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "xz_bindings".to_string());
    PathBuf::from(format!("{stem}.py"))
}
