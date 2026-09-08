# Type System

## Model

Strong, static, sound. There are no implicit conversions; a cast is an explicit operation (`as`). The type system is the primary instrument for human review: every contract boundary states its types, and every failure path is visible in a type.

## Contract-point explicitness

- At **contract boundaries** — function signatures, record fields, channel payload types, public API — types are **mandatory**.
- Inside **function bodies**, local variables may omit types and rely on inference.
- Rationale: the reviewer reads the boundary to understand the contract; the AI writer gets brevity inside the body.

```
func area(shape: Shape) -> Float {          // signature: types mandatory
    let r = 3.14159 * shape.radius           // body: inference allowed
    r * r
}
```

## Primitive types

`Bool`, `Int` (64-bit signed), `usize` (native unsigned, index/size type),
`Float` (IEEE-754 double), `Char`, `Str`, `Bytes`.

- `Int` is the default integer; `usize` appears only where the platform
  requires it (FFI, indexing, sizes).
- Narrowing `Int` → `usize` (and vice versa) requires explicit `as` and is
  checked at compile time where provable.

## Special types

| Type | Meaning |
|---|---|
| `Ptr` | opaque pointer; usable only in `extern` signatures and thin wrappers. No arithmetic, no dereference outside a wrapper. |
| `Unit` | the unit type — exactly one value. The payload of fallible functions that return nothing: `func main() -> Result[Unit, Err]` ends with `ok()`. |

`none` is the literal for absence — the only inhabitant of `Option[T]`'s
absence case. Constructors are the only way to produce or consume option/
result values; they are part of the language, not sugar:

- `ok(value)` / `err(e)` — constructors for `Result[T, E]`
- `none` — absence value; `Option[T]` is `T \| none`

## Composite types

| Type | Kind | Notes |
|---|---|---|
| `record` | product type, named fields | value semantics |
| `enum` | sum type, tagged variants | exhaustively matchable |
| `Option[T]` | `T \| none` | absence is not an error |
| `Result[T, E]` | value or error | failure paths are typed |
| `List[T]`, `Map[K, V]`, `Set[T]` | collections | value types, copy semantics |
| `Chan[T]` | typed channel | exactly one declared payload type |

`Map` and `Set` preserve **insertion order** on iteration. Iteration order is
part of the type contract: a reviewer must be able to predict the sequence of
values a loop produces, so no hash-order nondeterminism is allowed into
observable behavior.

## Error channel

`Result[T, E]` carries exactly one error type `E`. A function that can fail in
more than one way declares an **error union**:

```
func load(path: Str) -> Result[Config, IoError | ParseError] { ... }
```

- `Err` is the root error type; any narrower error type is accepted where
  `Err` is declared.
- `E1 | E2` is a compile-time sum: the function may return either. The
  `?` propagation rule (see [06-error-handling.md](06-error-handling.md)) uses
  this union to decide whether a callee's error fits the caller's channel.

## No implicit conversions

- No integer/float auto-promotion
- No boolean coercion (no truthiness of `0` or `""`)
- Narrowing requires explicit `as` and is checked at compile time where provable

```
let i: Int = 7
let f: Float = i as Float     // explicit, visible
```

## Generics

Generic records and functions, with constraints:

```
func max[T: Ordered](a: T, b: T) -> T {
    if a > b { a } else { b }
}
```

## Units & domain types (planned)

Unit-typed numbers (`Meters`, `Seconds`) make review stronger: arithmetic across incompatible units is a compile error unless explicitly converted.

```
let distance: Meters = 100
let time: Seconds = 10
let speed: MetersPerSecond = distance / time   // ok
// distance + time                             // compile error: unit mismatch
```

## Type checking pipeline

1. Parse → AST with annotations
2. Name resolution
3. Constraint collection
4. Unification / inference (bounded to function bodies)
5. Contract checking (pre/post/invariant validity)
6. Exhaustiveness checks (match statements, error channels)

## Review value

A reviewer reading only the signature of a function knows:

- What it takes (parameter types)
- What it returns (return type)
- What can fail (`Result[T, E]` error channel)
- What it guarantees (`pre`/`post` contracts)
- What it can change (`mut` parameters)