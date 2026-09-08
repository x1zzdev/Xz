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

record Buffer {
    ptr: Ptr
    size: Int
}

// Typed, contracted wrapper — the sanctioned way to use FFI.
// Buffer is a handle type: never copied, handed off with transfer.
/// Allocates size bytes; ok(Buffer) on success, err on null.
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

/// Releases a buffer exactly once; consumes the handle.
/// @intent  Frees the underlying allocation; ownership moves to this wrapper.
/// @effects extern
func release_buffer(buf: Buffer) {
    free(buf.ptr)
}

func main() -> Result[Unit, Err] {
    let buf = alloc_buffer(16)?
    print("capacity: " + buf.size.to_str())  // reading a value field: fine
    release_buffer(transfer(buf))            // ownership moves; buf is dead
    ok()
}
```

## Handles: the missing link

Without handle semantics, the safety story has a hole: `Buffer` is a plain
`record`, so `let b = a` would copy the `Ptr`, and two value-bindings would
share one allocation — a double `free`. Handles close it:

```
let a = alloc_buffer(16)?     // a: Buffer handle
let b = a                     // ERROR: handles are never copied
release_buffer(transfer(a))   // ok; a is dead
release_buffer(transfer(a))   // ERROR: a was already transferred
```

- Pure code (no `@effects extern`) can hold a handle and read its value
  fields, but can only hand it on with `transfer`.
- Raw pointers never leak into value code, so the FFI escape hatch stays
  confined to thin, contracted wrappers.
- The full rules live in [03-type-system.md](03-type-system.md); the type
  mapping below shows where handles begin.

## Type mapping (Xz ↔ C)

| Xz | C |
|---|---|
| `Bool` | `bool` (i8) |
| `Int` | `int64_t` |
| `Float` | `double` |
| `Char` | `char` |
| `Str` | `(ptr: char*, len: usize)` struct |
| `Bytes` | `(ptr: uint8*, len: usize)` struct |
| `Ptr` | `void*` — a handle, never copied; only in wrappers |
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