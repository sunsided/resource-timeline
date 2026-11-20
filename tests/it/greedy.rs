//! The greedy scheduler: hand-checked order and generated request sets.

use proptest::prelude::*;
use resource_timeline::greedy::{Request, schedule};
use resource_timeline::{ActivityId, Demand, Window};

use crate::plan::{demand, empty_plan};

/// Generated requests: id, priority, window start and length, and demands.
type Generated = Vec<(u32, i64, i64, Vec<Demand>)>;

/// Up to six requests with random priorities and windows.
fn requests() -> impl Strategy<Value = Generated> {
    prop::collection::vec(
        (
            0_u32..4,
            0_i64..20,
            0_i64..15,
            prop::collection::vec(demand(true), 1..4),
        ),
        0..6,
    )
}

proptest! {
    /// Every placed request starts in its window, the result is violation-free, and each
    /// placement is the earliest feasible start given the requests placed before it.
    #[test]
    fn greedy_is_consistent(generated in requests()) {
        let (mut plan, _) = empty_plan();
        let requests: Vec<Request<'_>> = (0_u32..)
            .zip(&generated)
            .map(|(id, (priority, earliest, length, demands))| Request {
                activity: ActivityId(id),
                priority: *priority,
                window: Window::new(*earliest, earliest + length).expect("valid"),
                demands,
            })
            .collect();

        let starts = schedule(&mut plan, &requests, 100_000).expect("small");
        prop_assert_eq!(plan.first_violation(), Ok(None));

        let mut order: Vec<usize> = (0..requests.len()).collect();
        order.sort_by_key(|&i| (core::cmp::Reverse(requests[i].priority), requests[i].activity));
        let (mut replay, _) = empty_plan();
        for i in order {
            let request = &requests[i];
            let expected = request
                .window
                .earliest()
                .max(0)
                .min(request.window.latest() + 1);
            let first = (expected..=request.window.latest())
                .find(|&start| replay.fits(request.demands, start).expect("small"));
            prop_assert_eq!(starts[i], first);
            if let Some(start) = first {
                prop_assert!(request.window.contains(start));
                replay.place(request.activity, start, request.demands).expect("valid");
            }
        }
    }
}
