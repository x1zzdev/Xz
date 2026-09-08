# Philosophy: AI-written, Human-reviewed

## The problem

AI models now generate production code at scale. The bottleneck has shifted from *writing* code to *verifying* it. A human reviewer looking at AI-generated code must answer:

- What does this code do?
- What can it change? (side effects)
- What are its guarantees? (contracts)
- What failure paths exist?

Most languages make these questions hard to answer because behavior is implicit. Xz inverts this: the language is designed for *reading*, not just writing.

## Core thesis

> Code should be written by machines, but designed for humans to read.

## Design principles

1. **Explicit over implicit**
   No implicit type conversions. No hidden magic. No operators that do "whatever the runtime decides." Every transformation is visible.

2. **One canonical syntax**
   For every intent, there is exactly one idiomatic way to express it. This makes generated code predictable and reviewable — a reviewer learns the pattern once and recognizes it everywhere.

3. **Contracts are mandatory, not optional**
   Preconditions, postconditions, and invariants are first-class syntax at public boundaries. A reviewer can see a function's guarantees without reading its body.

4. **Side effects are visible**
   Mutation requires `mut`. I/O and concurrency require explicit channel/effect constructs. Nothing changes silently behind a reviewer's back.

5. **Failure paths are typed**
   Errors use `Result` types on a single explicit channel. There are no exceptions that can escape silently.

6. **Feedback is machine-readable**
   The compiler emits structured JSON diagnostics. An LLM editor can read a compile error and propose a correct fix — this is the loop that makes "AI-written, human-reviewed" practical at scale.

7. **Determinism by default**
   Value semantics and structured concurrency mean the same input produces the same output. Reviewable code must be reproducible code.

## What Xz is not

- Not a dynamically-typed scripting language
- Not a language that hides memory management behind a GC without guarantees
- Not a language with "clever" per-library sugar that must be learned case by case

## Consequences for the reviewer

| Question | Answer in Xz |
|---|---|
| What can this function change? | Only `mut` bindings it declares or receives |
| What are its guarantees? | `pre` / `post` contracts in the signature |
| What can go wrong? | `Result[T, E]` in the return type |
| What does it talk to? | `Chan[T]` declarations and `send`/`recv` sites |
| Why did the compiler reject it? | JSON diagnostics with code, span, and suggested fix |