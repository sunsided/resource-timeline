//! Storage strategies: heap-allocated or fixed-capacity, chosen per timeline and plan.
//!
//! Every container a timeline, a state timeline or a plan keeps goes through one of two
//! strategies, selected by a type parameter:
//!
//! | Strategy | Containers | Memory | Exhaustion |
//! | --- | --- | --- | --- |
//! | [`Heap`] (feature `alloc`, the default) | `Vec` | grows on demand, fallibly | [`Error::OutOfMemory`] |
//! | [`Fixed<N>`] | inline arrays of `N` (change points: `2 N`) | fixed, no allocator needed | [`Error::CapacityExceeded`] |
//!
//! Both run the same code: the algorithms are written once against the [`Seq`] trait, and the
//! strategies only differ in how a container grows. A plan picks its strategy through
//! [`PlanStorage`]: [`Heap`] again, or [`FixedPlan<T, S, N>`] for at most `T` numeric and `S`
//! state timelines with [`Fixed<N>`] each.
//!
//! The runtime impact budget of a timeline still applies on top of the capacity: a
//! [`Fixed<16>`] timeline created with budget 8 accepts 8 impacts and reports
//! [`Error::BudgetExhausted`] on the ninth.
//!
//! ```mermaid
//! graph LR
//!     algorithms["Timeline, StateTimeline, Plan logic"] --> seq["Seq: push, insert, remove, retain"]
//!     seq --> heap["Heap: Vec, fallible reserve"]
//!     seq --> fixed["Fixed: Inline array, typed capacity errors"]
//! ```
//!
//! # Examples
//!
//! An allocation-free battery timeline with room for four impacts:
//!
//! ```
//! use resource_timeline::storage::Fixed;
//! use resource_timeline::{ActivityId, Bound, Bounds, Container, Error, Impact, Timeline};
//!
//! let mut battery = Timeline::<Fixed<4>>::depletable_in(0, 50, Bounds::new(10, 100)?, 4)?;
//! battery.add(ActivityId(1), Impact::Rate { start: 0, end: 20, per_tick: -3 })?;
//!
//! let violation = battery.first_violation()?.expect("the reserve is broken");
//! assert_eq!((violation.at, violation.bound), (14, Bound::BelowMin(10)));
//! assert_eq!(&*violation.contributors, [ActivityId(1)]);
//! # Ok::<(), resource_timeline::Error>(())
//! ```
//!
//! Exceeding the capacity is a typed error that names the full container:
//!
//! ```
//! use resource_timeline::storage::Fixed;
//! use resource_timeline::{ActivityId, Container, Error, StateImpact, StateTimeline};
//!
//! // Budget 8, but only room for 2 impacts.
//! let mut mode = StateTimeline::<Fixed<2>>::new_in(0, 0, 8);
//! mode.add(ActivityId(1), StateImpact::Set { at: 1, value: 1 })?;
//! mode.add(ActivityId(1), StateImpact::Set { at: 2, value: 0 })?;
//! assert_eq!(
//!     mode.add(ActivityId(2), StateImpact::Set { at: 3, value: 1 }),
//!     Err(Error::CapacityExceeded { container: Container::Impacts, capacity: 2 })
//! );
//! # Ok::<(), resource_timeline::Error>(())
//! ```

mod inline;

use core::fmt::Debug;
use core::hash::Hash;
use core::ops::{Deref, DerefMut};

pub use inline::{Buffer, Inline};

use crate::{Container, Error, StateTimeline, Timeline};

/// Restricts the storage traits to the implementations in this crate.
mod sealed {
    /// Implemented only inside this crate.
    pub trait Sealed {}
}

/// A value that can live in a fixed-capacity container.
///
/// Unused slots of an [`Inline`] container hold [`EMPTY`](Self::EMPTY), so the container needs
/// neither `unsafe` nor `Default`. The trait is sealed: it is implemented for the crate's own
/// element types only.
pub trait Slot: Clone + Debug + Eq + Hash + sealed::Sealed {
    /// The filler of unused slots. Never observable through a container.
    const EMPTY: Self;
}

