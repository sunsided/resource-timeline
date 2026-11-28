# resource-timeline

[![crates.io](https://img.shields.io/crates/v/resource-timeline.svg)](https://crates.io/crates/resource-timeline)
[![docs.rs](https://img.shields.io/docsrs/resource-timeline)](https://docs.rs/resource-timeline)
[![CI](https://github.com/sunsided/resource-timeline/actions/workflows/ci.yml/badge.svg)](https://github.com/sunsided/resource-timeline/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/sunsided/resource-timeline/graph/badge.svg)](https://codecov.io/gh/sunsided/resource-timeline)
[![MSRV](https://img.shields.io/crates/msrv/resource-timeline)](https://crates.io/crates/resource-timeline)
[![License](https://img.shields.io/crates/l/resource-timeline)](https://github.com/sunsided/resource-timeline#license)
[![unsafe forbidden](https://img.shields.io/badge/unsafe-forbidden-success.svg)](https://github.com/rust-secure-code/safety-dance/)

Resource timelines for onboard scheduling: energy, memory, thermal and communication budgets.

The crate answers one question for a scheduler: if these activities run at these times, does
any resource leave its safe range, and if so, when, by how much, and because of whom? A timeline
tracks one resource over integer ticks (a battery that drains and charges at given rates, a
data buffer, an instrument that can only be claimed once at a time, or a mode that has to hold
during an observation), and the check reports the first violation with the activities
responsible. On top of that come plans over several timelines, earliest-start search, and
envelopes that check a flexible schedule once for every start the windows allow, shares that
split a resource between agents out of contact, and zones that keep crews out of each other's
workspace. Every timeline also comes in a fixed-capacity, allocation-free variant for onboard
use.

Part of the [planning and search crates](https://sunsided.github.io/planning-and-search/); see also this crate's [overview page](https://sunsided.github.io/planning-and-search/crates/resource-timeline/).

## Who this is for

- Flight software and mission operations engineers who place activities on a spacecraft or
  rover and need to know whether battery, memory, data volume, instruments or comm windows hold
  up, onboard or on the ground before uplink.
- Developers of onboard or embedded schedulers that need checked arithmetic, explicit budgets
  and no heap.
- Planners and replanners that want a violation explained (the tick, the level and the
  activities involved) instead of a bare "infeasible".
- Game AI and simulation developers whose agents spend stamina, ammunition, storage or build
  capacity over game ticks, as in colony, strategy and survival games. Earliest-start search
  answers when an agent can afford an action, interval claims model shared workstations and
  cooldowns, end-state targets check a build order against a scouted attack, and the
  allocation-free variants keep checks off the heap inside a frame. See
  [`examples/colony.rs`](https://github.com/sunsided/resource-timeline/blob/main/examples/colony.rs)
  and [`examples/rush.rs`](https://github.com/sunsided/resource-timeline/blob/main/examples/rush.rs).
- Multi-agent and multi-robot systems whose agents share a resource (relay bandwidth, power at
  a shared station, a work site) but cannot see each other's use between contacts: budget
  shares let each agent check only its own part onboard. See
  [`examples/relay.rs`](https://github.com/sunsided/resource-timeline/blob/main/examples/relay.rs).
- Construction and production schedulers with spatially exclusive workspaces: zones turn an
  overlap graph between tasks into unary resources, with conflicts that name the zone and both
  tasks. See [`examples/site.rs`](https://github.com/sunsided/resource-timeline/blob/main/examples/site.rs).
- For the timing side of the same schedule (whether its temporal constraints can be met under
  uncertain durations, and executing it), use
  [temporal-control](https://github.com/sunsided/temporal-control). The two crates share only a
  convention (`i64` ticks, half-open intervals) and no dependency, because temporal
  controllability does not imply resource safety and both checks have to run.

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
- A greedy priority scheduler (example and test vehicle), see [`examples/rover.rs`](https://github.com/sunsided/resource-timeline/blob/main/examples/rover.rs)
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
  see [`examples/stn_bridge.rs`](https://github.com/sunsided/resource-timeline/blob/main/examples/stn_bridge.rs)
- Decoupled budgets for resources shared by agents out of contact (`share::split`): per-agent
  shares with margins that never add up to more than the capacity, re-split by the agents in
  contact, with exact needs from `Timeline::lowest_from` and, under uncertain demand,
  `Envelope::lowest_from`
- Zone resources for spatially exclusive workspaces (`zone`): unary zones occupied by tasks,
  zones from an undirected overlap graph (one per edge, exact), and conflicts that name the
  zone and both tasks; allocation-free on fixed storage
- Brute-force oracles for every check, compared in property tests

See [docs/design.md](https://github.com/sunsided/resource-timeline/blob/main/docs/design.md) for
the semantics of each check.

## Examples

Every algorithm has a small example built on a real-world case. Run one with
`cargo run --example <name>`.

| Algorithm | Example |
| --- | --- |
| First violation with contributors, rates, state timelines, plans, greedy scheduling | [`rover`](https://github.com/sunsided/resource-timeline/blob/main/examples/rover.rs): a Mars rover sol with energy, memory, transmitter, instrument temperature and comm passes |
| Earliest-start search with claims and states, greedy scheduling | [`colony`](https://github.com/sunsided/resource-timeline/blob/main/examples/colony.rs): a settler's day in a colony game |
| End-state targets | [`rush`](https://github.com/sunsided/resource-timeline/blob/main/examples/rush.rs): a strategy game build order against a scouted rush |
| Level envelopes over independent start windows, state envelopes | [`kitchen`](https://github.com/sunsided/resource-timeline/blob/main/examples/kitchen.rs): a restaurant kitchen's circuit load and combi-oven modes before service |
| Envelopes with uncertain durations | [`therapy_group`](https://github.com/sunsided/resource-timeline/blob/main/examples/therapy_group.rs): admissions to a therapy group with a hard seat limit |
| Envelopes with uncertain durations and precedences, windows from `temporal-control` | [`stn_bridge`](https://github.com/sunsided/resource-timeline/blob/main/examples/stn_bridge.rs): a rover STNU checked before and during execution |
| Fixed-capacity plans, `fits` and borrowed search scratch, closed-form search along rates, profile extraction (samples, export, breakpoints) and the checksummed codec for profiles and timelines | [`cubesat`](https://github.com/sunsided/resource-timeline/blob/main/examples/cubesat.rs): a small satellite replans an image onboard and downlinks its battery profile |
| Budget shares, re-allocation at contact, worst-case needs from envelopes | [`relay`](https://github.com/sunsided/resource-timeline/blob/main/examples/relay.rs): three lunar rovers share a relay orbiter's return link |
| Zone resources from an overlap graph, zone conflicts, serial scheduling with precedences | [`site`](https://github.com/sunsided/resource-timeline/blob/main/examples/site.rs): crews on one floor of a building site |

The `site` benchmark (`cargo bench --bench site`) schedules a **synthetic** instance with the
dimensions of the multi-crew case in the ISARC 2026 paper on RL task allocation in flexible
construction scenarios (86 tasks, 7 crews in 5 trades, 36 components, 34 overlap edges). It does
not reproduce the paper's tasks, durations or edges (the paper's data was not available when it
was written), so its numbers are not comparable with the paper's results.

## Installation

```sh
cargo add resource-timeline
```

For a target without a heap, keep only the fixed-capacity timelines, plans and the codec:

```toml
[dependencies]
resource-timeline = { version = "0.1", default-features = false }
```

## Usage

```rust
use resource_timeline::{ActivityId, Bound, Bounds, Impact, Timeline};

// Battery with 100 units, a reserve of 20 and a capacity of 120, room for 16 impacts.
let mut battery = Timeline::depletable(0, 100, Bounds::new(20, 120)?, 16)?;
battery.add(ActivityId(1), Impact::Step { at: 5, delta: -40 })?;
battery.add(ActivityId(2), Impact::Rate { start: 10, end: 20, per_tick: -5 })?;

// The reserve breaks at tick 19, when the level drops to 15.
let violation = battery.first_violation()?.expect("the reserve is broken");
assert_eq!((violation.at, violation.level), (19, 15));
assert_eq!(violation.bound, Bound::BelowMin(20));
# Ok::<(), resource_timeline::Error>(())
```

## Features

| Feature | Default | Enables |
| --- | --- | --- |
| `alloc` | yes | Heap storage (the default storage of every type), level and state envelopes, and the greedy scheduler |
| `serde` | no | `Serialize` and `Deserialize` for the plain data types, timelines and profiles (deserializing timelines and profiles also needs `alloc`) |

Without `alloc`, the crate needs no allocator: timelines, state timelines and plans use
`storage::Fixed` and `storage::FixedPlan`, and it builds for bare-metal targets such as
`thumbv7em-none-eabihf` (checked in CI).

## MSRV

The library builds on Rust 1.85 (`rust-version` in `Cargo.toml`, checked in CI). Tests,
benches and examples may need a newer toolchain. Raising the MSRV is a minor-version change.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](https://github.com/sunsided/resource-timeline/blob/main/LICENSE-APACHE))
- MIT license ([LICENSE-MIT](https://github.com/sunsided/resource-timeline/blob/main/LICENSE-MIT))

at your option. The code under
[`examples/`](https://github.com/sunsided/resource-timeline/tree/main/examples) is public
domain ([CC0-1.0](https://github.com/sunsided/resource-timeline/blob/main/LICENSES/CC0-1.0.txt)), so you can copy it freely.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion
in this work by you, as defined in the Apache-2.0 license, shall be dual licensed as above,
without any additional terms or conditions.

## Contributing

See [CONTRIBUTING.md](https://github.com/sunsided/resource-timeline/blob/main/CONTRIBUTING.md)
for the setup, the Taskfile commands and the branch and commit conventions.
