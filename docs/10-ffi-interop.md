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

@cstruct record Buffer {
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
| `Bool` | `bool` — one byte (`i8`) in memory, `i1` in the register ABI ([13-codegen.md](13-codegen.md)) |
| `Int` | `int64_t` |
| `Float` | `double` |
| `Char` | `char` |
| `Str` | `(ptr: char*, len: usize)` struct |
| `Bytes` | `(ptr: uint8*, len: usize)` struct |
| `Ptr` | `void*` — a handle, never copied; only in wrappers |
| `record` | `struct` (memory-layout option `@cstruct`) |

### `@cstruct` records

A plain `record` is a language-level value type; its layout is unspecified and
it never crosses the FFI boundary. Prefixing the declaration with `@cstruct`
requests the **C ABI struct layout** — fields in declaration order with C
alignment and padding — and is the only record form that may be passed to or
returned from an `extern` function by value:

```
@cstruct record Color {
    r: usize
    g: usize
    b: usize
    a: usize
}
```

Because the layout is a promise to C, the field types must be
C-representable:

- a primitive — `Bool`, `Int`, `usize`, `Float`, `Char`, `Str`, `Bytes`, or
  `Ptr` (each maps per the table above), or
- another `@cstruct record` (nested by value), which must not form a cycle.

`Unit`, `Option`, `Result`, `List`, `Chan`, an `enum`, a plain `record`, and a
type parameter are rejected as `@cstruct` fields — each has a representation
that C does not know. The compiler enforces this so a reviewer never has to
audit a layout by hand.

A `@cstruct` record with a `Ptr` field is still a handle type (see
[03-type-system.md](03-type-system.md)): C layout governs how it is passed, not
whether it may be copied.

## Exporting an Xz library (`xz build --shared`)

`xz build --shared <file.xz>` emits `libXz.so` and a matching `libXz.h` for C
callers. Only functions marked `@export` become symbols; everything else keeps
internal linkage and stays private:

```
@export func add(a: Int, b: Int) -> Int {
    a + b
}
```

```c
// libXz.h (generated)
#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>

typedef struct XzStr { const char* ptr; size_t len; } XzStr;

int64_t add(int64_t a, int64_t b);
```

An exported signature must be C-representable end to end — the same types a
`@cstruct` field may use (`Bool`, `Int`, `usize`, `Float`, `Char`, `Str`,
`Bytes`, `Ptr`, a `@cstruct record`), with `Unit` allowed as the return only.
An exported function may not be generic, `async`, or `main`: a type parameter,
a `Result`/`Option`/`List`/`enum`/plain `record`, or `Chan` has no single C
declaration. This keeps the generated header an honest, complete description of
the library's ABI ([11-grammar.md](11-grammar.md)).

A `mut` parameter maps to `T*` in the header and bindings — the C in/out
convention. The callee still uses copy-in/copy-out internally
([04-memory-model.md](04-memory-model.md), [13-codegen.md](13-codegen.md)), so
a C caller passes the address of the value it wants updated.

`Str`/`Bytes` cross as the two-field `XzStr`/`XzBytes` structs (pointer +
length, no NUL guarantee); `Ptr` is `void*`. The `.so` carries the same libc
runtime as `xz build-native` and has no Rust dependency.

## Interop matrix

| Source | Direction | Mechanism |
|---|---|---|
| Xz → C library | call `extern` | direct C ABI |
| Xz → Rust library | via C ABI | Rust exposes `#[no_mangle] extern "C"` |
| Xz → Python library | via C ABI | use CPython C-API / C extension shim |
| Python → Xz module | `xz bind --lang python` | auto-generates `ctypes` wrappers from a `.xz` interface |
| C → Xz library | `xz build --shared` | emits `libXz.so` + `.h` with exported functions |

## Bindings workflow

`xz bind --lang python <file.xz>` reads the same `@export` functions and
`@cstruct` records as `xz build --shared` and writes a `ctypes` module named
after the source file (`foo.xz` -> `foo.py`). The module declares each
`@cstruct` as a `ctypes.Structure` and types every exported function, then
loads the sibling `libXz.so`. The wrapper deliberately does not reuse the
`libXz` name: a `.py` module named `libXz` would be shadowed by `libXz.so`,
which Python treats as an extension module.

### Interface files (`.xzint`)

A `.xzint` interface file is a declaration-only Xz source: `extern func`
signatures for a C library's symbols plus `@cstruct record` declarations for
the types they use. It has no bodies, no `main`, and no other top-level
declarations. Contracts belong to the Xz wrappers that call the externs, not to
the raw declarations. One interface file is written per library and reused by
every project that calls it.

```
xz pkg gen --lang python libcurl.xzint              # -> libcurl.py, loads libcurl.so
xz pkg gen --lang python libcurl.xzint --lib libcurl.so.4
```

`xz pkg gen --lang python <file.xzint>` emits a `ctypes` module named after the
interface file (`libcurl.xzint` -> `libcurl.py`). The module declares each
`@cstruct` as a `ctypes.Structure` and types every `extern` function, then loads
the C library named by `--lib`; the default is the interface file's stem plus
`.so` (`libcurl.xzint` -> `libcurl.so`). Unlike `xz bind`, which loads the
`libXz.so` produced by `xz build --shared`, the wrapper binds the third-party C
library directly.

### Fetching interfaces (`xz pkg add`)

A `.xzint` file is written once per library and reused by every project that
calls it, so a project can pull one from a registry instead of vendoring it by
hand:

```
xz pkg add libcurl                              # -> ./libcurl.xzint
xz pkg add libcurl --registry https://xz.example/interfaces
```

`xz pkg add <name>` fetches `<registry>/<name>.xzint` and writes `<name>.xzint`
into the current directory. The registry is named by `--registry`; when it is
absent the `XZ_REGISTRY` environment variable is used, and with neither set the
command fails rather than guessing a host. `<name>` must be a plain identifier
(letters, digits, `.`, `_`, `-`, not starting with `-` or `.`), so it can neither
escape the registry path nor be read as a command-line flag.

The fetched text is untrusted until it passes exactly the checks `xz pkg gen`
applies: lex, parse, `validate_interface` (only `extern func` and `@cstruct
record`), name resolution, and type checking. A file that fails any check is
rejected and nothing is written, so a malformed or hostile interface never
reaches the project.

Fetching shells out to the host `curl`, falling back to `wget`; no HTTP client
is linked into the compiler. This first slice is fetch-and-verify only: it does
not resolve dependencies, pin versions, or authenticate the registry — a hash or
signature is future work.

## Python bridge (first-class)

- `xz build --shared --bind python` produces a `.so` plus a `.py` wrapper module with proper types (Xz `Int` → Python `int`, `Result` → exceptions or `None`/tuples).
- Xz code can call Python libraries through a generated CPython shim, mapping Xz `Result` to Python exceptions.

## Priority

This is a **Phase 1–2 concern**, not a "later" concern. A language without interop has no reason to exist for real users; interop is the on-ramp.