/// A growable sequence with fallible growth: the one interface all algorithms use.
///
/// Implemented by `Vec` (heap) and by [`Inline`] (fixed capacity). Reading goes through the
/// slice the sequence dereferences to. Growing names the [`Container`] it fills, so a full
/// inline container reports [`Error::CapacityExceeded`] with that name. The trait is sealed.
pub trait Seq<T: Slot>:
    Deref<Target = [T]> + DerefMut + Default + Clone + Debug + Eq + Hash + sealed::Sealed
{
    /// An empty sequence, usable in `const` contexts.
    const EMPTY: Self;

    /// The fixed number of elements the sequence can hold, or `None` if it grows on demand.
    fn capacity(&self) -> Option<usize>;

    /// Makes room for `extra` more elements, so that the next `extra` insertions cannot fail.
    ///
    /// # Errors
    ///
    /// [`Error::OutOfMemory`] if the heap cannot provide the room,
    /// [`Error::CapacityExceeded`] naming `container` if a fixed capacity is too small. The
    /// sequence is unchanged on error.
    fn try_reserve(&mut self, extra: usize, container: Container) -> Result<(), Error>;

    /// Appends `item`.
    ///
    /// # Errors
    ///
    /// As for [`try_reserve`](Self::try_reserve) with one extra element.
    fn try_push(&mut self, item: T, container: Container) -> Result<(), Error>;

    /// Inserts `item` at `index`, shifting later elements up.
    ///
    /// # Errors
    ///
    /// As for [`try_reserve`](Self::try_reserve) with one extra element.
    ///
    /// # Panics
    ///
    /// If `index > len`.
    fn try_insert(&mut self, index: usize, item: T, container: Container) -> Result<(), Error>;

    /// Removes and returns the element at `index`, shifting later elements down.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    fn remove(&mut self, index: usize) -> T;

    /// Removes and returns the last element, if any.
    fn pop(&mut self) -> Option<T>;

    /// Keeps the elements for which `keep` returns `true`, in order. Visits each element once,
    /// in order.
    fn retain(&mut self, keep: impl FnMut(&T) -> bool);

    /// Shortens the sequence to `len` elements; does nothing if it is not longer.
    fn truncate(&mut self, len: usize);

    /// Removes every element.
    fn clear(&mut self) {
        self.truncate(0);
    }

    /// Removes consecutive repeated elements, as `Vec::dedup` does.
    fn dedup(&mut self)
    where
        T: Copy,
    {
        let mut previous: Option<T> = None;
        self.retain(|&item| {
            let keep = previous != Some(item);
            previous = Some(item);
            keep
        });
    }

    /// Appends every element of `items`.
    ///
    /// # Errors
    ///
    /// As for [`try_reserve`](Self::try_reserve) with `items.len()` extra elements. Nothing is
    /// appended on error.
    fn try_extend_from_slice(&mut self, items: &[T], container: Container) -> Result<(), Error> {
        self.try_reserve(items.len(), container)?;
        for item in items {
            self.try_push(item.clone(), container)?;
        }

        Ok(())
    }
}

/// How a timeline or state timeline stores its impacts, change points and explanations.
///
/// Implemented by [`Heap`] and [`Fixed<N>`]. The trait is sealed.
pub trait Storage: Clone + Copy + Debug + Default + Eq + Hash + sealed::Sealed {
    /// A sequence sized like the impact list: impacts, sets, requirements, contributors.
    type List<T: Slot>: Seq<T>;
    /// A sequence sized like twice the impact list: change points.
    type DoubleList<T: Slot>: Seq<T>;
}

/// How a [`Plan`](crate::Plan) stores its timelines.
///
/// Implemented by [`Heap`] and [`FixedPlan<T, S, N>`]. The trait is sealed.
pub trait PlanStorage: Clone + Copy + Debug + Default + Eq + Hash + sealed::Sealed {
    /// The storage of every timeline in the plan.
    type Line: Storage;
    /// The numeric timelines.
    type Timelines: Seq<Timeline<Self::Line>>;
    /// The state timelines.
    type States: Seq<StateTimeline<Self::Line>>;
}

/// Heap storage: every container is a `Vec` that grows on demand.
///
/// Growth is fallible: allocation failure is reported as [`Error::OutOfMemory`], never as an
/// abort. This is the default storage of every type in the crate.
#[cfg(feature = "alloc")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Heap;

