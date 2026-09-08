# Syntax

Xz uses Python-like, indentation-based syntax. Blocks are delimited by indentation; there are no braces or semicolons.

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

## Grammar sketch (outline)

A simplified outline; the full grammar is specified in Phase 1. It is the
authority for which constructs exist; examples elsewhere in this repo must
match it.

```
program      := statement*
statement    := decl | func | task | chan_decl | extern_decl
             | record_decl | enum_decl | expr | contract
block        := indented statement+

// declarations
decl         := ("let" | "mut") IDENT ":" type ("=" expr)?
chan_decl    := "chan" IDENT ":" "Chan[" type "]"
extern_decl  := "extern" "func" IDENT "(" params ")" ("->" type)?

// user-defined types
record_decl  := "record" IDENT "{" field+ "}"
enum_decl    := "enum" IDENT "{" variant+ "}"
field        := IDENT ":" type
variant      := IDENT "(" (field ("," field)*)? ")"

// functions
func         := ("async")? "func" IDENT "(" params ")" ("->" type)? contract* block
params       := param ("," param)*
param        := ("mut")? IDENT ":" type
contract     := ("pre" | "post" | "invariant") expr

// tasks
task         := "task" IDENT block

// types
type         := prim | IDENT | IDENT "[" type ("," type)* "]" | type "|" type
prim         := "Bool" | "Int" | "usize" | "Float" | "Char" | "Str" | "Bytes"
             | "Unit" | "Ptr" | "none"

// expressions (selected)
expr         := literal | IDENT | "match" expr "{" match_arm+ "}"
             | "if" expr block ("elif" expr block)* ("else" block)?
             | "loop" block | "for" IDENT "in" expr block
             | call | "send" "(" expr "," expr ")" | IDENT "<-" "recv" "(" expr ")"
             | "await" call | call "?" | expr "as" type
             | "ok" "(" (expr)? ")" | "err" "(" expr ")" | "none"
             | "transfer" "(" expr ")"
match_arm    := pattern "->" expr
pattern      := IDENT | IDENT "(" (IDENT ("," IDENT)*)? ")"

// intent comments (public funcs/tasks, except main)
intent       := "///" "intent"  NL_TEXT
             | "///" "@requires" NL_TEXT trusted?
             | "///" "@ensures"  NL_TEXT trusted?
             | "///" "@effects"  effect_list
trusted      := "@trusted" "//" "reviewed by" IDENT "on" DATE   // inline suffix, required note
effect_list  := "none" | ("mut" | "io" | "chan" | "extern") ("," effect_list)?
```

The full grammar adds precedence for `as`/`?`/calls and match-arm pattern
syntax (`circle(r) -> expr`); the outline above fixes the set of constructs,
which is what the reviewer needs.

## Canonical syntax rule

For every intent there is exactly one idiomatic expression:

- Assignment of a new binding → `let`
- Mutating an existing binding → `mut` + operator
- Optional value → `Option[T]`
- Fallible call → `Result[T, E]`
- Propagating a fallible call → `?` at the call site
- Multiple failure modes → error union `Result[T, E1 | E2]`
- Type conversion → explicit `as`
- Absence → `none`
- Fallible function that returns nothing → `Result[Unit, E]`, success value `ok()`
- Formatting a value into text → `value.to_str()` (not `as Str` — `as` is a cast, and an `Int` is not a `Str`)
- Communication → `send` / `recv` on a `Chan[T]`
- Suspension → `await` on an `async` call
- Handing off a handle (`Ptr`-bearing record) → `transfer(x)`; handles are never copied (see [10-ffi-interop.md](10-ffi-interop.md))

No two ways to express the same thing. This is what makes AI-generated code predictable to review.