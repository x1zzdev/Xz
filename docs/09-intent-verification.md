# Intent Verification — Xz's Differentiator

## Problem

A reviewer's deepest fear about AI-generated code is not syntax errors — it is the AI *lying*: a comment that says one thing while the code does another. Static typing catches type mismatches, but nothing catches **claims about behavior** that do not hold.

## Design: "No unverified claims"

Xz makes the *contract between comment and code* verifiable. Every documented claim is either **proven by the compiler**, **explicitly trusted by a human**, or **rejected** in strict builds.

## Intent comments

Public functions require a structured doc comment:

```
/// Converts degrees to radians.
/// @intent  Returns the radian equivalent of the input angle.
/// @ensures result == deg * PI / 180.0
/// @effects none
func deg_to_rad(deg: Float) -> Float {
    deg * PI / 180.0
}
```

| Tag | Meaning | Verified? |
|---|---|---|
| `@intent` | Natural-language description of behavior (for humans and AI) | Advisory — flagged `I0001` if not mirrored by formal claims |
| `@requires` | Entry conditions in natural language | Must be mirrored by `pre`; else warning |
| `@ensures` | Exit guarantees in natural language | Must be mirrored by `post`; else warning |
| `@effects` | Declared side-effect profile: `none` / `mut` / `io` / `chan` / `extern` | **Automatically derived** from the body and compared |

## Claim checking

1. **Formal contracts** (`pre`/`post`/`invariant`) are statically checked where provable.
2. **Derived effects**: the compiler derives the actual effect profile of a function from its body (which `mut` bindings it touches, I/O calls, channel `send`/`recv`, `extern` calls) and compares it with `@effects`. A mismatch is an error:

   ```
   /// @effects none
   func bump(mut n: Int) -> Int {        // ERROR I0020
       n += 1                             // effect 'mut' not declared
       n
   }
   ```

3. **Unprovable claims**: if `@ensures` contains a claim the compiler cannot prove, it emits `I0001` (advisory) — unless the claim carries `@trusted`.

## The trusted escape hatch

Rust has `unsafe`; Xz has `@trusted`:

```
/// @ensures result > 0        @trusted  // reviewed by human on 2026-09-08
func positive(x: Float) -> Float { ... }
```

- `@trusted` is an **inline suffix** on the specific `@requires`/`@ensures`
  line it vouches for. It is a stamp, not a tag line: it always attaches to a
  claim, and a review note (`// reviewed by <who> on <date>`) is required.
- `@trusted` marks that claim as human-reviewed. In `xz build --strict`, **any
  unproven, untrusted claim blocks the build**.
- This creates the same safe/unsafe split as Rust, applied to *truthfulness*
  rather than memory safety.

## Why this is the differentiator

| Language | Catches | Xz adds |
|---|---|---|
| Rust | Type errors, memory unsafety | Untrue behavioral claims |
| Gleam / Go | Type errors, obvious bugs | Comment–code contract drift |
| Xz | All of the above | Proven or trusted claims only in strict builds |

This is not "Python syntax on Rust." It is a language whose compiler enforces that **what you say is what the code does** — the exact property AI-written code needs most.

## Feedback to the writer (AI)

- `I0001` — claim not provable; add `@trusted` or strengthen the `post`
- `I0020` — undeclared effect; add `@effects mut` or remove the mutation
- `I0021` — `@ensures` has no matching `post`; write the formal claim

All emitted as JSON diagnostics (see [07-compiler.md](07-compiler.md)).