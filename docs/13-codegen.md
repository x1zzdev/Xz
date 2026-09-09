# Code Generation — LLVM Backend (Phase 4)

Status: **implemented**. The front end (`lex` → `parse` → `resolve` →
`typecheck` → `intent`) produces an AST; the backend lowers that AST to LLVM
IR and executes it via a JIT engine (`xz run`).

This document is the design contract for the backend. It fixes the type
mapping, module layout, ABI, and the set of constructs Phase 4 supports.

## Pipeline (from [07-compiler.md](07-compiler.md))

```
source ──► lexer ──► parser ──► AST ──► name resolution
      ──► type inference/checking ──► contract checking ──► intent verification
      ──► LLVM IR ──► JIT execution          (Phase 4: xz run)
      ──► native binary / shared library     (Phase 5+: xz build --shared)
```

The front end checks a whole program and collects zero diagnostics before the
backend runs. The backend **assumes a type-correct program**: it does not
re-typecheck. This keeps codegen a faithful, mechanical lowering.

## Module layout (`xz-cli/src/backend/`)

| File | Responsibility |
|---|---|
| `llvm_backend.rs` | owns the `Context`/`Module`/`Builder`; maps types; declares records/enums/functions; emits the `main` entry wrapper. Exposes `compile(program) -> CompiledModule` |
| `codegen.rs` | the recursive Expr/Stmt → instruction lowering (arithmetic, compare, if, match, call, let, `?`, records, enums, Str) |
| `runtime.rs` | the JIT engine; the C-ABI host functions that implement `print`/`to_str`; locating and running `main` |

`runtime.rs` is only used by `xz run`. `llvm_backend.rs` + `codegen.rs` are
what `xz build` (native output, Phase 5) will reuse.

## Type mapping

