# Roadmap

Status: Phases 1–4 (front end + LLVM JIT backend) are implemented; `xz check`
and `xz run` work on the example programs. Phases 5–8 are not yet implemented.

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
- [x] JIT execution (`xz run`) with host `print`/`to_str`/`abs`/`sqrt`
- [x] IR emission (`xz build`, Phase 4 output is IR; native binary is Phase 5)
- [x] Example programs execute: `hello.xz`, `contracts.xz`
- [x] LLVM optimization pipeline on the JIT path (O3 + `globaldce`; program
  functions internal so DCE drops the dead ones — see [13-codegen.md](13-codegen.md))
- [ ] Native binary output
- [ ] JSON diagnostics emission

## Phase 5 — FFI & interop (early, by design)

- `extern` declarations and C ABI bridge
- Type mapping (Xz ↔ C), `@cstruct` records, `Ptr`
- Shared-library output (`xz build --shared`)
- `xz bind --lang python` ctypes wrapper generation

## Phase 6 — Concurrency runtime

- async/await scheduler
- Typed channels
- Deterministic scheduling

## Phase 7 — Standard library

- Collections, I/O, math, time
- Networking (HTTP)

## Phase 8 — Tooling

- LSP server
- Formatter
- AI toolchain integration (JSON diagnostics + editor loop)
- Package manager (`xz pkg`) for `.xzint` interface files

## Guiding constraint for every phase

Every feature must strengthen the answer to the reviewer's questions:

*What does it do? What can it change? What are its guarantees? What can go wrong?*

If a proposed feature makes any of those questions harder to answer, it is rejected.