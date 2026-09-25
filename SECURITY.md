> 한국어판: [SECURITY_kr.md](SECURITY_kr.md)

# Security Policy

## Supported versions

Xz is pre-1.0 and moves quickly. Only the latest commit on `main` is supported. There are no maintenance branches and no backports.

## Reporting a vulnerability

Report vulnerabilities privately. Do not open a public issue.

- Email ax1s@x1zz.com, or
- Use GitHub's private vulnerability reporting on the repository's Security tab.

Include what you can of the following:

- The affected command and version or commit.
- A minimal `.xz` file or interface file that triggers the issue.
- The behavior you expected and the behavior you got.
- Why you believe it is a security problem and not a correctness bug.

You will get an acknowledgement within a few days. If the report is valid, we will agree on a fix and a disclosure date before any public write-up. Credit is given unless you ask to stay anonymous.

## What counts as security

Xz compiles and runs code and links into C and Python. Reports that matter most:

- Memory-safety failures in generated code or the runtime, including bounds and ownership handling.
- Ways to make the compiler emit code that violates a declared contract or an enforced invariant.
- Failures in the FFI boundary: ownership transfer, handle lifetime, or layout assumptions in the generated C header.
- Sandbox or supply-chain problems in `xz pkg add`, including registry fetch and verification.

A divergence between the compiler and the specification that a reviewer could not detect is in scope, because the language's guarantee is that such divergence is caught.

## What does not count

- Crashes on malformed input that produce a diagnostic instead of memory unsafety.
- Denial of service from compiling a large or adversarial source file on your own machine.
- Issues that require an already-compromised toolchain or a modified LLVM install.
- Missing hardening flags that are not part of a stated guarantee.

When in doubt, report it. It is better to close a report as not-a-vulnerability than to miss a real one.

## Safe harbor

Good-faith research on your own systems is welcome. We will not pursue legal action for research that follows this policy, avoids privacy violations and service disruption, and gives us reasonable time to fix the issue before disclosure.
