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
func deg_to_rad(deg: Float) -> Float {
    deg * PI / 180.0
}
```

- `@intent` — natural-language description (for humans and AI)
- `@requires` / `@ensures` — NL claims, must be mirrored by `pre`/`post`
- `@effects` — declared side-effect profile (`none`/`mut`/`io`/`chan`/`extern`), auto-derived and compared
- `@trusted` — human-review stamp; required for unprovable claims in strict builds

## FFI / extern

```
extern func malloc(size: usize) -> Ptr
extern func free(ptr: Ptr)

func alloc_buffer(size: Int) -> Result[Buffer, AllocError]
    pre  size > 0
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

A simplified outline; the full grammar is specified in Phase 1.

```
program      := statement*
statement    := decl | func | expr | contract | task | chan | extern
extern       := "extern" "func" IDENT "(" params ")" ("->" type)?
intent       := "///" ("@" ("intent" | "requires" | "ensures" | "effects" | "trusted")) ...
decl         := ("let" | "mut") IDENT ":" type ("=" expr)?
func         := "func" IDENT "(" params ")" ("->" type)? contract? block
params       := param ("," param)*
param        := IDENT ":" type
type         := prim | IDENT | IDENT "[" type ("," type)* "]"
contract     := ("pre" | "post" | "invariant") expr
block        := indented statement+
```

## Canonical syntax rule

For every intent there is exactly one idiomatic expression:

- Assignment of a new binding → `let`
- Mutating an existing binding → `mut` + operator
- Optional value → `Option[T]`
- Fallible call → `Result[T, E]`
- Communication → `send` / `recv` on a `Chan[T]`

No two ways to express the same thing. This is what makes AI-generated code predictable to review.