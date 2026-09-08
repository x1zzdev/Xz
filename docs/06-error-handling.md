# Error Handling

## Model: Result types on a single error channel

Xz has **no exceptions**. Every function that can fail returns a `Result`:

```
func sqrt(x: Float) -> Result[Float, DomainError]
    pre x >= 0
{
    ...
}
```

## The single error channel

All error propagation flows through an explicit, typed return channel. There is no way for an error to escape a function without appearing in its signature.

## Key types

| Type | Meaning |
|---|---|
| `Result[T, E]` | either `ok(value)` or `err(e)` |
| `Err` | the root error type; functions may declare narrower error types |
| `Option[T]` | optionality — absence is **not** an error |

## Using results

```
let r = sqrt(-1.0)
match r {
    ok(v)  -> print(v)
    err(e) -> report(e)
}
```

## Propagation that stays visible

The `?` operator propagates a `Result` to the caller **if and only if** the caller's declared error channel can accept it. The propagation path remains visible in the signature:

```
func validate_and_sqrt(x: Float) -> Result[Float, ValidationError | DomainError] {
    let v = validate(x)?
    sqrt(v)?
}
```

`?` is the only propagation operator. There is no implicit propagation.

## The `?` acceptance rule (precise)

`expr?` — where `expr: Result[T, E_callee]` — is legal **only if** the caller's
declared error channel `E_caller` accepts `E_callee`:

1. `E_caller == E_callee` — identical type.
2. `E_caller` is an error union `E1 | E2 | ...` and `E_callee` is one of its
   members (or itself a union of members).
3. `E_caller == Err` — the root type accepts any narrower error type.

Otherwise the call must be handled locally with `match`; propagating it is a
compile error, because it would widen the caller's signature without a visible
declaration.

```
func a() -> Result[Int, IoError] { ... }
func b() -> Result[Int, ParseError] { ... }

func c() -> Result[Int, IoError | ParseError] {
    a()?        // ok: IoError is a member of the union
    b()?        // ok: ParseError is a member of the union
    ...
}

func d() -> Result[Int, IoError] {
    a()?        // ok
    b()?        // ERROR: ParseError not in {IoError}; handle locally
}
```

In the success case, `expr?` evaluates to the unwrapped `T`.

## Contract failures vs runtime errors

- **Contract failures** (`pre`/`post`/`invariant` violations) are compile-time checkable where provable. Otherwise they are treated as logic bugs and reported as structured diagnostics — they are never caught-and-handled runtime events.
- **Runtime errors** (I/O, network, encoding) use `Result`.

## Why not exceptions?

- Exceptions are an implicit, invisible control-flow path. A reviewer cannot see all the ways a function can exit.
- `Result` makes every failure path explicit, typed, and reviewable — aligned with the core thesis of the language.

## Review value

A reviewer reads only the signature to know:

- What can fail: the `E` in `Result[T, E]`
- How it propagates: every `?` at a call site
- What is absent vs. what is an error: `Option[T]` vs `Result[T, E]`