# Ecosystem

Xz core is the language and the compiler. A framework does not have to wait for
an official integration to use it: the C ABI and the `.xzint` interface format
are the public boundary, and host toolkits build on top of that boundary. Two
are in development.

## The projects

| Project | Host | What it owns |
|---|---|---|
| [Xz](https://github.com/x1zzdev/Xz) (this repository) | none | Language, specification, compiler, LLVM backend, C ABI, `.xzint`, `xz check-json`, LSP |
| [next.xz](https://github.com/x1zzdev/next-xz) | Next.js / TypeScript | Binding generation, Bun FFI and Wasm loader, agent loop, `/___audit` overlay |
| [rails.xz](https://github.com/imrubydev/rails-xz) | Ruby on Rails | Ruby FFI bridge, ActiveJob agent loop, `/xz_audit` engine |

The three share one thesis. The framework owns the web layer, Xz owns the logic
that has to be fast and verifiable, and a human audits contracts instead of a
multi-file diff.

## How a host plugs in

Every integration follows the same path, and it uses only surfaces this
repository already ships.

1. An agent writes a contract shell as an `.xz` module with `@export` on the
   functions the host calls. The `@intent`, `@requires`, `@ensures`, and
   `@effects` comments describe the behavior.
2. The host runs `xz check-json` and feeds the structured diagnostics back to the
   model, bounded by a retry budget.
3. `xz build --shared` compiles the module to a C ABI shared library.
4. A binding generator reads the `.xzint` interface and emits host-language
   calls (TypeScript, Ruby) over the same C symbols.
5. A human reviews contract and effect badges in the host's audit view.

Steps 1, 2, 3, and 5 have the same shape in every host, which is why the core
language does not need to know which framework is calling it. This is what
"interop first" means in [01-philosophy.md](01-philosophy.md) and how
[10-ffi-interop.md](10-ffi-interop.md) keeps the boundary stable.

## What belongs where

- A change that makes the language itself more reviewable belongs in this
  repository.
- A change about a specific runtime (Bun, Wasm, Fiddle, ActiveJob) or a
  specific user interface belongs in the host toolkit.
- The `.xzint` format and the `xz` CLI commands are the contract. A host toolkit
  may not require a change to either without a specification update here.

## Adding a project

A new integration is welcome. Open a discussion or an issue with the host runtime
and the boundary you plan to use. The bar is the same as for any contribution:
does it make it easier for a reviewer to answer what the code does, what it can
change, what it guarantees, and what can go wrong?
