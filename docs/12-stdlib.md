# Standard Library — Minimum Surface

Status: this is the **minimum surface** needed to typecheck the example
programs and to validate Phases 1–3, plus the first collection slices
(`List[T]`, the `Map[K, V]` first slice, and the `Set[T]` first slice), the
first file-I/O slice (`read_file`), and the first clock slice (`time`). The
rest of the standard library (`Set` removal, networking, the remainder of I/O)
is Phase 7 on the [roadmap](08-roadmap.md). Anything not listed here does not
exist yet.

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

## `List[T]`

An ordered, immutable-element sequence. Value type: assignment/argument/return
copies it; `xs.append(x)` returns a *new* list (see
[03-type-system.md](03-type-system.md)).

| Construct | Meaning |
|---|---|
| `[e1, e2, ...]` | list literal (in order) |
| `xs[i] -> Result[T, IndexError]` | 0-based, bounds-checked indexing |
| `xs.len() -> Int` | element count |
| `xs.is_empty() -> Bool` | |
| `xs.append(x: T) -> List[T]` | a new list with `x` appended |
| `for x in xs { ... }` | iterate elements in order |

`[]` (empty) requires a declared element type: `let xs: List[Int] = []`.
There is no index assignment and no `push`; growth is the explicit, value-returning
`append`.

## `Map[K, V]`

An insertion-ordered association collection with immutable entries. Value type:
assignment/argument/return copies it; `m.insert(k, v)` returns a *new* map
(see [03-type-system.md](03-type-system.md)).

| Construct | Meaning |
|---|---|
| `{k1: v1, k2: v2, ...}` | map literal, in insertion order |
| `m.len() -> Int` | entry count |
| `m.is_empty() -> Bool` | |
| `m.get(k: K) -> Option[V]` | `none` when the key is absent |
| `m.insert(k: K, v: V) -> Map[K, V]` | a new map; replaces the value if `k` is present, keeping its position |
| `m.keys() -> List[K]`, `m.values() -> List[V]` | snapshots in insertion order |

Keys must be `Int`, `usize`, `Bool`, `Char`, or `Str` (types with decidable
equality); `Float`, records, enums, and collections are rejected. A repeated key
in a literal is the same as inserting again: the later value wins and the key
keeps its first position. `{}` (empty) requires a declared key and value type:
`let m: Map[Str, Int] = {}`. There is no `m[k]` indexing and no index
assignment; `get` returns `Option`, and changes go through `insert`.

## `Set[T]`

An insertion-ordered collection of distinct elements with immutable membership.
Value type: assignment/argument/return copies it; `s.insert(e)` returns a *new*
set (see [03-type-system.md](03-type-system.md)).

| Construct | Meaning |
|---|---|
| `{e1, e2, ...}` | set literal, in first-insertion order; a repeated element is a no-op |
| `s.len() -> Int` | element count |
| `s.is_empty() -> Bool` | |
| `s.contains(e: T) -> Bool` | membership |
| `s.insert(e: T) -> Set[T]` | a new set with `e` added; a no-op when `e` is present |
| `for e in s { ... }` | iterate elements in insertion order |

Elements must be `Int`, `usize`, `Bool`, `Char`, or `Str` (types with decidable
equality); `Float`, records, enums, and collections are rejected. A repeated
element in a literal keeps its first position. `{}` (empty) requires a declared
element type: `let s: Set[Int] = {}`. There is no `remove` yet and no in-place
mutation; changes go through `insert`.

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
| `read_file(path: Str) -> Result[Str, IoError]` | `@effects io`; the whole file decoded as UTF-8. `err` when the path cannot be read or its bytes are not valid UTF-8. The `Str` payload is a fresh heap buffer, freed by the same rules as `concat`/`to_str` results |

## `time`

Clock reads. Both return seconds as `Float` and carry `@effects io` — they read
the host clock, so a function that calls them declares `io`. There is no
`Instant`/`Duration` type yet; the unit is seconds, as with `math`.

| Signature | Notes |
|---|---|
| `now() -> Float` | wall-clock time as seconds since the Unix epoch (UTC), fractional. May jump forwards or backwards (NTP, manual clock changes); use it for timestamps, not for measuring durations |
| `monotonic() -> Float` | seconds from the host's monotonic clock (`CLOCK_MONOTONIC`, system boot) that never decreases; use it for measuring elapsed time. Only differences are meaningful, never the absolute value. The JIT and native paths read the same clock, so the origin does not depend on the process |

## Out of scope (Phase 7)

`Set` removal, networking (HTTP), ranges, `Instant`/`Duration` types, and any
module structure. The grammar reserves their syntax; nothing provides it yet.
`List`, `Map`, `Set` (above), `read_file`, and `time` are the collections and
I/O so far.