# Syntax

Xz uses Python-like syntax with **brace-delimited blocks**. Indentation (4 spaces) is a mandatory layout convention, enforced by the formatter rather than the parser. There are no semicolons — a newline terminates a statement. The authoritative grammar is [11-grammar.md](11-grammar.md).

## Lexical conventions

- 4-space indentation (no tabs)
- `//` line comments, `/* */` block comments
- Values: `lowerCamelCase` identifiers; Types: `PascalCase`
- Strings: `"..."` with escapes; raw strings `` r"..." ``
- No semicolons; a newline terminates a statement

## Literals and declarations

Values are immutable by default (`let`); mutation is explicit (`mut`):

```
let x: Int = 42
let name: Str = "xz"
let pi: Float = 3.14
let flag: Bool = true
let maybe: Option[Str] = none

mut counter: Int = 0
counter += 1
```

Primitive types: `Bool`, `Int` (64-bit), `Float` (IEEE-754 double), `Char`, `Str`, `Bytes`.

## Records and enums

```
record Point {
    x: Float
    y: Float
}

enum Shape {
    circle(radius: Float)
    rect(width: Float, height: Float)
}
```

## Functions

Signatures must declare types for all parameters and the return. Inside the body, local inference is allowed.

```
func area(shape: Shape) -> Float {
    match shape {
        circle(r) -> 3.14159 * r * r
        rect(w, h) -> w * h
    }
}
```

## Contracts (Design by Contract)

Public functions may declare pre/post conditions. They are part of the signature and are checked:

```
func divide(a: Float, b: Float) -> Float
    pre  b != 0
    post result * b == a
{
    a / b
}
```

- `pre <expr>` — precondition (must hold on entry)
- `post <expr>` — postcondition (`result` refers to the return value)
- `invariant <expr>` — on records/loops, must hold at all times

Formal contracts are required **when a claim is made**: an `@requires`/`@ensures`
NL claim must be paired with a `pre`/`post` (see
[09-intent-verification.md](09-intent-verification.md)). A public function
that makes no claims declares no contracts — the type signature is then its
entire contract, which the reviewer sees as the absence of guarantees.

## Error-returning functions

Functions that can fail declare the error channel in the return type:

```
func read_file(path: Str) -> Result[Str, IoError]
    pre path != ""
{
    ...
}
```

## Intent comments

Public functions require a structured doc comment whose claims are checked against the code (see [09-intent-verification.md](09-intent-verification.md)):

```
/// Converts degrees to radians.
/// @intent  Returns the radian equivalent of the input angle.
/// @ensures result == deg * PI / 180.0
/// @effects none
func deg_to_rad(deg: Float) -> Float
    post result == deg * PI / 180.0
{
    deg * PI / 180.0
}
```

- `@intent` — natural-language description (for humans and AI); advisory, never machine-gated
- `@requires` / `@ensures` — NL claims, each paired (in order) with a `pre`/`post`
- `@effects` — declared side-effect profile (`none`/`mut`/`io`/`chan`/`extern`), auto-derived and compared
- `@trusted` — human-review stamp appended to a specific `@ensures`/`@requires` line; discharges the proof obligation of the paired formal contract in strict builds (see [09-intent-verification.md](09-intent-verification.md))

## FFI / extern

```
extern func malloc(size: usize) -> Ptr
extern func free(ptr: Ptr)

/// Allocates a buffer; every raw FFI call sits behind a contracted wrapper.
/// @intent  Allocates size bytes; ok(Buffer) on success, err on null.
/// @ensures result is ok implies result.value.ptr != 0
/// @effects extern
func alloc_buffer(size: Int) -> Result[Buffer, AllocError]
    pre  size > 0
    post result is ok implies result.value.ptr != 0
{
    let p = malloc(size as usize)
    if p == 0 { err(AllocError()) } else { ok(Buffer(p, size)) }
}
```

Raw FFI is an escape hatch; safe use is always through contracted wrappers (see [10-ffi-interop.md](10-ffi-interop.md)).

## Control flow

```
if x > 0 {
    print("positive")
} elif x == 0 {
    print("zero")
} else {
    print("negative")
}

loop {
    // ...
    break
}

for item in items {
    // ...
}
```

> **Phase 4 scope.** The backend implements `loop { ... }` with `break` /
> `continue`, and `for i in n { ... }` iterating the integer range `0..n`
> (n exclusive, `n: Int`). Collection iteration (`for item in items`) awaits
> the Phase 7 collections stdlib; the type checker rejects non-Int iterables
> for now (see [13-codegen.md](13-codegen.md)).

## Concurrency

```
chan work: Chan[Job]
chan done: Chan[Result[JobId, Err]]

async func fetch(url: Str) -> Result[Str, HttpError] {
    ...
}

task worker {
    loop {
        let job <- recv(work)     // receive (blocks)
        let r = execute(job)
        send(done, r)
    }
}
```

## Grammar

The full, authoritative grammar — lexical rules, operator precedence, EBNF,
intent-comment grammar, and well-formedness constraints — is specified in
[11-grammar.md](11-grammar.md). That document is the authority for which
constructs exist; examples elsewhere in this repo must match it.

The surface in one line each:

- **Declarations** — `let`/`mut` bindings, `chan`, `record`, `enum`, `extern`
- **Functions** — `func` (optionally `async`), mandatory signature types,
  `pre`/`post`/`invariant` contracts, brace block
- **Tasks** — `task` + brace block
- **Expressions** — `if`/`elif`/`else`, `match`, `loop`, `for..in`,
  `send`/`recv`, `await`, `?`, `as`, `ok`/`err`/`some`/`none`, `transfer`
- **Intent comments** — `/// @intent`/`@requires`/`@ensures`/`@effects` with
  inline `@trusted`

## Canonical syntax rule

For every intent there is exactly one idiomatic expression:

- Assignment of a new binding → `let`
- Mutating an existing binding → `mut` + operator
- Optional value → `Option[T]`
- Present option value → `some(v)`; absent → `none`
- Fallible call → `Result[T, E]`
- Propagating a fallible call → `?` at the call site
- Multiple failure modes → error union `Result[T, E1 | E2]`
- Type conversion → explicit `as`
- Absence → `none`
- Contract implication → `implies` (contract expressions only)
- Fallible function that returns nothing → `Result[Unit, E]`, success value `ok()`
- Formatting a value into text → `value.to_str()` (not `as Str` — `as` is a cast, and an `Int` is not a `Str`)
- Communication → `send` / `recv` on a `Chan[T]`
- Suspension → `await` on an `async` call
- Handing off a handle (`Ptr`-bearing record) → `transfer(x)`; handles are never copied (see [10-ffi-interop.md](10-ffi-interop.md))

No two ways to express the same thing. This is what makes AI-generated code predictable to review.