`Kind` (the front end's checked type) → LLVM type. `Str` is a two-word struct.

| Xz | LLVM |
|---|---|
| `Bool` | `i1` |
| `Int` | `i64` (signed) |
| `usize` | `i64` |
| `Float` | `double` |
| `Char` | `i8` |
| `Unit` | `void` (function return); `{}` (as a value) |
| `Ptr` | `ptr` (opaque) |
| `Str` | `struct { i8*, i64 }` — pointer + byte length |
| `Bytes` | `struct { i8*, i64 }` (same shape as `Str`) |
| `record` | an LLVM `struct` of the field types, in declaration order |
| `enum` | `struct { i8*, i32 }` where the `i32` is the **tag** and the `i8*` is a **boxed heap pointer** to a struct of the active variant's fields (see [Notes](14-codegen-notes.md)) |
| `Result[T, E]` | `struct { T, i1 }` — a *payload + ok-flag* (no heap; the error payload is unused by the runtime) |
| `Option[T]` | `struct { T, i1 }` — same shape as `Result` |
| `Chan[T]`, `Task`, `for`, `async` | not supported in Phase 4 (see Scope) |

### Function ABI

All functions use the C ABI (`extern "C"`). `Unit` return is `void`. Aggregate
values (records, enum, Str, Result) are passed **by value** in a struct, and
returned by value — matching how the C-ABI host functions (`print`, `to_str`)
expect them. Records and enums are *value types* ([04-memory-model.md](04-memory-model.md)),
so a function that takes a record copies it in by value.

`main` is special: it may return `Unit` or `Result[Unit, Err]`, and the runtime
calls it with no arguments.

## Runtime host functions (`runtime.rs`)

`print(x: Str)` and every `to_str()` method have no Xz body — they are lowered
to calls to C-ABI functions provided by the host, injected via
`ExecutionEngine::add_global_mapping` so the JIT resolves them without a
system linker.

| Host function | Signature | Implements |
|---|---|---|
| `xz_print` | `fn(i8*, i64) -> ()` | `print(s: Str)` — writes the byte range to stdout |
| `xz_i64_to_str` | `fn(i64) -> XzStr` | `Int.to_str()` |
| `xz_f64_to_str` | `fn(f64) -> XzStr` | `Float.to_str()` |
| `xz_char_to_str` | `fn(i8) -> XzStr` | `Char.to_str()` |
| `xz_bool_to_str` | `fn(i1) -> XzStr` | `Bool.to_str()` |
| `xz_sqrt` | `fn(f64) -> f64` | `sqrt` → `libm sqrt` (contracts.xz) |

`XzStr` is `#[repr(C)] { ptr: usize, len: usize }`. Each host `to_str` builds a
`String`, copies its bytes into a leaked heap buffer, and returns
`{ ptr, len }`. The returned memory is never freed (process-scoped, acceptable
for `xz run`).

### Str ownership in codegen

The front end treats `Str` as a value, but the backend needs *stable* byte
memory across calls. The rule: **a `Str` value is a `{ ptr, len }` pair pointing
at an immutable, process-lifetime buffer.** String literals are emitted as LLVM
globals. `Int.to_str()` / `Float.to_str()` / concatenation allocate a fresh
leaked buffer. Concatenation allocates once and copies both halves. There is no
mutation of `Str` in Phase 4, so no free is needed.

## Codegen rules (Expr → IR)

| Expr | Lowering |
|---|---|
| literal `Int`/`Float`/`Bool`/`Char` | LLVM constants |
| literal `Str` | a module-level global byte array; the value is `{ gep(global), len }` |
| `Name` | load from the alloca that holds the binding (see `let`) |
| `let x = e` | `alloca T`; store `e`; keep the alloca in scope |
| `x op y` arithmetic | `build_int_*` / `build_float_*` by operand kind |
| comparison | `build_int_compare` / `build_float_compare`; `Int` is signed (`SGT`/…), `Float` is ordered |
| `and`/`or`/`not` | `build_and`/`build_or`/`build_not` on `i1` |
| `if c { a } elif ... { } else { b }` | lower the elif/else chain to nested two-branch ifs; merge with `build_phi` |
| `match` | branch on a tag (enum) or the ok-flag (Result/Option); the `match` value is a `phi` of arm values |
| `ok(v)` / `err(e)` | struct `{ v, 1 }` / `{ zero, 0 }` |
| `none` | a zero of the declared `Option[T]` struct (the binding's type is the payload's source) |
| `expr?` | branch on the ok-flag; on err, return a zero aggregate (early return) |
| `x is some` / `x is ok` in `if` | flow typing: the branch narrows `x` to a pointer at its payload field, matching the type checker |
| `x.value` under `is ok` | `extract_value` the payload |
| record field access | `extract_value` (records are by-value structs) |
| record construction | `insert_value` into a zero struct, in field order |
| enum construction | allocate a heap box, store the variant's fields, build `{ box, tag }` |
| function call | `build_direct_call` with the target's `FunctionValue` |
| method call (`.to_str()`, `.len()`, `.abs()`) | lowered to host functions or a field op |
| `main` body | its block is generated into the `main` `FunctionValue` |

### `?` early return

`expr?` on a `Result[T,E]` produces the payload value in the success branch.
The failure branch returns an empty aggregate of the enclosing function's
return type. Contracts already guarantee the error channel is `Err`-accepted,
so the runtime never observes an error value here.

## `xz build` vs `xz run`

- `xz build <file.xz>` runs the full check pipeline, then generates the module
  and verifies it (`Module::verify`). Phase 4 emits IR only; native binary
  output (a `.o`/executable) is Phase 5.
- `xz run <file.xz>` runs the check pipeline, compiles to a JIT engine, links
  the host functions, and calls `main`, propagating any returned error as a
  non-zero exit.

## Scope (explicitly out of Phase 4)

- `task`, `async`/`await`, `chan`/`send`/`recv`, `for`, `loop` — Phase 6
  (concurrency runtime), no IR lowering here.
- `extern`/`Ptr`/`transfer`, shared-library output, `xz bind` — Phase 5 (FFI).
- `for`/`loop`/`break`/`continue` — Phase 6 (concurrency runtime), no IR lowering here.
- Generic function instantiation (`max[T: Ordered]`, `Point[T]`) — the front
  end typechecks them, but Phase 4 lowers only *concrete* function signatures.
  A generic call is not yet code-generated.
- `List`/`Map`/`Set` and collection `[]` indexing — Phase 7.
- No optimization passes, no debug info, no bitcode file output in Phase 4
  (`xz run` JIT-compiles at `OptimizationLevel::None`).

These exclusions keep the backend a reviewable, mechanical phase; each later
phase (5/6/7) is a documented extension of this contract.
