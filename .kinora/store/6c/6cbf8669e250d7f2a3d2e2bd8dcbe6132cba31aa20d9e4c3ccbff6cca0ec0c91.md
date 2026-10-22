# Plan — the gate on the wire

**Task:** kino://9fe618262add641c158bfc7815d36c9aad93471e3391b070f8e764e044941817

One phase: tests (`tests/wire_governance.rs`) → implementation → review → fixes.
`Disposition` derives Facet for the preflight reply; `WireVerdict` mirrors the
four verdicts; an empty `edited_argv` means none; the registry gains
`pending()`; the console check is the engine's, as `input`'s is.

Status: todo.
