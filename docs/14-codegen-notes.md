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

## 4. `Str` as `{ i8*, i64 }` with bounded reclamation

`Str` maps to a two-word struct `{ ptr, len }`.

- string literals → module-level globals (`build_global_string_ptr`);
- `to_str()` results and concatenations → heap buffers in the host (`std::alloc`);
- `Str.len()` → the byte length field (the stdlib says "character count";
  for the ASCII examples this is equal — documented limitation).

Memory is reclaimed **conservatively but soundly**:

- Every host allocation is recorded in a global registry
  (`LIVE_STR`). `xz_str_free(ptr, len)` deallocates **only** if `ptr` is still
  in the registry, so freeing a literal, an unknown pointer, or an
  already-freed buffer is a no-op. This makes the generated IR safe to be
  sloppy: the registry is the backstop against double-frees.
- Codegen marks a binding as the **unique owner** of a fresh buffer only when
  the buffer was created by this function (`concat`, `to_str`) and never
  copied. Unique owners are freed at overwrite and function exit.
- Any copy or embed — `let y = x`, identity `x.to_str()`, a `Str` inside
  `Option`/`Result`/record/enum, or a `Str` through an `if`/`match` phi —
  *downgrades* the source to shared, so it leaks instead of being freed. This
  is the sound trade: aliased buffers leak rather than risk a use-after-free.
- A function whose return type **is or contains `Str`** never frees at exit
  (the return value may alias a local buffer that must outlive the call).
- Fresh temps passed directly into `print` are freed right after the call.

`wasm`/native phases can take the next step: a per-frame arena (free the whole
frame at exit) or real reference counting, once `Str` mutation/loops exist.

## 5. The C ABI for host functions

`print` and every `to_str()` are Rust `#[unsafe(no_mangle)] extern "C"`
functions injected into the JIT via `ExecutionEngine::add_global_mapping`.
The ABI detail that mattered: a function returning the `XzStr` struct
(`#[repr(C)] { usize, usize }`) by value must match LLVM's `{ i8*, i64 }`
struct return — it does on x86-64 SysV. The architecture test in
`/tmp/opencode/llvmtry` proved this before the backend was written.

> **Gotcha.** `get_function_value(name)` failed for *declared but
> undefined* host functions, so binding silently no-op'd and the JIT jumped to
> a null address (segfault on `print`). Fix: look the functions up on the
> **module**, not the execution engine.

`abs` and `sqrt` were originally host calls too (`xz_i64_abs`, `xz_f64_abs`,
`xz_sqrt`). That made every use a C-ABI round-trip the optimizer cannot see
through. They are now LLVM intrinsics (`llvm.abs.i64`, `llvm.fabs.f64`,
`llvm.sqrt.f64`) emitted via inkwell's `Intrinsic::find(...).get_declaration()`
and called directly — the backend lowers them to native instructions
(single/multi-instruction, fully scheduling) and the optimizer can fold or
inline them. `print`/`to_str` stay host calls (they are I/O and formatting;
inlining a format call's body is not a win and keeps the C ABI contract
small).

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

## 11. Optimization is a pipeline, not a flag

The first version of `xz run` JIT-compiled the module directly at
`OptimizationLevel::None` — LLVM only did instruction-selection-level cleanup.
That is the classic "LLVM backend but no speed" failure mode: an unoptimized
lowering of `let`/`alloca`-heavy IR is slower than plain C. The fix
(`LlvmBackend::optimize`, run by `runtime::run`) is:

- initialize the native target and build a host `TargetMachine`;
- run the new pass manager's default O3 pipeline plus `globaldce` via
  `Module::run_passes`, with `set_verify_each(true)` so any pass that would
  produce invalid IR fails loudly before the JIT;
- compile with `OptimizationLevel::Aggressive`.

Two decisions made this safe:

1. **Program functions are internal; `main` stays external.** `globaldce`
   (inside the default pipeline) removes internal functions with no callers.
   When `main` was first left external-only and `internalize` was appended, the
   pass internalized `main` too and then DCE'd it, breaking `xz run` with "no
   'main' function". Declaring program functions internal from the start (with
   `set_linkage`) means the pipeline's own `globaldce` does the right thing.
2. **`?` flow-typing pointers survive passes.** The narrow-on-`is` trick keeps
   a *pointer into an alloca field* in the scope table. `mem2reg`/SROA leave
   allocas whose address escapes alone (they are only promoted when provably
   safe), so the lowering stays correct under optimization. The regression
   test `optimization_pipeline_inlines_and_dces` pins this down: it asserts the
   trivial `sq` helper is inlined, DCE'd, and its call site constant-folded.

This pipeline is also what makes the value-semantics aggregate copies cheap:
SROA and `mem2reg` promote small records to registers and drop the
`alloca`/`store`/`load` round trips, which is most of the "aggregate copy
elision" the memory model promises (docs/04-memory-model.md).