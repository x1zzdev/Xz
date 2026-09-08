# Examples

Design-validation programs. **Not runnable yet** — the language is in the design
phase (see [docs/08-roadmap.md](../docs/08-roadmap.md)). They exist to validate
ergonomics and to give the implementation a concrete target.

| File | Demonstrates |
|---|---|
| `hello.xz` | Minimal program; `@intent`/`@ensures`/`@effects` on a public function |
| `contracts.xz` | `record`/`enum`, `match`, `pre`/`post`, `Result` + `?`, error unions (`E1 \| E2`) |
| `concurrency.xz` | `task`, typed channels `Chan[T]`, `send`/`recv`, deterministic completion |
| `ffi.xz` | `extern` declarations, `Ptr`/`usize`, contracted wrappers over raw C ABI |

Conventions used throughout:

- Public functions always carry a `///` doc comment with `@intent`, `@ensures`,
  and `@effects` — the intent verification contract (see
  [docs/09-intent-verification.md](../docs/09-intent-verification.md)).
- Raw FFI (`extern`) never appears outside a thin, contracted wrapper.
- `?` is used only on calls whose error channel fits the caller's declared
  error channel.

If an example needs a language feature that is not yet specified in the docs,
the gap must be filed in `docs/` rather than silently invented here.