# resource-timeline: design

Status: draft. Milestones 1 to 6 are implemented, including end-state
level checks, profile extraction, uncertain durations, precedence-aware and
state envelopes, and fixed-capacity storage.

## Purpose

`resource-timeline` answers one question for an onboard scheduler: if these
activities run at these times, does any resource leave its safe range, and
if so, when, by how much, and because of whom?

It is the companion of `temporal-control`. That crate decides *when* events
may happen (STN/STNU, dynamic controllability). This crate decides whether
the resulting resource usage is safe. The two guarantees do not compose for
free: a dynamically controllable schedule can still drain the battery, fill
the mass memory or double-book the transmitter. Flight integration must
enforce both checks, independently. This crate therefore stands on its own
and has no dependency on `temporal-control`.

The concepts are inspired by timeline-based onboard planners such as MEXEC
and the Mars 2020 onboard planner, which track resource levels as timelines
and distinguish resources that are consumed from resources that are
borrowed. No specific algorithm or data format from those systems is copied
here.

## Requirements

- `no_std`. With the default `alloc` feature, heap storage is available and
  used by default; without it the crate needs no allocator and timelines,
  state timelines and plans use fixed-capacity storage (see "Storage").
  Envelopes and the greedy scheduler require `alloc`.
- Integer time. Time is an `i64` tick count; the crate never uses floats.
- Integer levels. Resource levels are `i64` in a mission-chosen unit
  (for example mWh for energy, KiB for storage, units for claimable
  resources).
- Checked arithmetic. Every sum that could overflow is checked; overflow is
  reported as an error, never wrapped and never panicked on.
- Explicit budgets. Every container has a caller-chosen capacity limit.
  Exceeding it returns an error, it never aborts on allocation.
- `#![forbid(unsafe_code)]`.
- No runtime dependencies unless strongly justified. There are none.
- Deterministic results: the outcome of a check does not depend on the
  order in which impacts were added.

## Resource kinds

| Kind | Example | Behaviour | Typical bounds |
| --- | --- | --- | --- |
| Depletable | battery energy, data volume, propellant | Activities consume or produce; the change persists after the activity | `[reserve, capacity]` |
| Claimable (reusable) | instrument, transmitter, comm window slot, CPU share | Activities borrow units for an interval and return them at the end | `[0, capacity]` |
| Rate-based | energy under a power draw, data under a downlink rate | Level changes linearly while an activity runs | as depletable |
| State | "instrument warm", "heater on", "pointing at Earth" | Discrete value set by activities; others require a value | equality, not range |

Depletable and claimable resources share one representation.
For a claimable resource the level is the number of *available* units: it
starts at capacity, a claim lowers it for `[start, end)`, and the lower
bound `0` is the "no double-booking" rule.

Rate-based resources are depletable timelines with `Rate` impacts, which
make the profile piecewise linear (milestone 2). State timelines are a
separate type, `StateTimeline` (milestone 4), because their constraint is
"the value equals X throughout an interval", not a numeric range.

Thermal limits are modelled at this level only as budgets, for example a
depletable "thermal margin" or a claimable "heater power slot". A physical
thermal model belongs to `polyzonotope-reach`, not here.

## Time model

- A tick is an `i64`. The unit (seconds, milliseconds, spacecraft clock
  subticks) and the epoch are a mission choice. The crate never converts.
- Each timeline has a start tick. Nothing may happen before it; the
  initial level holds from the start tick until the first change.
- Intervals are half-open: `[start, end)`. An activity that ends at `t`
  and another that starts at `t` do not overlap. This matches how a
  claim is released and immediately re-claimed at the same tick.
- Timelines have no end tick. A permanent step persists forever; the check
  covers every tick from the start onward.

## Timeline representation

### Milestone 1: piecewise-constant (superseded by the change-point profile below)

The level is a step function of time:

```text
level(t) = initial
         + Σ { delta  | Step { at, delta }        with at <= t }
         + Σ { -amount | Claim { start, end, amount } with start <= t < end }
```

Internally the timeline stores the impacts as added. A check builds a list
of change events `(tick, delta, activity)` (a claim produces two events),
sorts it by tick, and sweeps once. All events at the same tick are netted
before the level is compared with the bounds, so back-to-back claims on a
capacity-1 resource pass. Between events the level is constant, so
comparing once per distinct event tick (plus the initial segment) checks
every tick. Cost: `O(n log n)` time, `O(n)` memory for `n` impacts.

### Milestone 2: piecewise-linear (implemented)

Implementation: the timeline keeps, next to its impact list, a sorted list
of change points `(tick, level jump, slope change)`, one per tick, updated
incrementally on `add` and `remove` (binary search plus a shift). A check
sweeps the points once in `O(n)` without sorting or allocating. Inside a
segment with slope `m` that starts in bounds at level `L`, the first
crossing of `min` is at `floor((L - min) / |m|) + 1` ticks, so the
reported tick is exact without stepping tick by tick.

