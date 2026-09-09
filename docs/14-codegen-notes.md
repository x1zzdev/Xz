# Codegen Notes — Hard Problems and Decisions (Phase 4)

This document records the non-obvious engineering problems encountered while
building the LLVM backend, and the decisions made. It exists so a reviewer of
AI-written code can see *why* the backend looks the way it does — the reasoning
is as much the deliverable as the code.

## 1. Environment: root-free LLVM 17

The build host has no system LLVM and no root access. The solution is a
**home-directory portable LLVM 17** — Ubuntu `.deb`s (`llvm-17`,
`llvm-17-dev`, `llvm-17-tools`) extracted into
`~/.local/share/xz-llvm17/debroot/` without installing. Two paths matter:

- `LLVM_SYS_170_PREFIX` → the extracted `usr/lib/llvm-17` (llvm-sys's search
  root for `llvm-config`, headers, `libLLVM.so`).
- `LIBRARY_PATH` → the extracted `usr/lib/x86_64-linux-gnu` (where
  `libLLVM-17.so.1` and `libffi.so.8` actually live; the `llvm-17/lib`
  symlinks point back here).

Both are pinned in `xz-cli/.cargo/config.toml` (`[env]`) so plain `cargo build`
works; `scripts/setup-llvm.sh` verifies and re-emits them.

> **Gotcha.** The extracted tree shipped `libffi.so` → `libffi.so.8.2.0` but
> the target file was missing, so the linker failed on `-lffi`. Copied the
> system `libffi.so.8.2.0` into the tree. Check for dangling symlinks when a
> new machine is set up.

## 2. inkwell's borrow model vs. a symbol table

`LlvmBackend` owns the LLVM `Context`, `Module`, and `Builder`. The first
attempt stored `context: Context` (owned). The problem: `append_basic_block`
returns a `BasicBlock` whose lifetime is tied to the **borrow** of the
`&Context` receiver. Holding block handles (for `if`/`match` merge phi nodes)
while mutating the codegen symbol table (`&mut self`) is a borrow error.

**Decision:** leak the context to `'static` and store it as a `&'ctx Context`
Copy reference. Accessing the field copies the reference, so block handles
borrow the (leaked) context, not the backend — the symbol table can be mutated
freely. This matches how inkwell's own kaleidoscope example is structured.

## 3. `main` is `void`; other functions return their real type

The runtime calls `main` as a no-arg C function and ignores the result, but
Xz programs declare `func main() -> Result[Unit, Err]` ending in `ok()`.
The backend therefore declares `main` with a **void** ABI regardless of its
declared return, and `gen_main` uses a void return. Every other function gets
its declared return type. `?` early-returns in `main` become `ret void`.

## 4. `Str` as `{ i8*, i64 }` with process-lifetime buffers

`Str` maps to a two-word struct `{ ptr, len }`. The pointer must stay valid for
the whole run, so:

- string literals → module-level globals (`build_global_string_ptr`);
- `to_str()` results and concatenations → **leaked heap buffers** in the host
  (`std::alloc`), never freed;
- `Str.len()` → the byte length field (the stdlib says "character count";
  for the ASCII examples this is equal — documented limitation).

There is no mutation of `Str` in Phase 4, so "never free" is sound.

## 5. The C ABI for host functions

`print` and every `to_str()` are Rust `#[unsafe(no_mangle)] extern "C"`
functions injected into the JIT via `ExecutionEngine::add_global_mapping`.
The ABI detail that mattered: a function returning the `XzStr` struct
(`#[repr(C)] { usize, usize }`) by value must match LLVM's `{ i8*, i64 }`
struct return — it does on x86-64 SysV. The architecture test in
`/tmp/opencode/llvmtry` proved this before the backend was written.

> **Gotcha.** `ee.get_function_value(name)` failed for *declared but
> undefined* host functions, so binding silently no-op'd and the JIT jumped to
> a null address (segfault on `print`). Fix: look the functions up on the
> **module**, not the execution engine.

## 6. Enum layout: `{ box, tag }` instead of a plain `i32`

Enums are `struct { i8*, i32 }`: a heap **box** holding the active variant's
fields (a `struct` of the variant's field types, allocated with `build_malloc`)
plus the variant **tag**. Variant constructors allocate the box and store the
fields by GEP; `match` branches on the tag and re-derives the fields from the
box. Boxing keeps variant payloads uniformly sized so the enum is a value
type that can be passed/returned by value, matching the docs' value semantics.

## 7. Codegen is untyped — it dispatches on LLVM value types

The AST carries no types (they live in the type checker). Codegen decides what
to do by inspecting each value's LLVM type: `i64`/`i1`/`i8`/`double`/struct,
and distinguishes a record (named struct) from `Result`/`Option` (anonymous
struct) by the struct's name. This keeps the backend a mechanical lowering
without re-typing the program.

## 8. Result/Option as `{ payload, i1 }`

`Result[T, E]` and `Option[T]` are `struct { payload, i1 }` (an ok-flag). `?`
branches on the flag and early-returns a zero aggregate on the error path.
`err(e)` **never evaluates `e`** — the error payload is unreachable at runtime
(only the flag matters), so the `DomainError("...")` constructor is skipped
entirely. This is why no error-record constructors need codegen.

## 9. A real front-end bug surfaced by execution

`xz run` printed `50` for `5.0`: the lexer concatenated the integer and
fractional digit runs and parsed them as one number (`"50"`), and ignored the
exponent entirely. Phase 1–3 never exercised values (typechecking only), so
the bug was latent. Fixed by parsing the full literal text (underscores
stripped) as `f64`. Lesson: execution is the fastest way to find front-end
bugs, and it is why `xz run` is part of Phase 4.

## 10. Verification is the safety net

`LlvmBackend::compile` ends with `Module::verify()`. This caught the two
worst codegen mistakes (a terminator-less match block and a `ret void` vs.
`ret { {} , i1 }` type mismatch in `main`) before any JIT crash. Keep it.