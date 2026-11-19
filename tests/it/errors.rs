//! Error paths of construction, insertion and checks.

use resource_timeline::{ActivityId, Bounds, Error, Impact, Timeline};

/// Activity used throughout.
const A: ActivityId = ActivityId(0);

/// A depletable timeline starting at tick 0 with an initial level of 5 in `[0, 10]`.
fn battery(budget: usize) -> Timeline {
    let bounds = Bounds::new(0, 10).expect("valid");
    Timeline::depletable(0, 5, bounds, budget).expect("valid")
}

/// Inverted bounds and negative capacities are rejected.
#[test]
fn invalid_bounds() {
    assert_eq!(
        Bounds::new(2, 1),
        Err(Error::InvalidBounds { min: 2, max: 1 })
    );
    assert_eq!(
        Timeline::claimable(0, -1, 4),
        Err(Error::InvalidBounds { min: 0, max: -1 })
    );
}

/// The impact budget is enforced and leaves the timeline unchanged.
#[test]
fn budget_exhausted() {
    let mut timeline = battery(1);
    timeline
        .add(A, Impact::Step { at: 0, delta: 1 })
        .expect("fits");
    assert_eq!(
        timeline.add(A, Impact::Step { at: 1, delta: 1 }),
        Err(Error::BudgetExhausted { limit: 1 })
    );
    assert_eq!(timeline.impacts().len(), 1);
}

/// Impacts before the timeline start are rejected.
#[test]
fn before_start() {
    let mut timeline = battery(4);
    assert_eq!(
        timeline.add(A, Impact::Step { at: -1, delta: 1 }),
        Err(Error::BeforeStart { at: -1, start: 0 })
    );
    assert_eq!(
        timeline.add(
            A,
            Impact::Claim {
                start: -1,
                end: 3,
                amount: 1
            }
        ),
        Err(Error::BeforeStart { at: -1, start: 0 })
    );
}

/// Empty and inverted claim intervals are rejected.
#[test]
fn empty_interval() {
    let mut timeline = battery(4);
    assert_eq!(
        timeline.add(
            A,
            Impact::Claim {
                start: 3,
                end: 3,
                amount: 1
            }
        ),
        Err(Error::EmptyInterval { start: 3, end: 3 })
    );
    assert_eq!(
        timeline.add(
            A,
            Impact::Claim {
                start: 3,
                end: 2,
                amount: 1
            }
        ),
        Err(Error::EmptyInterval { start: 3, end: 2 })
    );
}

/// Zero and negative claim amounts are rejected.
#[test]
fn non_positive_amount() {
    let mut timeline = battery(4);
    assert_eq!(
        timeline.add(
            A,
            Impact::Claim {
                start: 0,
                end: 1,
                amount: 0
            }
        ),
        Err(Error::NonPositiveAmount { amount: 0 })
    );
    assert_eq!(
        timeline.add(
            A,
            Impact::Claim {
                start: 0,
                end: 1,
                amount: i64::MIN
            }
        ),
        Err(Error::NonPositiveAmount { amount: i64::MIN })
    );
}

/// Claimable timelines reject permanent steps.
#[test]
fn kind_mismatch() {
    let mut instrument = Timeline::claimable(0, 1, 4).expect("valid");
    assert_eq!(
        instrument.add(A, Impact::Step { at: 0, delta: -1 }),
        Err(Error::KindMismatch)
    );
}

/// A violating level that does not fit `i64` is reported as overflow, not wrapped.
#[test]
fn overflowing_violation_level() {
    let bounds = Bounds::new(0, 10).expect("valid");
    let mut timeline = Timeline::depletable(0, 5, bounds, 4).expect("valid");
    timeline
        .add(
            A,
            Impact::Step {
                at: 1,
                delta: i64::MAX,
            },
        )
        .expect("valid");
    assert_eq!(timeline.first_violation(), Err(Error::Overflow));
}