Original plan:

Rate-based impacts add a slope over an interval: `Rate { start, end,
per_tick }`. The level becomes piecewise-linear. Extremes of a linear
segment are at its ends, so the sweep still visits only event ticks but
must check the level just before each slope change as well as at it.
Slopes are integers per tick; fractional rates are expressed by choosing a
finer level unit. Charging (for example solar input) is a positive rate;
a capacity clamp (battery full, excess wasted) is *not* modelled, because
clamping hides violations; the upper bound check reports it instead.

## Activity impact model

An activity is identified by an opaque `ActivityId(u32)` chosen by the
caller. The crate does not know activity durations or names; it only
records impacts tagged with the id:

| Impact | Meaning |
| --- | --- |
| `Step { at, delta }` | Level changes by `delta` at `at` and stays changed. Consumption is a negative delta, production a positive one. |
| `Claim { start, end, amount }` | `amount > 0` units are unavailable during `[start, end)`. |
| `Rate { start, end, per_tick }` | Level changes by `per_tick` every tick during `[start, end)`. |
| `StateImpact::Require { start, end, value }` (state timelines) | The state must equal `value` during `[start, end)`. |
| `StateImpact::Set { at, value }` (state timelines) | The state becomes `value` at `at`. |

An activity may have several impacts on several timelines. Multi-timeline
plans are a set of timelines, one per resource; a plan-level check runs each
and reports the earliest violation (milestone 3).

Validation on insertion:

- ticks before the timeline start are rejected;
- claims and rates need `start < end`;
- claim amounts must be positive;
- claimable timelines accept claims only (a permanent step on an instrument
  is a modelling error);
- the impact count is limited by the timeline's budget.

## Checks

| Check | Milestone | Description |
| --- | --- | --- |
| Range bounds | 1 | `min <= level(t) <= max` for every tick from the start. Covers capacity (upper) and reserve (lower). |
| Claim capacity | 1 | Range bound with `min = 0` on a claimable timeline. |
| Window containment | 3, 4 | An activity must lie inside an allowed window (comm passes, daylight). Implemented as a state timeline whose value a ground activity sets at each window start and end, plus a `Require` of the activity. |
| End-state level | 3 | `Target { at, min }`: the level at tick `at` must be at least `min` (for example battery charge at the end of a sol). Reported as `Bound::BelowTarget`; part of every check, search and envelope. At the same tick a broken bound wins, among targets the strictest. |
| State requirements | 4 | `Require` intervals must see the required state value throughout; sets of different values at one tick are a conflict. |

The check stops at the first violation in time order. Ties at the same tick
cannot happen per timeline because the level at a tick is a single value.

## Flexible starts (implemented as envelopes)

Implemented in `envelope`: each flexible activity has fixed-duration impacts
relative to its start and a start window `[earliest, latest]` (from the STN).
With the windows treated as independent, the extreme level at tick `t` is
the fixed level plus, for each activity, the extreme of its own summed
profile over the relative times `[t - latest, t - earliest]`. That is the
exact tightest envelope for independent windows. The verdict is
three-valued: `Safe`, `Possible` (some schedule may break a bound at the
reported tick) and `Certain` (every schedule breaks it there). Evaluation
ticks are every shape change and the tick before; without rates the
envelopes are constant in between and the reported tick is the earliest;
with rates the upper envelope is convex and the lower concave between them,
so `Safe` remains exact while the reported tick may be later.

### Precedences

`Precedence { before, from, after, to, gap }` requires
`time(after, to) - time(before, from) >= gap`, where each endpoint is the
start or the end (start-start, start-end, end-start, end-end). The end of a
contingent activity is its start plus its uncertain duration, the end of a
flexible one the start plus the last tick of its impacts. Floyd-Warshall
over the network of starts and contingent ends (windows as edges to an
origin, durations as edges between start and end) tightens every window
and duration range and yields the largest separation of every pair of time
points; inconsistent constraints are an error.

The linked activities get a joint extreme:

- all stepwise (steps and claims): Muscettola's construction, forced events
  plus the heaviest closure of the pending ones under "never later than",
  by a minimum cut. Exact, independent of the window lengths.
- with rates or uncertain durations: every impact is separable in its time
  points (a step an indicator, a claim a difference of two indicators, a
  rate a difference of two clamped ramps), so the extreme is the maximum of
  a separable function under difference constraints. A layered graph with
  one node per domain value (Ishikawa) turns it into one minimum cut (Dinic),
  exact without an LP. Every tick a coupled time point can take becomes an
  evaluation tick.
