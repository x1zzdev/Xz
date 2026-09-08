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
func deg_to_rad(deg: Float) -> Float
    post result == deg * PI / 180.0
{
    deg * PI / 180.0
}
```

| Tag | Meaning | Checked how? |
|---|---|---|
| `@intent` | Prose description of behavior (for humans and AI) | Advisory — read by reviewers, never machine-gated |
| `@requires` | Entry conditions in natural language | **Paired** with a `pre`, in order |
| `@ensures` | Exit guarantees in natural language | **Paired** with a `post`, in order |
| `@effects` | Declared side-effect profile: `none` / `mut` / `io` / `chan` / `extern` | **Auto-derived** from the body and compared |
| `@trusted` | Human-review stamp on one claim line | Discharges the proof obligation of the paired `pre`/`post` |

Natural-language text is **never parsed**. The compiler checks three separable
things: the *pairing* (structure), the *formal contracts* (provability), and
the *derived effects* (behavior). The NL text is for the human reviewer; the
paired formal line is the machine's obligation.

## Claim checking

1. **Structural pairing.** Every `@requires` must have a `pre` and every
   `@ensures` a `post`, **paired in order** (the k-th NL claim pairs with the
   k-th formal contract). A formal contract without an NL claim is fine — it
   is still checked. An NL claim without a formal partner is an error
   (`I0021`): the claim cannot be verified, so it must be either formalized or
   deleted.

   The mandatory element is the **intent comment** itself (`I0022` on every
   public `func`/`task` except `main`). Formal contracts are required *when a
   claim is made* — a public function that makes no claims declares no
   `pre`/`post`, and its type signature is then its entire contract.

2. **Formal proof.** Every `pre`/`post` is statically proven where provable.
   An unprovable claim emits `I0001`: in `xz build --strict` it must carry
   `@trusted` on the paired NL line, or the build fails. There is no runtime
   enforcement — a contract is either proven, trusted, or rejected.

3. **Effect derivation.** The compiler derives the actual effect profile of a
   function from its body (which `mut` bindings it touches, I/O calls, channel
   `send`/`recv`, `extern` calls), **transitively over calls** — a function's
   derived profile is the union of its own effects and its callees' (stdlib
   functions like `print` carry `@effects io`). A mismatch with the declared
   `@effects` is an error:

   ```
   /// @effects none
   func bump(mut n: Int) -> Int {        // ERROR I0020
       n += 1                             // effect 'mut' not declared
       n
   }
   ```

   The derived profile is surfaced in tooling (hover, `xz check --verbose`),
   so the reviewer answers "what can it change?" without reading the body.

## The trusted escape hatch

Rust has `unsafe`; Xz has `@trusted`:

```
/// @ensures result > 0        @trusted  // reviewed by human on 2026-09-08
func positive(x: Float) -> Float
    post result > 0
{
    ...   // compiler cannot prove result > 0; trusted instead
}
```

- `@trusted` is an **inline suffix** on the specific `@requires`/`@ensures`
  line it vouches for. It is a stamp, not a tag line: it always attaches to a
  claim, and a review note (`// reviewed by <who> on <date>`) is required.
- By order, the stamp transfers to the **paired formal claim**: the k-th
  `@trusted` discharges the proof obligation of the k-th `post` (or `pre`).
  The claim itself is not deleted from the reviewer's view — it is recorded as
  *human-reviewed*.
- In `xz build --strict`, **any unproven, untrusted claim blocks the build**.
- This creates the same safe/unsafe split as Rust, applied to *truthfulness*
  rather than memory safety.

## The entry point

`main` is the one exemption from the doc-comment requirement: its contract is
"run the program". Its canonical form is uniform with every other function:

```
func main() -> Result[Unit, Err] {
    let data = load_config()?       // Err accepts any narrower error (06, rule 3)
    ...
    ok()
}
```

- The payload is `Unit`, so the success constructor is the zero-argument
  `ok()`. `func main() -> Result[Int, Err] { ok(0) }` is **not** canonical:
  the program does not return an `Int` (see [03-type-system.md](03-type-system.md)).
- An `err` that reaches `main` is printed to stderr and exits non-zero — the
  only error path that is handled by the runtime, and it is visible in the
  signature.

## Why this is the differentiator

| Language | Catches | Xz adds |
|---|---|---|
| Rust | Type errors, memory unsafety | Untrue behavioral claims |
| Gleam / Go | Type errors, obvious bugs | Comment–code contract drift |
| Xz | All of the above | Proven or trusted claims only in strict builds |

This is not "Python syntax on Rust." It is a language whose compiler enforces that **what you say is what the code does** — the exact property AI-written code needs most.

## Feedback to the writer (AI)

- `I0001` — formal `pre`/`post` cannot be proven (planned: static provability); add `@trusted` to the paired NL claim, or strengthen/simplify the contract
- `I0003` — `@trusted` attached to the wrong tag (must be `@ensures`/`@requires`)
- `I0004` — `@trusted` without a review note, in `--strict`; add `// reviewed by <who> on <date>`
- `I0020` — declared `@effects` does not match the derived profile (transitively over calls)
- `I0021` — NL claim without a paired formal contract; write the `pre`/`post`
- `I0022` — missing intent comment on a public `func`/`task` (every top-level declaration except `main`)
- `I0023` — missing `@effects` declaration
- `I0024` — unknown effect label in `@effects` (allowed: `none`, `mut`, `io`, `chan`, `extern`)

All emitted as JSON diagnostics (see [07-compiler.md](07-compiler.md)).