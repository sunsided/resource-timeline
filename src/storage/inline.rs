//! A fixed-capacity sequence over an inline array, without `unsafe`.
//!
//! [`Inline`] keeps its elements in a [`Buffer`], an array whose unused slots hold
//! [`Slot::EMPTY`]. Insertion and removal rotate the occupied prefix in place, so every
//! operation is safe code with the same complexity as the `Vec` counterpart.

use core::fmt;
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};

use super::{Seq, Slot, sealed};
use crate::{Container, Error};

/// The backing array of an [`Inline`] sequence.
///
/// Implemented for `[T; N]` (capacity `N`) and `[[T; 2]; N]` (capacity `2 N`, used for change
/// points). The trait is sealed.
pub trait Buffer<T: Slot>: Clone + sealed::Sealed {
    /// A buffer whose every slot is [`Slot::EMPTY`].
    const EMPTY: Self;

    /// All slots, used or not.
    fn slots(&self) -> &[T];

    /// All slots, used or not, mutably.
    fn slots_mut(&mut self) -> &mut [T];
}

impl<T: Slot, const N: usize> sealed::Sealed for [T; N] {}

impl<T: Slot, const N: usize> Buffer<T> for [T; N] {
    const EMPTY: Self = [const { T::EMPTY }; N];

    fn slots(&self) -> &[T] {
        self
    }

    fn slots_mut(&mut self) -> &mut [T] {
        self
    }
}

impl<T: Slot, const N: usize> sealed::Sealed for [[T; 2]; N] {}

impl<T: Slot, const N: usize> Buffer<T> for [[T; 2]; N] {
    const EMPTY: Self = [const { [const { T::EMPTY }; 2] }; N];

    fn slots(&self) -> &[T] {
        self.as_flattened()
    }

    fn slots_mut(&mut self) -> &mut [T] {
        self.as_flattened_mut()
    }
}

/// A sequence of at most as many elements as its buffer `B` has slots, stored inline.
///
/// Dereferences to the slice of used slots. Growing beyond the capacity fails with
/// [`Error::CapacityExceeded`] and leaves the sequence unchanged. Equality, hashing and
/// debug output look at the used slots only.
pub struct Inline<T: Slot, B: Buffer<T>> {
    /// The slots. The first `len` are used, the rest hold [`Slot::EMPTY`].
    buffer: B,
    /// The number of used slots. Never more than the number of slots in `buffer`.
    len: usize,
    /// Ties the element type to the sequence.
    element: PhantomData<T>,
}

impl<T: Slot, B: Buffer<T>> Inline<T, B> {
    /// The number of slots.
    fn slot_count(&self) -> usize {
        self.buffer.slots().len()
    }

    /// The error for a sequence that is full.
    fn full(&self, container: Container) -> Error {
        Error::CapacityExceeded {
            container,
            capacity: self.slot_count(),
        }
    }

    /// Replaces the slot at `index` with the filler and returns its previous content.
    fn take(&mut self, index: usize) -> T {
        core::mem::replace(&mut self.buffer.slots_mut()[index], T::EMPTY)
    }
}

impl<T: Slot, B: Buffer<T>> sealed::Sealed for Inline<T, B> {}

impl<T: Slot, B: Buffer<T>> Seq<T> for Inline<T, B> {
    const EMPTY: Self = Self {
        buffer: B::EMPTY,
        len: 0,
        element: PhantomData,
    };

    fn capacity(&self) -> Option<usize> {
        Some(self.slot_count())
    }

    fn try_reserve(&mut self, extra: usize, container: Container) -> Result<(), Error> {
        if self.len.saturating_add(extra) > self.slot_count() {
            return Err(self.full(container));
        }

        Ok(())
    }

    fn try_push(&mut self, item: T, container: Container) -> Result<(), Error> {
        let index = self.len;
        self.try_insert(index, item, container)
    }

    fn try_insert(&mut self, index: usize, item: T, container: Container) -> Result<(), Error> {
        assert!(index <= self.len, "insertion index out of range");
        if self.len == self.slot_count() {
            return Err(self.full(container));
        }

        let len = self.len;
        let slots = self.buffer.slots_mut();
        slots[len] = item;
        slots[index..=len].rotate_right(1);
        self.len += 1;
        Ok(())
    }

    fn remove(&mut self, index: usize) -> T {
        assert!(index < self.len, "removal index out of range");
        let len = self.len;
        self.buffer.slots_mut()[index..len].rotate_left(1);
        self.len -= 1;
        self.take(len - 1)
    }

    fn pop(&mut self) -> Option<T> {
        let last = self.len.checked_sub(1)?;
        self.len = last;
        Some(self.take(last))
    }

    fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        let mut kept = 0;
        for read in 0..self.len {
            if keep(&self.buffer.slots()[read]) {
                self.buffer.slots_mut().swap(kept, read);
                kept += 1;
            }
        }

