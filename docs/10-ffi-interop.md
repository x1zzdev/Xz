# FFI & Interoperability — Ecosystem Survival Plan

## Problem

New languages die from empty ecosystems. Xz's strategy is **interop-first**: from day one, existing C, Rust, and Python libraries must be usable, so the language ships with a de-facto ecosystem instead of waiting for one to grow.

## Strategy

1. **C ABI is the bridge.** C is the lingua franca of system libraries; Rust and Python both interop with C.
2. **Safe wrappers over raw FFI.** Raw FFI is an escape hatch (like `unsafe`), always confined to thin wrapper functions with contracts.
3. **Generated bindings for Python.** Xz compiles to shared libraries with machine-generated Python wrappers, and vice versa.

## Syntax

```
// Raw C ABI declaration — an escape hatch, use sparingly
extern func malloc(size: usize) -> Ptr
extern func free(ptr: Ptr)

// Typed, contracted wrapper — the sanctioned way to use FFI
/// Allocates a buffer; every raw FFI call sits behind a contracted wrapper.
/// @intent  Allocates size bytes; ok(Buffer) on success, err on null.
/// @ensures result is ok implies result.value.ptr != 0
/// @effects extern
func alloc_buffer(size: Int) -> Result[Buffer, AllocError]
    pre  size > 0
    post result is ok implies result.value.ptr != 0
{
    let p = malloc(size as usize)
    if p == 0 { err(AllocError()) } else { ok(Buffer(p, size)) }
}
```

## Type mapping (Xz ↔ C)

| Xz | C |
|---|---|
| `Bool` | `bool` (i8) |
| `Int` | `int64_t` |
| `Float` | `double` |
| `Char` | `char` |
| `Str` | `(ptr: char*, len: usize)` struct |
| `Bytes` | `(ptr: uint8*, len: usize)` struct |
| `Ptr` | `void*` (opaque, only in wrappers) |
| `record` | `struct` (memory-layout option `@cstruct`) |

## Interop matrix

| Source | Direction | Mechanism |
|---|---|---|
| Xz → C library | call `extern` | direct C ABI |
| Xz → Rust library | via C ABI | Rust exposes `#[no_mangle] extern "C"` |
| Xz → Python library | via C ABI | use CPython C-API / C extension shim |
| Python → Xz module | `xz bind --lang python` | auto-generates `ctypes` wrappers from a `.xz` interface |
| C → Xz library | `xz build --shared` | emits `libXz.so` + `.h` with exported functions |

## Bindings workflow

```
xz pkg add libcurl            # fetch + verify an interface definition
xz pkg gen --lang python     # generate ctypes wrappers from .xzint interface files
```

- Interface files (`.xzint`) are pure declarations: `extern` signatures + contracts. They are written once per library and shared.
- Every FFI wrapper carries contracts, so unsafe calls stay behind verified boundaries — preserving the "reviewable" promise even at the edge.

## Python bridge (first-class)

- `xz build --shared --bind python` produces a `.so` plus a `.py` wrapper module with proper types (Xz `Int` → Python `int`, `Result` → exceptions or `None`/tuples).
- Xz code can call Python libraries through a generated CPython shim, mapping Xz `Result` to Python exceptions.

## Priority

This is a **Phase 1–2 concern**, not a "later" concern. A language without interop has no reason to exist for real users; interop is the on-ramp.