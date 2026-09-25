# Changelog

All notable changes to this project are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versioning will follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html) once the language reaches 1.0.

## [Unreleased]

### Added

- Front end and LLVM JIT backend: `xz check`, `xz check-json`, and `xz run`.
- Contracts and intent verification: `pre` / `post`, `@intent`, `@requires`, `@ensures`, `@effects`, and the `@trusted` escape hatch.
- C ABI bridge, shared-library output (`xz build --shared`), and Python ctypes binding generation (`xz bind --lang python`).
- Structured concurrency: typed channels, a deterministic task scheduler, and `async` / `await` on the JIT path.
- Tooling: `xz fmt`, `xz lsp`, `xz pkg gen`, and `xz pkg add`.
- Standard library first slices: `List`, `Map`, `Set`, `io`, `math`, and `time`.
- Ecosystem documentation covering the `next.xz` and `rails.xz` host toolkits that consume the compiler through the C ABI and `.xzint` interfaces.

[Unreleased]: https://github.com/x1zzdev/Xz/commits/main