        self.truncate(kept);
    }

    fn truncate(&mut self, len: usize) {
        while self.len > len {
            self.pop();
        }
    }
}

impl<T: Slot, B: Buffer<T>> Deref for Inline<T, B> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.buffer.slots()[..self.len]
    }
}

impl<T: Slot, B: Buffer<T>> DerefMut for Inline<T, B> {
    fn deref_mut(&mut self) -> &mut [T] {
        let len = self.len;
        &mut self.buffer.slots_mut()[..len]
    }
}

impl<T: Slot, B: Buffer<T>> Default for Inline<T, B> {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl<T: Slot, B: Buffer<T>> Clone for Inline<T, B> {
    fn clone(&self) -> Self {
        Self {
            buffer: self.buffer.clone(),
            len: self.len,
            element: PhantomData,
        }
    }
}

impl<T: Slot, B: Buffer<T>> fmt::Debug for Inline<T, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<T: Slot, B: Buffer<T>> PartialEq for Inline<T, B> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<T: Slot, B: Buffer<T>> Eq for Inline<T, B> {}

impl<T: Slot, B: Buffer<T>> Hash for Inline<T, B> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (**self).hash(state);
    }
}

impl<T: Slot, B: Buffer<T>, const M: usize> PartialEq<[T; M]> for Inline<T, B> {
    fn eq(&self, other: &[T; M]) -> bool {
        **self == *other
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests of the inline sequence against `Vec` semantics.

    use super::Inline;
    use crate::storage::Seq;
    use crate::{ActivityId, Container, Error};

    /// A sequence of three activity ids.
    type Three = Inline<ActivityId, [ActivityId; 3]>;

    /// Shorthand for an activity id.
    const fn id(value: u32) -> ActivityId {
        ActivityId(value)
    }

    /// Pushing beyond the capacity fails with the named container and changes nothing.
    #[test]
    fn push_until_full() {
        let mut seq = Three::EMPTY;
        for value in 0..3 {
            seq.try_push(id(value), Container::Impacts).expect("room");
        }

        assert_eq!(
            seq.try_push(id(9), Container::Impacts),
            Err(Error::CapacityExceeded {
                container: Container::Impacts,
                capacity: 3
            })
        );
        assert_eq!(seq, [id(0), id(1), id(2)]);
        assert_eq!(seq.capacity(), Some(3));
    }

    /// Insert, remove, pop and retain match the `Vec` operations of the same name.
    #[test]
    fn edits_match_vec() {
        let mut seq = Three::EMPTY;
        seq.try_insert(0, id(2), Container::Sets).expect("room");
        seq.try_insert(0, id(0), Container::Sets).expect("room");
        seq.try_insert(1, id(1), Container::Sets).expect("room");
        assert_eq!(seq, [id(0), id(1), id(2)]);

        assert_eq!(seq.remove(1), id(1));
        assert_eq!(seq, [id(0), id(2)]);
        assert_eq!(seq.pop(), Some(id(2)));
        assert_eq!(seq.pop(), Some(id(0)));
        assert_eq!(seq.pop(), None);

        seq.try_extend_from_slice(&[id(4), id(5), id(6)], Container::Sets)
            .expect("room");
        seq.retain(|item| item.0 != 5);
        assert_eq!(seq, [id(4), id(6)]);
    }

    /// Reserving beyond the capacity fails without touching the content.
    #[test]
    fn reserve_checks_the_capacity() {
        let mut seq = Three::EMPTY;
        seq.try_push(id(1), Container::Scratch).expect("room");
        assert!(seq.try_reserve(2, Container::Scratch).is_ok());
        assert_eq!(
            seq.try_reserve(3, Container::Scratch),
            Err(Error::CapacityExceeded {
                container: Container::Scratch,
                capacity: 3
            })
        );
        assert_eq!(
            seq.try_extend_from_slice(&[id(2), id(3), id(4)], Container::Scratch),
            Err(Error::CapacityExceeded {
                container: Container::Scratch,
                capacity: 3
            })
        );
        assert_eq!(seq, [id(1)]);
    }

    /// Removed slots are reset, so equality and hashing ignore stale content.
    #[test]
    fn equality_ignores_unused_slots() {
        let mut a = Three::EMPTY;
        a.try_push(id(7), Container::Impacts).expect("room");
        a.try_push(id(8), Container::Impacts).expect("room");
        a.pop();
        let mut b = Three::EMPTY;
        b.try_push(id(7), Container::Impacts).expect("room");
        assert_eq!(a, b);

        let mut doubled = Inline::<ActivityId, [[ActivityId; 2]; 2]>::EMPTY;
        for value in [3, 1, 3, 3] {
            doubled
                .try_push(id(value), Container::ChangePoints)
                .expect("room");
        }

        doubled.dedup();
        assert_eq!(doubled, [id(3), id(1), id(3)]);
    }
}
