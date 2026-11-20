# resource-timeline

Resource timelines for onboard scheduling: energy, memory, thermal and communication budgets.

## Scope

- Numeric timelines for consumable resources (energy, data volume) with step and rate
  (piecewise-linear) impacts, and for reusable resources (instruments, transmitters, power
  channels) with interval claims
- State timelines for modes and visibility windows, with set and require impacts
- First-violation checks with explanations: the tick, the level or state, and the activities
  responsible
- Incremental add and remove on a sorted change-point profile; level queries at any tick
- Plans over several timelines: atomic placement and earliest-feasible-start search
- A greedy priority scheduler (example and test vehicle), see `examples/rover.rs`
- Level envelopes for activities with start windows: safe, possibly violated or certainly
  violated for every schedule the windows admit
- Companion to temporal-control, without a dependency on it

## Status

Pre-release. Milestones 1 to 5 of [docs/design.md](docs/design.md) are implemented, and
incremental updates from milestone 6. Not implemented yet: fixed-capacity (allocation-free)
storage, state timelines in envelopes, and precedence-aware envelopes. The API will change.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
