# Rule: no unwrap/expect in library code

Library (non-test) code must not call `.unwrap()` or `.expect()` on a `Result`
or `Option`. A fallible path returns an error to its caller; it does not panic
inside a reusable crate.

**Rejected:** "unwrap when we're sure." The agent's certainty is precisely the
thing this framework does not trust — and a panic in a library crashes every
binary that depends on it, far from the site of the bad assumption. If a value
is truly infallible, model it in the type system (`NonZero`, a validated
newtype, an enum) so `unwrap` is unnecessary rather than merely unreached.

**Mechanism:** clippy — `unwrap_used` and `expect_used`, at `warn` (rung 1).
Both fire only where the lints are enabled; tests are exempt.

**Escape hatch:** `#[allow(clippy::unwrap_used)]` on the specific line with a
one-line justification comment, or free use inside `#[cfg(test)]`. The rule
stays in force; the exception is local and visible.
