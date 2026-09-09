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
| Syntax | Python-like; brace-delimited blocks, indentation layout |
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
├── examples/               # Design-validation programs (not runnable yet)
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
    ├── 10-ffi-interop.md    # C ABI bridge + Python bindings (ecosystem survival)
    ├── 11-grammar.md       # Full grammar: lexical, precedence, EBNF (authoritative)
    └── 12-stdlib.md        # Minimum stdlib surface (collections/network: Phase 7)
```

## Status

**Phases 1–4 implemented.** The front end (`xz check`) and the LLVM JIT backend
(`xz run`) work; `hello.xz` and `contracts.xz` execute. Phases 5–8 (FFI,
concurrency runtime, stdlib, tooling) are on the
[docs/08-roadmap.md](docs/08-roadmap.md). The backend uses a root-free portable
LLVM 17 (see [docs/13-codegen.md](docs/13-codegen.md)).