/// Fixed-capacity storage for `N` impacts per timeline, without any allocation.
///
/// Containers are inline arrays: `N` impacts, sets, requirements, targets and contributors,
/// `2 N` change points. Exceeding a capacity is reported as [`Error::CapacityExceeded`] and
/// leaves the timeline unchanged. A `Timeline<Fixed<N>>` occupies roughly `100 N` bytes, so
/// large `N` belong in a `static` or on a dedicated stack.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fixed<const N: usize>;

/// Fixed-capacity plan storage: at most `TIMELINES` numeric and `STATES` state timelines, each
/// with [`Fixed<N>`] storage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FixedPlan<const TIMELINES: usize, const STATES: usize, const N: usize>;

#[cfg(feature = "alloc")]
impl sealed::Sealed for Heap {}

#[cfg(feature = "alloc")]
impl Storage for Heap {
    type List<T: Slot> = alloc::vec::Vec<T>;
    type DoubleList<T: Slot> = alloc::vec::Vec<T>;
}

#[cfg(feature = "alloc")]
impl PlanStorage for Heap {
    type Line = Self;
    type Timelines = alloc::vec::Vec<Timeline<Self>>;
    type States = alloc::vec::Vec<StateTimeline<Self>>;
}

impl<const N: usize> sealed::Sealed for Fixed<N> {}

impl<const N: usize> Storage for Fixed<N> {
    type List<T: Slot> = Inline<T, [T; N]>;
    type DoubleList<T: Slot> = Inline<T, [[T; 2]; N]>;
}

impl<const T: usize, const S: usize, const N: usize> sealed::Sealed for FixedPlan<T, S, N> {}

impl<const T: usize, const S: usize, const N: usize> PlanStorage for FixedPlan<T, S, N> {
    type Line = Fixed<N>;
    type Timelines = Inline<Timeline<Fixed<N>>, [Timeline<Fixed<N>>; T]>;
    type States = Inline<StateTimeline<Fixed<N>>, [StateTimeline<Fixed<N>>; S]>;
}

#[cfg(feature = "alloc")]
impl<T> sealed::Sealed for alloc::vec::Vec<T> {}

#[cfg(feature = "alloc")]
impl<T: Slot> Seq<T> for alloc::vec::Vec<T> {
    const EMPTY: Self = Self::new();

    fn capacity(&self) -> Option<usize> {
        None
    }

    fn try_reserve(&mut self, extra: usize, _container: Container) -> Result<(), Error> {
        Self::try_reserve(self, extra).map_err(|_| Error::OutOfMemory)
    }

    fn try_push(&mut self, item: T, container: Container) -> Result<(), Error> {
        Seq::try_reserve(self, 1, container)?;
        self.push(item);
        Ok(())
    }

    fn try_insert(&mut self, index: usize, item: T, container: Container) -> Result<(), Error> {
        Seq::try_reserve(self, 1, container)?;
        self.insert(index, item);
        Ok(())
    }

    fn remove(&mut self, index: usize) -> T {
        Self::remove(self, index)
    }

    fn pop(&mut self) -> Option<T> {
        Self::pop(self)
    }

    fn retain(&mut self, keep: impl FnMut(&T) -> bool) {
        Self::retain(self, keep);
    }

    fn truncate(&mut self, len: usize) {
        Self::truncate(self, len);
    }
}

/// Implements [`Slot`] for `Copy` element types with the given filler value.
macro_rules! slots {
    ($($ty:ty => $empty:expr),* $(,)?) => {
        $(
            impl sealed::Sealed for $ty {}

            impl Slot for $ty {
                const EMPTY: Self = $empty;
            }
        )*
    };
}

slots! {
    crate::ActivityId => crate::ActivityId(0),
    i64 => 0,
    (crate::ActivityId, crate::Impact) => (crate::ActivityId(0), crate::Impact::Step { at: 0, delta: 0 }),
    (crate::ActivityId, crate::StateImpact) => (crate::ActivityId(0), crate::StateImpact::Set { at: 0, value: 0 }),
    (crate::ActivityId, i128) => (crate::ActivityId(0), 0),
}

pub(crate) use sealed::Sealed;
