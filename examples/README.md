# Examples

Design-validation programs. **Not runnable yet** — the language is in the design
phase (see [docs/08-roadmap.md](../docs/08-roadmap.md)). They exist to validate
ergonomics and to give the implementation a concrete target.

| File | Demonstrates |
|---|---|
| `hello.xz` | Minimal program; `@intent`/`@ensures`/`@effects` on a public function |
| `contracts.xz` | `record`/`enum`, `match`, `pre`/`post`, `Result` + `?`, error unions (`E1 \| E2`) |
| `concurrency.xz` | `task`, typed channels `Chan[T]`, `send`/`recv`, deterministic completion |
| `ffi.xz` | `extern` declarations, `Ptr`/`usize`, handle types, `transfer` |

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
the gap must be filed in `docs/` rather than silently invented here.