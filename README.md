# Xz

A general-purpose programming language for code that machines write and people have to trust.

[![CI](https://github.com/x1zzdev/Xz/actions/workflows/ci.yml/badge.svg)](https://github.com/x1zzdev/Xz/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![LLVM 17](https://img.shields.io/badge/backend-LLVM%2017-informational.svg)](docs/13-codegen.md)

AI models write production code faster than anyone can read it. Writing is no longer the bottleneck; deciding what to trust is. Xz is built around that shift, so a reviewer can answer the important questions from the signature instead of the body:

| Reviewer's question | Where Xz answers it |
|---|---|
| What does it do? | `@intent`, checked against the code |
| What can it change? | `mut` bindings and `@effects` |
| What are its guarantees? | `pre` / `post` contracts |
| What can go wrong? | `Result[T, E]` on a single error channel |

A documented claim that the code does not satisfy fails the build. A claim the compiler cannot prove needs a human `@trusted` stamp. A comment cannot quietly lie about the code it describes.

## A look at the language

```xz
/// Returns the principal square root of x.
/// @intent  Returns the square root of a non-negative number.
/// @requires x is non-negative
/// @ensures result is ok implies result.value >= 0.0
/// @effects none
func sqrt(x: Float) -> Result[Float, DomainError]
    pre  x >= 0.0
    post result is ok implies result.value >= 0.0
{
    if x < 0.0 {
        err(DomainError("negative input"))
    } else {
        ok(approx_sqrt(x))
    }
}
```

The contract is not a convention someone may forget. The compiler reads it.

## Build and run

Xz needs a current Rust toolchain and a portable LLVM 17 install. The setup script checks the LLVM install and prints the two environment variables the backend expects.

```sh
git clone https://github.com/x1zzdev/Xz.git
cd Xz/xz-cli
scripts/setup-llvm.sh
cargo run -- run ../examples/hello.xz
```

The same command works for the other examples: `contracts.xz`, `concurrency.xz`, `async.xz`, `ffi.xz`, `lists.xz`, `maps.xz`, `sets.xz`, `io.xz`, `time.xz`. See [examples/README.md](examples/README.md) for what each one shows.

## Status

The compiler is usable today. The front end (`xz check`) and the LLVM JIT backend (`xz run`) work. The C ABI bridge, shared-library output (`xz build --shared`), and generated Python ctypes bindings (`xz bind --lang python`) are in. Typed channels, a deterministic task scheduler, and `async`/`await` run on the JIT path. The language server (`xz lsp`), the formatter (`xz fmt`), and the package commands (`xz pkg gen`, `xz pkg add`) are available.

Phases 1 through 4 are complete, and the remaining phases are partly implemented. The real state of each phase is tracked in [docs/08-roadmap.md](docs/08-roadmap.md), not in a status table that goes stale.

## Documentation

The specification lives in `docs/`. Start with the philosophy, then read the part that matches your interest.

| Document | Contents |
|---|---|
| [01-philosophy.md](docs/01-philosophy.md) | The thesis and the reviewer's questions |
| [02-syntax.md](docs/02-syntax.md) | Syntax and the shape of a program |
| [03-type-system.md](docs/03-type-system.md) | Types and contract-point explicitness |
| [04-memory-model.md](docs/04-memory-model.md) | Value semantics and explicit mutation |
| [05-concurrency.md](docs/05-concurrency.md) | Structured concurrency and typed channels |
| [06-error-handling.md](docs/06-error-handling.md) | `Result` types and one error channel |
| [07-compiler.md](docs/07-compiler.md) | Diagnostics, formatter, and language server |
| [08-roadmap.md](docs/08-roadmap.md) | What is done and what is next |
| [09-intent-verification.md](docs/09-intent-verification.md) | How declared behavior is checked |
| [10-ffi-interop.md](docs/10-ffi-interop.md) | C ABI bridge and Python bindings |
| [11-grammar.md](docs/11-grammar.md) | The authoritative grammar |
| [12-stdlib.md](docs/12-stdlib.md) | The standard library surface |
| [13-codegen.md](docs/13-codegen.md) | The LLVM backend and runtime |
| [14-codegen-notes.md](docs/14-codegen-notes.md) | Backend problems and the decisions that settled them |
| [15-ecosystem.md](docs/15-ecosystem.md) | Host integrations built on the Xz core |

A Korean overview is available at [README_kr.md](README_kr.md).

## Ecosystem

Xz is the language and the compiler. Frameworks connect to it through the C ABI
and the `.xzint` interface format, so a host integration can be built without
changing the language itself.

| Project | Host | Role |
|---|---|---|
| **Xz** (this repository) | none | Language, specification, compiler, LLVM backend, C ABI, `.xzint` |
| [next.xz](https://github.com/x1zzdev/next-xz) | Next.js / TypeScript | Binding generation, Bun FFI and Wasm loader, agent loop, `/___audit` overlay |
| [rails.xz](https://github.com/imrubydev/rails-xz) | Ruby on Rails | Ruby FFI bridge, ActiveJob agent loop, `/xz_audit` engine |

Each toolkit follows the same path: an agent writes a contracted `.xz` module,
`xz check-json` drives the repair loop, `xz build --shared` produces the library,
and a human approves the contracts in the host's audit view. The core does not
change for each framework, and that is the point. See
[docs/15-ecosystem.md](docs/15-ecosystem.md).

## Contributing

Contributions are welcome, and the process is meant to be boring. Read [CONTRIBUTING.md](CONTRIBUTING.md) for the build, the test loop, and the two rules that matter: spec before implementation, and one logical change per commit. Everyone participating agrees to the [Code of Conduct](CODE_OF_CONDUCT.md).

## Support and security

Questions and ideas go to [GitHub Discussions](https://github.com/x1zzdev/Xz/discussions) or the issue tracker. Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md), never in a public issue.

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you explicitly state otherwise, any contribution you submit for inclusion in this project, as defined in the Apache-2.0 license, is dual licensed as above, with no additional terms.

Xz is a programming language and is unrelated to the `xz` compression utility.
