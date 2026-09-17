# Roadmap

Status: Phases 1–4 (front end + LLVM JIT backend) are implemented; `xz check`
and `xz run` work on the example programs. Phase 5 (FFI) is underway — the
C ABI bridge, shared-library output, and `xz bind --lang python` work.
Phase 6 has typed channels, a deterministic task scheduler, and `async`/`await`
lowering on the JIT path; the native runtime does not yet emit the scheduler.
Phase 7 has begun (`List`/`Map`/`Set` first slices, `read_file`, and clock
reads); Phase 8 has begun with the LSP diagnostics server and now includes the
`xz fmt` formatter, the `xz pkg gen --lang python` binding generator for
`.xzint` interface files, and `xz pkg add` registry fetch + verify.

## Phase 0 — Design (current)

- [x] Core philosophy ([01-philosophy.md](01-philosophy.md))
- [x] Language design decisions (type system, memory model, syntax, concurrency, errors)
- [x] Intent verification design ([09-intent-verification.md](09-intent-verification.md))
- [x] FFI/interop design ([10-ffi-interop.md](10-ffi-interop.md))
- [x] Spec gaps closed: `usize`/`Ptr`/`Err`/`Unit`/error unions defined, `?` propagation rule, `@trusted` placement, `result.value` narrowing under `is ok`, channel-as-sanctioned-global-state, program-termination rule, **handle semantics** (`Ptr`-bearing records: no copies, `transfer`-only handoff — the sanctioned exception to value semantics) ([02](02-syntax.md), [03](03-type-system.md), [05](05-concurrency.md), [06](06-error-handling.md), [09](09-intent-verification.md), [10](10-ffi-interop.md))
- [x] Intent-verification semantics made implementable: NL claims never parsed — structural pairing with `pre`/`post`, formal proof, effect derivation ([09](09-intent-verification.md))
- [x] Example programs to validate ergonomics ([../examples](../examples/))
- [x] Full grammar specification — lexical rules, operator precedence, EBNF, intent-comment grammar, well-formedness constraints ([11-grammar.md](11-grammar.md))
- [x] Standard library API draft — minimum surface needed to typecheck the examples; full stdlib is Phase 7 ([12-stdlib.md](12-stdlib.md))

## Phase 1 — Front end

- Lexer & indentation parser (nom / pest)
- AST with source spans
- Name resolution

## Phase 2 — Type system

- Constraint-based inference (bounded to function bodies)
- `Result` / `Option` / generics
- Exhaustiveness checks
- Unit types (`Meters`, `Seconds`, …)
- Built-in method surface (`to_str`, `Str`/`Option` methods, [12-stdlib.md](12-stdlib.md))

## Phase 3 — Contracts & intent verification

- `pre` / `post` / `invariant` parsing and checking
- Static provability where possible; structured diagnostics otherwise
- `@intent` / `@requires` / `@ensures` / `@effects` parsing and claim checking
- Automatic effect-profile derivation and comparison
- `@trusted` stamp and `--strict` build mode (no unproven, untrusted claims)

## Phase 4 — Backend

- [x] LLVM IR generation (inkwell) — `xz-cli/src/backend/` ([13-codegen.md](13-codegen.md))
- [x] JIT execution (`xz run`); host `print`/`to_str`; `abs`/`sqrt` as LLVM intrinsics
- [x] IR emission (`xz build`, Phase 4 output is IR; native binary is Phase 5)
- [x] Example programs execute: `hello.xz`, `contracts.xz`
- [x] LLVM optimization pipeline on the JIT path (O3 + `globaldce`; program
  functions internal so DCE drops the dead ones — see [13-codegen.md](13-codegen.md))
- [x] Conservative Str buffer reclamation (registry-guarded `xz_str_free`;
  unique-owned bindings freed at overwrite/exit, aliases leak — see
  [13-codegen.md](13-codegen.md))
