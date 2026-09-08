# Concurrency

## Model: structured concurrency + typed channels

- `async` functions use `await` for suspension.
- Tasks communicate **exclusively** via typed channels (`Chan[T]`).
- There is **no shared mutable state** between tasks — value semantics makes this natural.

## Channels

A channel has exactly one declared payload type and is part of the type system.

```
chan work: Chan[Job]
chan done: Chan[Result[JobId, Err]]

task worker {
    loop {
        let job <- recv(work)     // receive (blocks)
        let r = execute(job)
        send(done, r)
    }
}
```

```
send(ch, value)    // copy of value is sent (value semantics)
let v <- recv(ch)  // receives a copy; blocks if empty
```

## Rules

1. A task's memory is its own. Sending a value over a channel transfers a **copy** (value semantics).
2. Channel payload types are mandatory at declaration.
3. **No global mutable state — except channel bindings.** A `chan` declaration
   is the *single sanctioned form* of global state: it is exactly the
   communication mechanism the model allows, its payload type is fixed at the
   one declaration site, and the reviewer sees the complete graph by reading
   those declarations. Everything else is local or explicitly `mut`-passed.
4. No locks, no shared counters, no unsafe access.
5. Handles never cross channels — a `send` transfers a copy, and handles
   cannot be copied (see [10-ffi-interop.md](10-ffi-interop.md)).

## Benefits for review

- A reviewer can see the complete communication graph of a program by reading channel declarations and `send`/`recv` sites.
- No race conditions, no lock ordering, no hidden shared memory.
- Channel-level behavior is deterministic: given the same inputs, the same values flow through each channel in the same order. The exact interleaving of *independent* tasks is scheduler-defined and specified in Phase 6.
- Data races are impossible by construction.

## Structured concurrency

```
async func fetch(url: Str) -> Result[Str, HttpError] {
    ...
}

let body = await fetch(url)?   // suspension is visible at the call site
```

`await` and channel operations are the only suspension points and are always visible in the syntax.

## Program termination

The program ends when `main` returns. Remaining tasks are torn down without
draining their channels — a task that must complete before the program exits
must say so: by waiting on an acknowledgment channel (see the example program)
or by explicit structured cancellation (planned, below). There is no
implicit join; the rule is one sentence long and leaves nothing to guess.

## Future options

- Deterministic scheduling specification
- Backpressure policies on channels
- Structured cancellation
- Supervision / restart policies for task trees