# Roadmap

Status: **documentation only** — no implementation yet.

## Phase 0 — Design (current)

- [x] Core philosophy ([01-philosophy.md](01-philosophy.md))
- [x] Language design decisions (type system, memory model, syntax, concurrency, errors)
- [ ] Full grammar specification
- [ ] Standard library API draft
- [ ] Example programs (to validate ergonomics)

## Phase 1 — Front end

- Lexer & indentation parser (nom / pest)
- AST with source spans
- Name resolution

## Phase 2 — Type system

- Constraint-based inference (bounded to function bodies)
- `Result` / `Option` / generics
- Exhaustiveness checks
- Unit types (`Meters`, `Seconds`, …)

## Phase 3 — Contracts

- `pre` / `post` / `invariant` parsing and checking
- Static provability where possible; structured diagnostics otherwise

## Phase 4 — Backend

- LLVM IR generation (inkwell)
- Native binary output
- JSON diagnostics emission

## Phase 5 — Concurrency runtime

- async/await scheduler
- Typed channels
- Deterministic scheduling

## Phase 6 — Standard library

- Collections, I/O, math, time
- Networking (HTTP)

## Phase 7 — Tooling

- LSP server
- Formatter
- AI toolchain integration (JSON diagnostics + editor loop)

## Guiding constraint for every phase

Every feature must strengthen the answer to the reviewer's questions:

*What does it do? What can it change? What are its guarantees? What can go wrong?*

If a proposed feature makes any of those questions harder to answer, it is rejected.