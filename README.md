# Xz — A General-Purpose Language for the AI-Writing Era

Xz is a general-purpose programming language designed around a single thesis:

> **AI-written, Human-reviewed.**

Code will increasingly be written by AI. Xz is built so that humans can *read, verify, and trust* machine-generated code — by making every behavior explicit, every contract visible, and every failure path typed.

## Design pillars

| Pillar | Choice |
|---|---|
| Philosophy | AI-written, Human-reviewed |
| Type system | Strong static typing; explicit types at contract boundaries, local inference inside bodies |
| Memory model | Value semantics; immutable by default, explicit `mut` |
| Syntax | Python-like (indentation-based) |
| Execution | Compiled to native binaries via LLVM |
| Concurrency | Structured concurrency (async/await) + typed channels |
| Error handling | `Result` types on a single explicit error channel (no exceptions) |
| Intent verification | "No unverified claims" — `@intent`/`@ensures`/`@effects` checked against code, `@trusted` escape hatch |
| Interoperability | FFI-first: C ABI bridge, generated Python bindings (`xz bind`) |
| Compiler feedback | Structured JSON diagnostics designed for LLM self-correction |

## Repository layout

```
Xz/
├── README.md               # This overview
└── docs/
    ├── 01-philosophy.md    # Core thesis and design principles
    ├── 02-syntax.md        # Language syntax (Python-like)
    ├── 03-type-system.md   # Strong static types, contract-point explicitness
    ├── 04-memory-model.md  # Value semantics, explicit mutation
    ├── 05-concurrency.md   # Structured concurrency + typed channels
    ├── 06-error-handling.md# Result types, single error channel
    ├── 07-compiler.md      # LLVM backend, JSON diagnostics
    ├── 08-roadmap.md       # Implementation roadmap
    ├── 09-intent-verification.md  # Comment–code contract checking (differentiator)
    └── 10-ffi-interop.md    # C ABI bridge + Python bindings (ecosystem survival)
```

## Status

**Design/documentation phase.** No implementation yet. See [docs/08-roadmap.md](docs/08-roadmap.md).