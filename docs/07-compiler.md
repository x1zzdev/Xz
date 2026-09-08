# Compiler

## Pipeline

```
source ──► lexer ──► parser ──► AST ──► name resolution
      ──► type inference/checking ──► contract checking ──► intent verification
      ──► LLVM IR ──► native binary / shared library
```

Backend: **LLVM** via Rust bindings (inkwell) — the same ecosystem proven in Xazz. Rust's parser libraries (nom, pest) are used for the front end.

Shared-library output (`xz build --shared`) is the interop on-ramp; `xz bind --lang python` generates Python wrappers from interface files (see [10-ffi-interop.md](10-ffi-interop.md)).

## CLI

```
xz build <file.xz>          # type check + contract check + codegen
xz check <file.xz>          # type/contract check only, no codegen
xz run <file.xz>            # build and execute
```

## JSON Diagnostics (for LLM self-correction)

Every diagnostic is emitted as structured JSON in addition to human-readable text:

```json
{
  "version": 1,
  "severity": "error",
  "code": "E0032",
  "message": "contract precondition may be violated",
  "category": "contract",
  "span": { "file": "src/main.xz", "start": [14, 5], "end": [14, 19] },
  "suggestion": {
    "fix": "add 'pre path != \"\"' to the declaration",
    "confidence": 0.9
  }
}
```

### Diagnostic guarantees

- **Stable error codes** — codes are never renumbered or reused for different errors
- **Machine-readable spans and categories** — deterministic, queryable
- **Suggested fixes with confidence scores** — the compiler proposes repairs
- **Round-trip loop** — an AI tool reads `code` + `span` + `suggestion`, applies a fix, re-runs. This is the self-correction loop that makes "AI-written, human-reviewed" practical.
- **Intent diagnostics** — codes `I0001` (unprovable formal claim), `I0020` (undeclared effect), `I0021` (NL claim without a paired formal contract), `I0022` (missing intent comment) power the truthfulness check in [09-intent-verification.md](09-intent-verification.md)

## Feedback to the writer (AI)

| Field | Purpose |
|---|---|
| `code` | stable identity for learning/fix mappings |
| `span` | exact location for surgical edits |
| `category` | groups errors by phase (lex, parse, type, contract) |
| `suggestion.fix` | actionable repair text |
| `suggestion.confidence` | lets the AI decide whether to auto-apply |

## Optimizations (planned)

- Copy-on-write for large values
- Tail-call elimination
- Dead-code elimination for unreachable contract branches
- Aggregate value copy elision (value semantics preserved)