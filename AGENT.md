# AGENT.md — Working agreements for this repository

Xz is in the **design/documentation phase**: the product of this repo is
`docs/` and `examples/`. There is no implementation yet. Everything an AI
assistant does here must make the language *more reviewable* — that is the
whole point of Xz (see [docs/01-philosophy.md](docs/01-philosophy.md)).

## Repo layout

- `docs/01..12` — the specification. `docs/11-grammar.md` is the authoritative
  grammar (which constructs exist); `docs/12-stdlib.md` is the stdlib surface
  (which names exist).
- `examples/*.xz` — design-validation programs. They must comply with the
  docs, not invent syntax.
- `xz-cli/` — the compiler implementation (Rust).

## Ground rules

1. **Philosophy gate.** Every change must strengthen the answer to the four
   reviewer questions: *What does it do? What can it change? What are its
   guarantees? What can go wrong?* If a proposed feature makes any of them
   harder to answer, reject it.
2. **Docs are the contract.** Any new construct goes into the grammar sketch
   (`02-syntax.md`) plus its cross-referenced doc, first. If an example needs
   a feature the docs don't specify, add the spec before the example — never
   invent syntax silently.
3. **Examples must comply.** Before finishing, grep examples and docs for
   stale or contradictory patterns (e.g. `as Str`, `ok(0)`, `Result[Int, Err]`,
   `ok(result).value`).
4. **One canonical syntax.** Never introduce a second way to say something.
   If a choice is ambiguous, prefer the most explicit, most reviewable form
   and document it.

## Commit rule (most important)

**Commit continuously and autonomously, in the smallest coherent unit, as soon
as one completes. Do not wait for the user to ask.**

- The exception is the first commit of a session's work that reeks of "let me
  check this is what you want" — but even then, commit the work; the user can
  amend or revert.
- One logical change = one commit. A spec gap, a doc fix, and an example
  update are three commits, not one, even when they touch related files.
- "Smallest coherent unit" means the change is internally consistent and
  complete: docs and examples agree, links resolve, code compiles, no
  half-edits.
- In a design repo, the commit history *is* the record of design decisions.
  Frequent small commits give reviewers a clean history.
- Commit messages state the decision, not the file list:
  `Add handle semantics to close the Ptr aliasing hole`, not `Update docs`.
- Never bundle unrelated edits into one commit, and never commit secrets.
- For a feature that spans multiple small units, commit each unit as it
  completes rather than one big "implement X" commit at the end.

## Before committing

- Run `git status`, `git diff`, and `git log --oneline -10`; stage only the
  intended files.
- Verify the change is consistent: no dangling links, no stale examples, no
  contradictions between docs.

## Language & style

- Docs are in English, written for a reviewer of AI-written code.
- No emojis. No comments in code that restate the code.
- Keep doc changes tight: a new rule is one section, one example, one
  rationale — not an essay.
- The user writes in Korean; respond in Korean unless the user switches.
- This repo's Rust uses the toolchain's native stdlib style (see `xz-cli/`
  for established patterns); match the surrounding code, not external
  conventions.