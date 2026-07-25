# Rule: no todo!/unimplemented!/dbg! in committed code

Committed code must not contain `todo!()`, `unimplemented!()`, or `dbg!()`.
These are scaffolding for work in progress: `todo!`/`unimplemented!` panic at
runtime, and `dbg!` writes to stderr and is never intended to ship. An agent
that leaves them behind ships a latent panic or noise.

**Rejected:** treating these as harmless placeholders that "someone will clean
up later." In an agentic loop there is often no later — the scaffolding is the
thing that gets committed. Catching it mechanically at commit time is the only
reliable cleanup.

**Mechanism:** clippy — `todo`, `unimplemented`, and `dbg_macro`, at `warn`
(rung 1). A genuine not-yet-implemented path should return a real error
(`Err(NotImplemented)`) rather than a panicking macro.

**Escape hatch:** `#[allow(clippy::todo)]` (etc.) on the line with a justification
when a panic really is the intended contract (e.g. an unreachable match arm —
though `unreachable!` with a message is usually clearer).
