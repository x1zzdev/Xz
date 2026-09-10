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

## 7. Codegen is untyped — and its type dispatch is compile-time

The AST carries no types (they live in the type checker). Codegen decides what
to do by inspecting each value's LLVM type: `i64`/`i1`/`i8`/`double`/struct,
and distinguishes a record (named struct) from `Result`/`Option` (anonymous
struct) by the struct's name. This keeps the backend a mechanical lowering
without re-typing the program.

Two clarifications matter for review:

- **The dispatch is `compile-time`.** `is_str(a)`, `is_float(a)`, `is_struct(a)`
  and the `to_str` host-function pick by bit width are Rust `match` on
  `v.get_type()` — they run in the *compiler*, never at runtime. LLVM sees one
  statically chosen lowering per construct; there is no runtime tag test. A
  review reading these as "분기 at codegen" is correct, but as "runtime
  branches" it is not: the emitted IR has no such conditional.
- **IR quality in the cases that do re-derive types is fixed by the
  optimizer.** `kind_to_llvm`/`is_str` re-derive a type from the zero-initialized
  struct each time; the O3 pipeline (Unit 1) folds those into constants. The
  single maintainability trade-off — the backend re-derives types instead of
  carrying `Kind` through — is deliberate: it keeps the lowering purely
  mechanical, and re-typing the AST is a refactor without runtime benefit.

## 8. Result/Option as `{ payload, i1 }`, and the padding decision

`Result[T, E]` and `Option[T]` are `struct { payload, i1 }` (an ok-flag). `?`
branches on the flag and early-returns a zero aggregate on the error path.
`err(e)` **never evaluates `e`** — the error payload is unreachable at runtime
(only the flag matters), so the `DomainError("...")` constructor is skipped
entirely. This is why no error-record constructors need codegen.

**Padding, and why it's accepted (feedback point 5).** For a scalar payload,
`{ payload, i1 }` has ABI padding: `{ i64, i1 }` is 16 bytes in memory (7 bytes
of tail padding). Field order (`{ i1, payload }`) does not help — the payload's
alignment forces the same size. This is a per-value ABI cost, not a heap cost;
in registers LLVM passes `{ i64, i1 }` as one value and the O3 pipeline's
SROA/scalarization often removes the stack round-trip entirely. The real
compaction — **niche optimization (NPO)** on the null pointer for
`Option[Ptr]` / `Option[&T]` (8 bytes: `null` = `none`) — is deferred to Phase 5
(FFI), where the payload set is known and one specific niche covers the common
case. Redesigning the ABI now would ripple through every call site and the
host for no measurable Phase-4 win.

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

## 12. Loops, and the "terminator in the middle of a block" trap

`loop`/`for`/`break`/`continue` lower to header/body/incr/after blocks. The
trap: after a `break` or `continue` the current block is already terminated,
and any code generator that unconditionally appends the loop back-edge (or an
`if`/`match` merge branch) produces `Terminator found in the middle of a basic
block!`. The fix is a `block_terminated()` guard used everywhere a block might
end in a control-flow jump — `gen_block` stops emitting after a `break`, and
`gen_if`/`gen_match`/`gen_loop`/`gen_for` only append their merge/back-edge
branches when the block is not already terminated. `Module::verify()` catches
any missed case.

Phase 4's `for i in n` is restricted to the integer range `0..n` (n exclusive,
`n: Int`); the type checker enforces that the iterable is an `Int`, keeping the
lowering total. Collection iteration is a Phase 7 concern. The loop support
makes iterative benchmarks measurable — the pipeline above turns the lowered
induction-variable loop into native machine code, so a 10M-iteration
accumulation loop runs in ~0.05s on this host (Rust/C -O2 territory).

## 13. Native output: an IR runtime and a malloc/data-layout trap

`xz build-native` produces a standalone executable: the module gets IR bodies
for the `xz_*` runtime functions (calling libc `write`/`malloc`/`memcpy`/
`free`/`snprintf`), `llc` lowers it to an object file, and `ld` links it with
the C runtime (`crt1.o`/`crti.o`/`crtn.o`) and `-lc -lm`. `main` is declared
returning `i32` (0) because that is what the C runtime expects; the runtime's
`void main` and the JIT path are unchanged.

Two traps surfaced:

1. **`build_malloc` vs. the runtime's `malloc`.** inkwell's `build_malloc`
   (used for enum boxes) emits a call to `malloc` sized by the *IRBuilder's*
   data layout, which is empty and therefore 32-bit — `malloc(i32)`. The
   native runtime declared `malloc(i64)`. The optimizer then split the two
   into `malloc` and `malloc.1`, and the link failed on the renamed one.
   Fix: declare one `malloc(i64)` in `compile` and call it explicitly (enum
   boxes compute their ABI size via the module's `TargetData`), and set the
   module's triple/data layout so all sizes are pointer-sized.
2. **`main` must return `int`.** crt1 calls `main` and uses the return value as
   the exit status; a `void main` leaks garbage into `$?`. Declaring `i32` and
   emitting `ret i32 0` fixes it (the JIT ignores the value).

This is the Phase 5 on-ramp: the same object/link path generalizes to
`xz build --shared` and the FFI interop surface.