# Standard Library — Minimum Surface

Status: this is the **minimum surface** needed to typecheck the example
programs and to validate Phases 1–3. The full standard library (collections,
file I/O, networking, time) is Phase 7 on the [roadmap](08-roadmap.md).
Anything not listed here does not exist yet.

## Conventions

- Every stdlib function carries an intent comment; stdlib claims are
  `@trusted` by the Xz maintainers. The stdlib is the reference for what
  "trusted" means.
- `print` and I/O carry `@effects io`; `math` carries `@effects none`.
- No global mutable state. The only global bindings are the **immutable
  constants** below; a constant is not mutable state.
- There is no module system yet. All names below are global; namespacing is
  future work (Phase 8, `xz pkg`).
- Method syntax `x.method(...)` is the canonical call form for the surface
  below; method resolution is a Phase 2 type-system task.

## Core (language-provided)

| Construct | Meaning |
|---|---|
| `ok(v)` / `err(e)` | `Result[T, E]` constructors; `ok()` when `T` is `Unit` |
| `some(v)` / `none` | `Option[T]` constructors |
| `x is ok` / `x is err` / `x is none` | predicates, valid in contract expressions; narrow `x` |
| `x.to_str() -> Str` | every value type implements `to_str()` (built-in `Show`) |
| `Err` | root error type; constructible `Err(message: Str)` |

**Error records.** A record whose fields are exactly `{ message: Str }` (or
that ends with such a field) is an **error record** and automatically conforms
to `Err`. Constructor takes the message positionally:

```
record DomainError { message: Str }      // conforms to Err
err(DomainError("negative input"))
```

The stdlib predeclares the common ones: `IoError`, `DomainError`,
`ParseError`, `AllocError`, `IndexError`, `DecodeError`, `HttpError`.

## `Str`

| Signature | Notes |
|---|---|
| `s.len() -> Int` | character count |
| `s.is_empty() -> Bool` | |
| `s.to_upper() -> Str`, `s.to_lower() -> Str` | Unicode-aware |
| `s.at(i: Int) -> Result[Char, IndexError]` | bounds-checked; no `[]` yet |
| `s.to_bytes() -> Bytes` | UTF-8 encoding |

`+` on `Str` concatenates (the one built-in dual meaning of a symbol, fixed
by the language): `"job " + r.id.to_str()`.

## `Bytes`

| Signature | Notes |
|---|---|
| `b.len() -> Int` | |
| `b.to_str() -> Result[Str, DecodeError]` | UTF-8 decode |

## Numeric

| Signature | Notes |
|---|---|
| `Int.to_str() -> Str`, `Int.abs() -> Int` | |
| `Float.to_str() -> Str`, `Float.abs() -> Float` | |
| `Bool.to_str() -> Str` | `"true"` / `"false"` |
| `Char.to_str() -> Str` | single-char string |

## `Option[T]`

| Signature | Notes |
|---|---|
| `o.is_some() -> Bool`, `o.is_none() -> Bool` | for assertions; consumption is `match` |

There is deliberately **no `unwrap()`** — a panicking accessor would be an
untyped failure path. To use a value, `match` it; to assert it exists, use
`is some`.

## `math`

| Signature | Notes |
|---|---|
| `PI: Float`, `E: Float` | immutable constants |
| `approx_sqrt(x: Float) -> Float` | raw IEEE-754 square root; NaN on negative input. **Not for direct use** — wrap in a contracted function (see `examples/contracts.xz`) |

## `io`

| Signature | Notes |
|---|---|
| `print(x: Str) -> Unit` | `@effects io`; takes `Str` only — build your string with `+`/`to_str()` |

## Out of scope (Phase 7)

`List`, `Map`, `Set`, file I/O, networking (HTTP), time, ranges, collection
indexing `[]`, and any module structure. The grammar reserves their syntax;
nothing provides it yet.