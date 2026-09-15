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
- Channel-level behavior is deterministic: given the same inputs, the same values flow through each channel in the same order. The exact interleaving of *independent* tasks is fixed by the [deterministic scheduling](#deterministic-scheduling) policy below.
- Data races are impossible by construction.

## Structured concurrency

```
async func fetch(url: Str) -> Result[Str, HttpError] {
    ...
}

let body = await fetch(url)?   // suspension is visible at the call site
```

`await` and channel operations are the only suspension points and are always visible in the syntax.

## Deterministic scheduling

The scheduler is **single-threaded, cooperative, and run-to-blocking**: a task
runs until it blocks or finishes, and is never preempted. There is no
parallelism in this phase. This is what makes a schedule a function of the
program and its messages rather than of host timing.

State:

- a **ready queue** (FIFO) of runnable tasks;
- per channel, a FIFO **message queue** and a FIFO **receiver queue** of tasks
  blocked on `recv`.

Rules:

1. **Spawn.** At program start the ready queue is `main`, followed by the
   top-level `task` declarations in source order. Each declaration spawns
   exactly one task.
2. **Run-to-block.** The scheduler removes the head of the ready queue and runs
   it until it blocks or finishes. A running task is never preempted.
3. **`send(ch, v)` never blocks** — channels are unbounded in this phase. If a
   task is blocked on `recv(ch)`, the message is handed to the *earliest* such
   receiver (its `recv` returns the message and the task is enqueued); otherwise
   the message is appended to `ch`'s message queue.
4. **`recv(ch)` blocks only when `ch` is empty.** If a message is queued it is
   dequeued in FIFO order and the task continues without yielding. If the
   channel is empty, the task is appended to `ch`'s receiver queue and the
   scheduler runs the next ready task.
5. **Wakeups are FIFO.** A task made runnable (by spawn, a `send` handoff, or a
   completed `await`) is appended to the **tail** of the ready queue.
6. **`await` suspends the current task.** An `async` function runs as a
   scheduled coroutine. `await e`, where `e` calls an `async` function, runs it
   as a child: the parent blocks until the child completes, then the parent is
   enqueued with the child's result. If the child itself blocks on `recv`, the
   scheduler runs other ready tasks meanwhile. `await` follows the same
   ready-queue policy as `recv`.
7. **Termination.** When `main` finishes, the schedule ends and every remaining
   task is torn down without draining its channels. The scheduler never runs a
   task after `main` returns.

**Determinism.** For a fixed program with no timing input (`time`/I/O effects
that depend on the host), the schedule — the order in which tasks run and the
value each `recv` returns — is unique. Independent tasks observe one another
only through channel messages, so their relative order is the total order fixed
by rules 1–6, not by the host. `examples/concurrency.xz` is the validation
program for this policy.

## Program termination

The program ends when `main` returns. Remaining tasks are torn down without
draining their channels — a task that must complete before the program exits
must say so: by waiting on an acknowledgment channel (see the example program)
or by explicit structured cancellation (planned, below). There is no
implicit join; the rule is one sentence long and leaves nothing to guess.

## Future options

- Backpressure policies on channels
- Structured cancellation
- Supervision / restart policies for task trees