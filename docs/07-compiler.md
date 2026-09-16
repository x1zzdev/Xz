# Compiler

## Pipeline

```
source ──► lexer ──► parser ──► AST ──► name resolution
      ──► type inference/checking ──► contract checking ──► intent verification
      ──► LLVM IR ──► native binary / shared library
```

Backend: **LLVM** via Rust bindings (inkwell) — the same ecosystem proven in Xazz. Rust's parser libraries (nom, pest) are used for the front end.

**Phase 4 status:** `xz build`/`xz run` are implemented for the front end plus a
JIT backend (see [13-codegen.md](13-codegen.md) for the design contract and
[14-codegen-notes.md](14-codegen-notes.md) for the hard problems). The backend
runs on a root-free portable LLVM 17 (`xz-cli/.cargo/config.toml` + `scripts/setup-llvm.sh`).

Shared-library output (`xz build --shared`) is implemented: it emits `libXz.so`
+ a generated `libXz.h` for the `@export` functions; `xz bind --lang python`
generates Python wrappers from interface files (see
[10-ffi-interop.md](10-ffi-interop.md)).

## CLI

```
xz build <file.xz>          # type check + contract check + codegen (Phase 4: emits LLVM IR)
xz build --shared <file.xz> # emit libXz.so + libXz.h for the @export functions
xz build-native <file.xz>   # emit IR + native runtime, compile with llc, link with ld -> ./xz_program
xz bind --lang python <file.xz>  # emit a ctypes wrapper (<stem>.py) for the @export functions
xz check <file.xz>          # type/contract check only, no codegen
xz check --strict <file.xz> # intent checks enforced (I0004: untrusted claims fail)
xz check-json [--strict] <file.xz>   # same, diagnostics as a JSON array
xz run <file.xz>            # build and JIT-execute
xz lsp                      # language server on stdio (Phase 8, first slice)
```

## Language server (`xz lsp`)

`xz lsp` runs a synchronous Language Server Protocol server over stdio. It
reuses the same pipeline as `xz check` (see `src/driver.rs`), so an editor sees
exactly the diagnostics `xz check-json` would print — same codes, messages, and
spans.

- **Document sync:** full (`TextDocumentSyncKind.Full`); every `didChange`
  carries the whole document, so no incremental edit state is kept.
- **Methods:** `initialize` / `initialized`, `shutdown` / `exit`,
  `textDocument/didOpen`, `textDocument/didChange`, `textDocument/didClose`,
  `textDocument/hover`, `textDocument/completion`. Diagnostics are published on
  open and recomputed on every change; `didClose` clears them.
- **Hover:** hovering an identifier that names a top-level symbol (`func`,
  `task`, `chan`, `extern`, `record`, `enum`, or enum variant) returns the
  rendered signature and any doc claims as markdown; anything else is `null`.
  Resolution is by name from the current document, and requires the document
  to parse.
- **Completion:** returns the top-level symbols the document declares (each with
  its kind, rendered signature, and doc claims) plus the static language
  vocabulary: keywords, built-in types, and global stdlib names/constants. The
  static vocabulary is still offered when the document has a parse error, so
  completion keeps working mid-edit; only a document that is not open returns
  an empty list. The client owns prefix filtering (`isIncomplete: false`).
- **Positions:** 1-based Xz spans are converted to 0-based UTF-16 code units
  (the LSP default; advertised as `positionEncoding: "utf-16"`).
- **Exit code:** 0 after a `shutdown` request, 1 otherwise (per the LSP spec).

Not yet implemented: go-to-definition and formatting.
Type errors carry a real span (`T0001` points at the offending statement or
declaration, not the document start).

## JSON Diagnostics (for LLM self-correction)

Every diagnostic is emitted as structured JSON (`xz check-json`) in addition to human-readable text:

```json
{
  "version": 1,
  "severity": "error",
  "code": "I0020",
  "message": "declared @effects 'none' does not match derived effects 'io' on 'f'",
  "category": "intent",
  "span": { "file": "src/main.xz", "start": [4, 6], "end": [4, 7] },
  "suggestion": { "fix": "extend @effects on 'f' to include 'io'", "confidence": 0.9 }
}
```

### Diagnostic guarantees

- **Stable error codes** — codes are never renumbered or reused for different errors
- **Machine-readable spans and categories** — deterministic, queryable
- **Round-trip loop** — an AI tool reads `code` + `span` + `message`, applies a fix, re-runs. This is the self-correction loop that makes "AI-written, human-reviewed" practical.
- **Intent diagnostics** — codes `I0003` (misplaced `@trusted`), `I0004` (untrusted claim in `--strict`), `I0020` (undeclared effect), `I0021` (NL claim without a paired formal contract), `I0022` (missing intent comment), `I0023` (missing `@effects`), `I0024` (unknown effect label) power the truthfulness check in [09-intent-verification.md](09-intent-verification.md)

### Initial code scheme

| Prefix | Phase | Meaning |
|---|---|---|
| `L0001` | lexer | lexical error |
| `P0001` | parser | syntax error |
| `R0001` | name resolution | unknown/duplicate name |
| `T0001` | type checker | type error |
| `Ixxxx` | intent verification | claim/effect/trust violations (above) |

Suggested fixes with confidence scores are part of the schema:
`"suggestion": { "fix": "...", "confidence": 0.9 }`. Today the intent
diagnostics carry them (e.g. I0020 proposes the corrected `@effects` list);
other phases adopt them as their error sites gain repair templates.

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