# Contributing to Xz

Thanks for taking the time to help. This project has an unusual shape, so a few minutes here will save you a rejected pull request later.

## What kind of project this is

Xz is a language defined by its specification. The product of the repository is `docs/` plus the reference implementation in `xz-cli/`. The most valuable contributions usually make the language *more reviewable*, which is the entire point of the project. That means a documentation change and a parser change are equally important, and sometimes the documentation change is the one that matters.

## Ways to contribute

- Fix or clarify the specification in `docs/`.
- Add or correct an example in `examples/`.
- Report a place where the compiler and the specification disagree.
- Implement or improve a command in `xz-cli/`.
- Improve the test suite. Tests are how the specification stays honest.
- Report a vulnerability privately as described in [SECURITY.md](SECURITY.md).

## Before you start

Read [docs/01-philosophy.md](docs/01-philosophy.md) first. Every change is judged against one question: does it make it easier to answer *what does it do, what can it change, what are its guarantees, and what can go wrong?* A feature that makes any of those harder to answer will be turned down, however clever it is.

[docs/11-grammar.md](docs/11-grammar.md) is the authority for which constructs exist. [docs/12-stdlib.md](docs/12-stdlib.md) is the authority for which names exist. If your change introduces syntax or a name that is not in those documents, the specification update comes first.

## Development setup

You need a current Rust toolchain and a portable LLVM 17 install. The setup script checks the install and prints the environment variables the backend expects.

```sh
git clone https://github.com/x1zzdev/Xz.git
cd Xz/xz-cli
scripts/setup-llvm.sh
cargo build
cargo test
```

`xz-cli/.cargo/config.toml` pins the LLVM paths for the machine that built it. On another machine, run `scripts/setup-llvm.sh --emit` or adjust that file. See [docs/13-codegen.md](docs/13-codegen.md) for the details.

The usual loop:

```sh
cargo test
cargo run -- check ../examples/contracts.xz
cargo run -- run ../examples/hello.xz
```

CI runs `cargo test`. Match the style of the surrounding code rather than reformatting unrelated files; the repository is not rustfmt-canonical by design.

## The two rules that matter

1. **Spec before implementation.** A new construct goes into [docs/02-syntax.md](docs/02-syntax.md) and its cross-referenced document first. If an example needs a feature the specification does not describe, add the specification before the example. Never invent syntax silently.
2. **One logical change per commit.** A specification gap, a documentation fix, and an example update are three commits, not one, even when they touch related files. Each commit should be internally complete: links resolve, examples agree with the docs, and tests pass.

## Commit messages

State the decision, not the file list.

```
Add handle semantics to close the Ptr aliasing hole
```

not

```
Update docs
```

## Pull requests

Keep a pull request focused. If you find an unrelated problem while working, open a separate issue or pull request instead of bundling it. In the description, say what changes, why, and which document is now the authority for it.

Because the specification is the contract, a pull request that changes behavior should point to the document that describes the new behavior. Reviewers check the diff against that document, not against their own preferences.

## Reporting bugs and requesting features

Use the issue templates. A bug report is most useful when it includes a minimal `.xz` file, the command you ran, the output you expected, and the output you got. If the compiler and the specification disagree, say which one you believe is wrong.

## Code of conduct

Participation is covered by [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Report unacceptable behavior to ax1s@x1zz.com.

## License

Xz is dual licensed under MIT or Apache-2.0, at your option. By submitting a contribution you agree to license it under the same terms. See the [README](README.md#license) for the exact wording.
