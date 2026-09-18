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

Shared-library output (`xz build --shared`) is implemented: by default it emits
`libXz.so` + a generated `libXz.h` for the `@export` functions (`--out <path>`
renames the shared object and the header follows beside it); `xz bind --lang python`
and `xz pkg gen --lang python` generate Python wrappers from Xz sources and
`.xzint` interface files respectively, and `xz pkg add` fetches and verifies an
interface file from a registry (see
[10-ffi-interop.md](10-ffi-interop.md)).

## CLI

```
xz build <file.xz>          # type check + contract check + codegen (Phase 4: emits LLVM IR)
xz build --shared <file.xz> # emit libXz.so + libXz.h for the @export functions
xz build --shared --out <path> <file.xz>  # same, but write <path> + a sibling .h
xz build-native <file.xz>   # emit IR + native runtime, compile with llc, link with ld -> ./xz_program
xz bind --lang python <file.xz>  # emit a ctypes wrapper (<stem>.py) for the @export functions
xz pkg gen --lang python [--lib <name>] <file.xzint>  # emit a ctypes wrapper (<stem>.py) for an extern interface
xz pkg add <name> [--registry <base_url>]            # fetch + verify <name>.xzint from a registry
xz check <file.xz>          # type/contract check only, no codegen
xz check --strict <file.xz> # intent checks enforced (I0004: untrusted claims fail)
xz check-json [--strict] <file.xz>   # same, diagnostics as a JSON array
xz run <file.xz>            # build and JIT-execute
xz fmt <file.xz>            # parse and print the canonical layout on stdout
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
  `textDocument/hover`, `textDocument/completion`, `textDocument/definition`,
  `textDocument/formatting`. Diagnostics are published on
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
- **Definition:** go-to-definition on an identifier that names a top-level
  symbol returns the `Location` of its declaration (`func`, `task`, `chan`,
  `extern`, `record`, `enum`, or enum variant); anything else is `null`. Like
  hover, resolution is by name within the current document and requires the
  document to parse.
- **Formatting:** `textDocument/formatting` runs the `xz fmt` engine and returns
  a single `TextEdit` that replaces the whole document with its canonical
  layout. A document that is not open, or does not parse, returns `null`; the
  server never applies edits (the client owns that), matching the read-only
  contract of `xz fmt`.
- **Positions:** 1-based Xz spans are converted to 0-based UTF-16 code units
  (the LSP default; advertised as `positionEncoding: "utf-16"`).
- **Exit code:** 0 after a `shutdown` request, 1 otherwise (per the LSP spec).

Type errors carry a real span (`T0001` points at the offending statement or
declaration, not the document start).

## Formatter (`xz fmt`)

`xz fmt <file.xz>` parses the file and prints its canonical layout on stdout.
It is read-only: unlike `gofmt -w` it never rewrites the file, so an editor
owns applying the result. A file that does not parse is rejected with the same
parse error `xz check` reports; the formatter never emits output for it.

The formatter is an **AST pretty-printer**: it prints from the parsed program,
not from the token stream. That is what makes the layout deterministic, but it
means the AST must carry everything the output needs. Two facts shape the
first slice:

- **Comments are the source of truth.** The lexer also returns every line
  comment, block comment, and `///` intent comment in source order. The printer
  emits them verbatim (doc claims are not re-rendered from the AST, which
  cannot distinguish a prose line from an `@intent` line).
- **Placement is by source position.** Before printing each top-level
  declaration or statement, the printer flushes any comment that starts before
  it, as its own line at that indentation; a comment that starts on the same
  line as a statement's end is kept trailing, separated by two spaces. This
  needs a start span for every statement, which is why statements carry one.

Canonical layout, fixed here (the "mandatory layout convention" of
[11-grammar.md](11-grammar.md)):

- 4 spaces per brace level; no tabs, no trailing whitespace, one final newline.
- `func`/`task` open their block on the signature line. A declaration with
  contracts prints the signature, then one contract per line (indent + 1), then
  `{` on its own line at the declaration's indent.
- `record`/`enum` open on the declaration line; fields/variants are one per
  line at indent + 1.
- Binary operators are surrounded by spaces; `,` is followed by one space;
  `:` in a type, field, or map entry is followed by one space; unary `-`/`not`
  bind tight.
- `if`/`match`/`loop`/`for` are laid out across lines; `elif`/`else` continue
  the closing `}` line (`} elif cond {`). Every other statement and top-level
  declaration is one line.
- Redundant parentheses are dropped; parentheses required by precedence are
  re-inserted from the AST.

Known limits of the first slice (tracked as follow-ups): block comments that
span lines are emitted verbatim; a comment inside a multi-line expression is
attached to the enclosing statement because expressions do not carry spans
yet; and integer literals are re-emitted in decimal because the AST keeps only
their value, not the original radix.

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