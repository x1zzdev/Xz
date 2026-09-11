# Examples

Design-validation programs. They exist to validate ergonomics and to give the
implementation a concrete target.

**Runnable** (Phase 4): `hello.xz`, `contracts.xz`, `ffi.xz`, and `lists.xz`
pass the full front end and execute via the LLVM JIT (and, for these, the
native path):

```
cd ../xz-cli
cargo run -- run ../examples/hello.xz      # Hello, Xz!length: 10
cargo run -- run ../examples/contracts.xz  # distance/final x/area
cargo run -- run ../examples/ffi.xz        # capacity: 16
cargo run -- run ../examples/lists.xz      # sum/grown/first/empty first
```

`concurrency.xz` typechecks but is **not runnable yet**: the concurrency
runtime is Phase 6.

| File | Demonstrates |
|---|---|
| `hello.xz` | Minimal program; `@intent`/`@ensures`/`@effects` on a public function |
| `contracts.xz` | `record`/`enum`, `match`, `pre`/`post`, `Result` + `?`, error unions (`E1 \| E2`) |
| `concurrency.xz` | `task`, typed channels `Chan[T]`, `send`/`recv`, deterministic completion |
| `ffi.xz` | `extern` declarations, `Ptr`/`usize`, handle types, `transfer` |
| `lists.xz` | `List[T]` literals, bounds-checked `xs[i]`, `append`, `for x in xs` |

Conventions used throughout:

- Public `func`/`task` declarations always carry a `///` doc comment with
  `@intent`, `@ensures`, and `@effects` — the intent verification contract
  (see [docs/09-intent-verification.md](../docs/09-intent-verification.md)).
- `@ensures`/`@requires` NL claims are paired (in order) with a `post`/`pre`;
  `@trusted` appears inline on the claim line it vouches for.
- `main` is the one exemption from the doc-comment rule and returns
  `Result[Unit, Err]`, ending in `ok()` — never `Result[Int, Err]`/`ok(0)`.
- Formatting uses `value.to_str()`, never `as Str` (`as` is a cast).
- Raw FFI (`extern`) never appears outside a thin, contracted wrapper.
- Handles (`Ptr`-bearing records) are never copied; handoff uses
  `transfer(x)`, which marks the source binding dead.
- `?` is used only on calls whose error channel fits the caller's declared
  error channel (`Err` accepts any; unions accept their members).

If an example needs a language feature that is not yet specified in the docs,
the gap must be filed in `docs/` rather than silently invented here. The
authorities are [docs/11-grammar.md](../docs/11-grammar.md) (which constructs
exist) and [docs/12-stdlib.md](../docs/12-stdlib.md) (which names exist).