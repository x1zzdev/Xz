# Governance

## The model

Xz uses a maintainer model. A small group of maintainers holds merge rights and makes the final call on what enters the language. The model is intentionally simple while the project is pre-1.0. It can change when the contributor base is large enough to need it.

## Who decides what

- **Routine changes** (bug fixes, documentation, tests, small implementation work) are merged by any maintainer after review.
- **Specification changes** (new syntax, new semantics, new standard library names) need review from at least one maintainer who is not the author, and must update the authoritative document before or with the implementation.
- **Changes to the philosophy or the license** need consensus among the maintainers.

## The constitution

[docs/01-philosophy.md](docs/01-philosophy.md) is the closest thing this project has to a constitution. Every feature is measured against the four reviewer questions. When a decision is contested and the documents do not settle it, the philosophy breaks the tie.

## Becoming a maintainer

There is no application process. A contributor who has landed several substantial changes, shows good judgment in review, and understands that rejecting a feature is sometimes the right answer may be invited by the existing maintainers. The invitation is about trust and sustained involvement, not about a contribution count.

## Conflicts of interest

A maintainer with a financial or personal stake in a decision should say so and step back from the final call. Reviewers are expected to judge a change against the specification, not against the author or the organization behind it.

## Changing this document

Governance changes are proposed as a pull request to this file and require consensus among the maintainers. The reasoning goes in the pull request description, because the history of the decision is part of the project record.
