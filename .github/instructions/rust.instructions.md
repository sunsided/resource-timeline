---
applyTo: "**/*.rs"
---

# Rust code

- Every module (`//!`), type, field, variant, function, constant and macro has a doc
  comment, private and `pub(crate)` items included. Docs state intent, invariants, units and
  error/panic conditions (`# Errors`, `# Panics`, `# Safety`). They do not restate the name.
- No `mod.rs`: modules with children are `foo.rs` plus `foo/`. Integration tests live under
  `tests/it/`.
- No separator comments (`// -----`, `// =====`). If a file needs sections, split it into
  submodules.
- Leave a blank line after the closing `}` of a block statement when another statement
  follows, and between logical phases of a function. No lumped code.
- One level of abstraction per function. Long loops (simulations, game ticks, solvers) are
  short drivers that call documented step functions. If a block needs a "what" comment,
  extract it into a function named after that comment.
- `clippy::pedantic` clean. Suppression only with a scoped
  `#[expect(lint, reason = "...")]`. Never `#[allow]`, and never crate-level suppression.
- No `unsafe` unless the crate opts out of `#![forbid(unsafe_code)]` with a documented
  reason. Each unsafe block has one operation and a `// SAFETY:` comment.
- The crate is `#![no_std]`; its only runtime dependency is the optional `serde`. Heap
  storage, `envelope` and `greedy` sit behind the `alloc` feature. Timelines, state
  timelines, plans and the codec must build without `alloc` with `Fixed` storage (the CI
  `no-std` job). Flag any new `[dependencies]` entry, any `std` use, and any feature-gated
  item without its `#[cfg]` on the re-export in `lib.rs`.
- All tick and level arithmetic is checked: overflow becomes `Error::Overflow`, never a wrap
  or a panic. Searches and cuts take a budget and report exhaustion as an error, never as a
  verdict. Flag unchecked `+`, `-`, `*` or `as` casts on ticks and levels.
- Fixed storage validates capacity and returns `Error::CapacityExceeded`; every rejected
  add, place or decode leaves the value unchanged. Heap and fixed storage share all logic:
  a change to one path is tested against the other (`tests/it/fixed.rs`).
- Every check has a brute-force oracle in `tests/it/oracle.rs` (and the state and envelope
  tests) that shares no logic with the implementation; a new check joins those property
  tests in the same PR. Fuzz targets cover the same ground with extreme values: `decode`
  (codec), `serde` (deserialization), `timeline` (edits, levels, export), `plan` (placement,
  `fits`, `earliest_start`) and `envelope` (soundness for a concrete schedule). A new entry
  point that parses input or does tick or level arithmetic joins one of them.
- Public API changes go with a matching update to the README scope list and
  `docs/design.md`. Name every public API change in the PR.
- Mermaid diagrams in doc comments must match the code they describe. Update them when the
  structure changes.
