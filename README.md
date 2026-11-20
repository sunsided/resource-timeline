# resource-timeline

Resource timelines for onboard scheduling: energy, memory, thermal and communication budgets.

## Scope

- Numeric timelines for consumable resources (energy, data volume) with step and rate
  (piecewise-linear) impacts, and for reusable resources (instruments, transmitters, power
  channels) with interval claims
- State timelines for modes and visibility windows, with set and require impacts
- First-violation checks with explanations: the tick, the level or state, and the activities
  responsible
- End-state level targets (for example the battery reserve at the end of a sol)
- Incremental add and remove on a sorted change-point profile; level queries at any tick
- Profile extraction for display and telemetry: breakpoints, regular samples, and a compact
  exported profile that answers level queries on its own
- Plans over several timelines: atomic placement and earliest-feasible-start search, solved in
  closed form per linear piece when rates are involved
- Fixed-capacity, allocation-free variants of every timeline and of the plan (`Fixed<N>`,
  `FixedPlan<T, S, N>`), sharing all logic with the heap variant, with typed capacity errors;
  the `alloc` feature (default) adds heap storage, envelopes and the greedy scheduler
- A greedy priority scheduler (example and test vehicle), see `examples/rover.rs`
- Level envelopes for activities with start windows: safe, possibly violated or certainly
  violated for every schedule the windows admit, with uncertain durations (claims and rates
  anchored at the activity end), precedence constraints (window propagation plus an exact
  min-cut extreme for stepwise activities), and state envelopes for requirements and conflicts
- Precedences between starts and ends (contingent ends included), coupled exactly by a
  minimum cut within a node budget
- A compact, versioned, CRC-32-checked byte encoding of timelines and profiles without
  allocation, and optional `serde` support (feature `serde`)
- Borrowed search scratch for small onboard stacks (`SearchScratch`)
- A bridge example that checks a schedule with the windows of `temporal-control`'s dispatcher,
  see `examples/stn_bridge.rs`
- Brute-force oracles for every check, compared in property tests
- Companion to temporal-control, without a dependency on it

## Status

Pre-release. Milestones 1 to 6 of [docs/design.md](docs/design.md) are implemented. The API
will change.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
