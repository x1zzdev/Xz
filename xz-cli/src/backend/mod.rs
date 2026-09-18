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