- the layered graph grows with the windows, so it is built only up to a node
  budget (`with_cut_budget`, default 4096). Above it the activities with
  rates or durations keep separate extremes over their tightened ranges:
  sound, never looser than without precedences, but a `Possible` may be a
  false alarm. This is the remaining gap.

The properties check equality with enumeration of the respecting schedules
for every endpoint combination, with rates and with contingent ends.

### State envelopes

`StateEnvelope` answers the same question for a state timeline. At a tick,
each activity offers options ("its latest own set is the one at offset `o`,
at ticks `[lo, hi]`" or "no set yet"); a requirement is possibly broken if an
option with another value can be the latest (placed as late as possible,
everything else at or before it), certainly broken if it covers the tick
for every start and no option with its value can be the unique latest. The
requirer's own sets move with its start, so its starts are split into
pieces by its own offsets. Conflicts are pairs of differing sets whose tick
ranges meet. "Certain" is per requirement or pair of sets, matching the per
bound meaning for numeric envelopes. Exact for independent windows; the
cost grows with the covered ticks (`O(L m^2)`).

## Uncertain durations (implemented)

Durations in an STNU are intervals `[l, u]`; the actual value is learned at
execution time. Resource checks must be conservative: if the check says
"safe", every allowed outcome must be safe.

Implemented as `envelope::Contingent`: an activity with a start window, a
duration range `[d_lo, d_hi]` and `Uncertain` impacts whose ticks are
anchored at the start (`Anchor::Start(o)`) or at the end
(`Anchor::End(o)`, the start plus the duration plus `o`). A claim from
`Start(0)` to `End(0)` holds a resource exactly as long as the activity
runs, on claimable timelines too; a rate over the same interval runs as
long as the activity. Intervals may be empty at the shortest duration but
never inverted.

The extreme contribution at tick `t` is the extreme of the profile
`g(u, d)` over the box `u in [t - s_hi, t - s_lo]`, `d in [d_lo, d_hi]`. The
profile is linear on the cells cut by the lines `u = o`, `u = d + o` and
their integer neighbours, so the extreme lies at a vertex of that
arrangement; the envelope evaluates exactly those candidates. The result
equals the pointwise extremes over every start and duration (property
test). Durations of different activities are independent of each other.

Deviation from the original plan, which put separate intervals on each
impact's start and end: the activity-level duration couples every impact
of one activity to the same outcome, as an STNU contingent link does, and
is tighter. Precedences may refer to the uncertain end directly.

## Storage (fixed capacity)

Every timeline, state timeline, violation and plan is generic over a
storage strategy. `Heap` keeps each container in a `Vec` with fallible
growth. `Fixed<N>` keeps `N` impacts, sets, requirements, targets and
contributors and `2 N` change points in inline arrays without `unsafe`
(unused slots hold a filler value); `FixedPlan<T, S, N>` holds at most `T`
numeric and `S` state timelines. All algorithms are written once against a
sealed `Seq` trait. A full container reports
`Error::CapacityExceeded { container, capacity }` before anything changes,
so placement stays atomic. Change points are reference-counted, so
removing an impact never inserts a point and stays infallible at full
capacity.

## Encoding

`codec` writes `LevelProfile`, `Timeline` and `StateTimeline` into a
caller's buffer without allocation: a 4-byte magic per type, a version
byte, a reserved flags byte, fixed-size little-endian records and a CRC-32
(IEEE) trailer. Decoding verifies magic, version, length and checksum, then
replays the content through the validating constructors into any storage.
With the `serde` feature the plain data types derive `serde`, `Bounds` and
`Window` validate on deserialization, and timelines and profiles serialize
by content (deserializing them needs `alloc`).

## Stack budget

With `Fixed<N>` the plan search keeps `216 N + 128` bytes of scratch (58 KiB
for `N = 256`) on the stack. `SearchScratch` with `fits_with` and
`earliest_start_with` moves it to caller memory; measured on x86-64, the
search then fits a 16 KiB thread stack. Explaining a violation still keeps a
`32 N`-byte list on the stack.

## Profile extraction

`Timeline::breakpoints` iterates the ticks where the level line jumps or
changes slope (with level and slope), `Timeline::samples` evaluates it on a
regular grid in one pass, and `Timeline::export` produces a `LevelProfile`
(three `i64` per breakpoint) that answers level queries without the impact
list. `StateTimeline::values` lists the value changes. All are
allocation-free on fixed storage.

## Earliest start with rates

Between consecutive candidate starts (where a demand tick meets or passes a
change tick or target) the order of all ticks is fixed, so the merged level
line has the same segments for every start, and the level at every segment
start, at the tick before every segment end and at every target is affine
in the start. The search sweeps the line at the piece's first start and
one tick later, reads values and integer coefficients off the pair and
solves the inequalities in closed form. The per-tick scan is kept as the
test oracle. `max_checks` counts pieces.

## Violation explanations

A violation report contains:

- `at`: the first tick at which the level is out of bounds;
- `level`: the level at that tick;
- `bound`: which bound is broken and its value (`BelowMin(min)` or
  `AboveMax(max)`);
- `contributors`: the activities whose net effect in force at `at` pushes
  in the direction of the violation (net negative for `BelowMin`, net
  positive for `AboveMax`), sorted by id, without duplicates.

"In force at `at`" means: steps with `at_step <= at`, claims with
`start <= at < end`. Contributions are netted per activity first, so an
activity that consumes and later produces the same amount does not appear.

A violation at the start tick with no contributors means the initial level
is already out of bounds.

Later milestones extend explanations with per-contributor amounts and a
minimal subset ("removing any one of these activities resolves the
violation") for the replanner.

## Interface towards temporal-control

No Cargo dependency in either direction. The shared contract is:

- time is an `i64` tick with the same unit and epoch on both sides;
- intervals are half-open;
- activities are identified by ids that the integrating code maps to
  `temporal-control` time points (start and end events).

The integrating layer (miniplan adapter, flight executive glue) reads a
schedule or the dispatcher's current windows from `temporal-control`, turns
each activity into impacts on the relevant timelines, and calls the check.
With uncertain durations it passes the STNU contingent bounds as impact
intervals (milestone 5).

If both crates later need the same public type, the shared piece goes into
a small common crate only then, per the ecosystem rule for `search-core`.

## Cross-crate needs

- `temporal-control`: agree on `i64` ticks, half-open intervals, and that
  neither crate imposes a tick unit or epoch. If `temporal-control`
  introduces a tick newtype, it should be a transparent wrapper with
  `From<i64>`/`Into<i64>` so integration code converts without loss.
- `temporal-control`: expose contingent link bounds `[l, u]` and the
  current execution windows in a form that can be turned into impact
  intervals (needed for milestone 5).
- `miniplan`: an optional adapter feature that maps PDDL numeric fluents or
  durative-action effects onto timeline impacts. Not before milestone 3.
- `polyzonotope-reach`: thermal and power physics, if needed, come from
  there as conservative bounds that this crate consumes as plain levels.

## Testing strategy

- Property tests (proptest, dev-dependency only) compare the sweep with a
  brute-force oracle that evaluates the closed-form level formula at every
  tick from the start to the last event tick, and the contributor rule
  directly from the impact list. Generators use small tick and amount
  ranges so that coincident events and violations are frequent.
- Order independence: shuffling the insertion order must not change the
  result.
- Unit tests for every error path (budget, overflow, inverted interval,
  impact before start, kind mismatch) and for edge cases (back-to-back
  claims, violation at the start tick, violation persisting forever).
- Later: differential tests against a second implementation for envelopes,
  fuzzing of the impact list, and Kani or similar bounded proofs for the
  overflow handling.

## Milestones

1. **Piecewise-constant timelines** (done): depletable and claimable
   resources, step and claim impacts, `[min, max]` check, first violation
   with contributors, impact budget, checked arithmetic, property tests
   against a tick-by-tick oracle.
2. **Piecewise-linear timelines** (done): rate impacts, exact crossing
   ticks, level queries at any tick, profile extraction for display and
   telemetry.
3. **Plans and windows** (done): a set of timelines checked together,
   atomic placement, earliest-feasible-start search (closed form per piece
   with rates), a greedy priority scheduler, window containment via state
   timelines, end-state level targets.
4. **State timelines** (done): discrete state values, `Set`/`Require`
   impacts, mismatch and conflict explanations.
5. **Uncertain durations** (done): envelopes for flexible starts, for
   contingent durations anchored at the activity end, with precedences, and
   over state timelines; three-valued results.
6. **Incremental checks and fixed-capacity storage** (done): add and remove
   on a sorted change-point profile, `O(n)` re-checks, and the
   allocation-free variant with const-generic capacity.

## Open questions

- Should `fits` mean "no new violation" rather than "no violation at all"
  on the touched timelines? Today an already violated timeline admits
  nothing.
- Is the default cut budget of 4096 nodes right, and should the fallback
  above it rather report an error than a sound but loose envelope?
- Should violation explanations also accept borrowed scratch, to remove the
  last `O(N)` stack buffer?
- Should claimable resources also support fractional claims of a shared
  pool (for example power channels), or is that simply a depletable
  resource with paired steps? Milestone 1 treats both the same way.
- Do we need a timeline end tick (planning horizon) to bound checks of
  rate impacts that never end?
- How much of the explanation (minimal conflict sets) belongs here
  versus in the replanner?
