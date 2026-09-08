# Memory Model

## Value semantics

Values are copied by default. Assigning a variable, passing an argument, or returning a value copies the data. There is **no hidden aliasing** — two variables never silently share mutable storage.

```
let a: Point = Point(1.0, 2.0)
let b = a                    // copy, not alias
b.x = 5.0                    // only b changes
```

## Immutable by default

`let` binds an immutable value. `mut` binds a mutable binding:

```
let  a: Int = 1
mut  b: Int = 1
b += 1        // ok
// a += 1    // compile error: a is immutable
```

## Copy rules

- Small values (primitives, records of primitives): copied eagerly.
- Large values (`List`, `Str`, `Bytes`): **copy-on-write** is the implementation strategy; the *semantics* remain value-copy. Deterministic and observable as pure value semantics.
- No pointers, no references, no aliasing escape hatches.

## Explicitness of mutation

Any code that changes program state must do one of:

1. Bind a `mut` variable, or
2. Pass a value through an explicit `mut` parameter, or
3. Use a visibly side-effecting construct (`send` on a channel, I/O functions).

```
mut acc: Int = 0

func add_to(mut acc: Int, n: Int) {
    acc += n
}

add_to(acc, 5)        // ok: mut parameter is visible at the call site
```

## Why not GC / ownership?

- A **GC** hides allocation and mutation behind runtime behavior, weakening reviewability.
- **Ownership/borrowing** (Rust-style) is powerful, but its borrow rules add cognitive load that conflicts with the "one canonical syntax" goal.
- Value semantics gives deterministic, easily-reasoned-about behavior at acceptable performance, with copy-on-write covering large structures.

## Data-race freedom

Because tasks never share mutable state (see [05-concurrency.md](05-concurrency.md)), data races are impossible **by construction**, not by proof.

## Determinism

The same input produces the same sequence of value transitions. There is no observer-visible nondeterminism from memory layout, allocation order, or scheduling of value copies.