- [x] Loop support (`loop` + `for i in n` range + `break`/`continue`) —
  iterative benchmarks now measurable (~C/Rust speed: 10M-iteration loop in
  ~0.05s, see [13-codegen.md](13-codegen.md))
- [x] Generic function monomorphization (per-call-site specialization from the
  concrete argument types — see [13-codegen.md](13-codegen.md))
- [x] JSON diagnostics emission (`xz check-json`; structured JSON for the AI
  toolchain loop)
- [x] Native binary output (`xz build-native`: emit IR + native libc runtime,
  compile with `llc`, link with `ld` — a standalone executable, no Rust runtime)
- [x] Record field assignment (`p.x = ...`) and the full `Str` method surface
  (`at`/`to_upper`/`to_lower`/`to_bytes`)
- [x] CLI exits non-zero on errors; the parser reports malformed input instead
  of panicking
- [x] Enforce `mut` for assignment/field assignment

## Phase 5 — FFI & interop (early, by design)

- [x] `extern` declarations and C ABI bridge
- [x] Type mapping (Xz ↔ C), `@cstruct` records, `Ptr`
- [x] Shared-library output (`xz build --shared`) — `@export` functions,
  generated `libXz.so` + `libXz.h`
- [x] `xz bind --lang python` ctypes wrapper generation

## Phase 6 — Concurrency runtime

- [x] Typed channels + task scheduler for the JIT — deterministic run-to-block
  ([05-concurrency.md](05-concurrency.md), [13-codegen.md](13-codegen.md))
- [x] Deterministic scheduling (specified and implemented for tasks/channels)
- [x] async/await scheduler and lowering

## Phase 7 — Standard library

- [x] `List[T]` first slice — literal `[a, b, c]`, bounds-checked `xs[i]`,
  non-mutating `append`, `len`/`is_empty`, `for x in xs`
- [x] `Map[K, V]` first slice — literal `{k: v}`, `Option` lookup `get`,
  non-mutating `insert`, `len`/`is_empty`, `keys`/`values`; insertion order
- [x] `Set[T]` first slice — literal `{e1, e2}` (no colon), element-restricted
  (`Int`/`usize`/`Bool`/`Char`/`Str`), non-mutating `insert`, `contains`,
  `len`/`is_empty`, insertion-order iteration
- [x] `io` first slice — `read_file(path)` returning `Result[Str, IoError]`
  (`@effects io`; [12-stdlib.md](12-stdlib.md); `examples/io.xz`)
- [x] `math` — `PI`/`E` constants and `approx_sqrt`/`abs` (earlier phases;
  [12-stdlib.md](12-stdlib.md); `examples/contracts.xz`)
- [x] `time` first slice — `now()` and `monotonic()` clock reads
  (`@effects io`; [12-stdlib.md](12-stdlib.md); `examples/time.xz`)
- Networking (HTTP)

## Phase 8 — Tooling

- [x] LSP diagnostics server, first slice (`xz lsp`; stdio LSP, `initialize` +
  `didOpen`/`didChange`/`didClose` → `publishDiagnostics`; [07-compiler.md](07-compiler.md))
- [x] LSP server — type-error spans; hover over top-level symbols
- [x] LSP server — completion (document symbols + language vocabulary)
- [x] LSP server — go-to-definition
- [ ] LSP server — formatting (`textDocument/formatting` over the `xz fmt` engine)
- [x] Formatter (`xz fmt`; comment-preserving AST pretty-printer — [07-compiler.md](07-compiler.md))
- [ ] AI toolchain integration (JSON diagnostics + editor loop)
- [x] `xz pkg gen --lang python` — ctypes wrappers from `.xzint` interface
  files ([10-ffi-interop.md](10-ffi-interop.md))
- [x] `xz pkg add` — fetch + verify an interface definition from a registry

## Guiding constraint for every phase

Every feature must strengthen the answer to the reviewer's questions:

*What does it do? What can it change? What are its guarantees? What can go wrong?*

If a proposed feature makes any of those questions harder to answer, it is rejected.