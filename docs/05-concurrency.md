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
3. No global mutable state. `mut` is always local or explicitly passed as a `mut` parameter.
4. No locks, no shared counters, no unsafe access.

## Benefits for review

- A reviewer can see the complete communication graph of a program by reading channel declarations and `send`/`recv` sites.
- No race conditions, no lock ordering, no hidden shared memory.
- Scheduling is deterministic given the same inputs (cooperative tasks).
- Data races are impossible by construction.

## Structured concurrency

```
async func fetch(url: Str) -> Result[Str, HttpError] {
    ...
}

let body = await fetch(url)?   // suspension is visible at the call site
```

`await` and channel operations are the only suspension points and are always visible in the syntax.

## Future options

- Deterministic scheduling specification
- Backpressure policies on channels
- Structured cancellation
- Supervision / restart policies for task trees