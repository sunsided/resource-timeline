//! A compact, checksummed byte encoding of profiles and timelines, without allocation.
//!
//! For downlink, persistence across a reset, or handing a timeline to another processor, a
//! [`LevelProfile`], a [`Timeline`] and a [`StateTimeline`] can be written into a caller's
//! byte buffer and read back into any storage. The format is fixed-size per element and
//! little-endian:
//!
//! | Field | Size | Content |
//! | --- | --- | --- |
//! | magic | 4 | `RTPF` (profile), `RTTL` (timeline) or `RTST` (state timeline) |
//! | version | 1 | [`VERSION`] |
//! | flags | 1 | zero, reserved |
//! | body | varies | the fields below |
//! | checksum | 4 | CRC-32 (IEEE) of every byte before it |
//!
//! - profile body: the breakpoint count `n` (`u32`), then `n` breakpoints of `at`, `level`,
//!   `slope` (`i64` each);
//! - timeline body: kind (`u8`, 0 depletable, 1 claimable), start, initial, min, max (`i64`),
//!   budget (`u64`), impact count and target count (`u32`), then each impact as activity
//!   (`u32`), tag (`u8`: 0 step, 1 claim, 2 rate) and three `i64` (`at, delta, 0` for a step,
//!   `start, end, amount` or `start, end, per_tick` otherwise), then each target as `at, min`;
//! - state timeline body: start (`i64`), initial (`u32`), budget (`u64`), impact count
//!   (`u32`), then each impact as activity (`u32`), tag (`u8`: 0 set, 1 require), two `i64`
//!   (`at, 0` or `start, end`) and the value (`u32`).
//!
//! `decode_in` reads into any storage; `decode` is its heap shorthand (feature `alloc`), so
//! that `Timeline::decode(bytes)` needs no type annotation.
//!
//! Decoding checks the magic, version, length and checksum before it looks at the body, then
//! rebuilds the value through the normal, validating constructors, so a decoded timeline obeys
//! every invariant and capacity of its storage. Insertion order is preserved.
//!
//! # Examples
//!
//! ```
//! use resource_timeline::storage::Fixed;
//! use resource_timeline::{ActivityId, Bounds, Impact, Target, Timeline};
//!
//! let mut battery = Timeline::<Fixed<8>>::depletable_in(0, 50, Bounds::new(10, 100)?, 8)?;
//! battery.add(ActivityId(1), Impact::Rate { start: 0, end: 20, per_tick: -2 })?;
//! battery.add_target(Target { at: 30, min: 20 })?;
//!
//! let mut bytes = [0_u8; 256];
//! let written = battery.encode(&mut bytes)?;
//! assert_eq!(written, battery.encoded_len());
//! let restored = Timeline::<Fixed<8>>::decode_in(&bytes[..written])?;
//! assert_eq!(restored, battery);
//!
//! // A flipped bit is caught by the checksum.
//! bytes[20] ^= 1;
//! assert!(Timeline::<Fixed<8>>::decode_in(&bytes[..written]).is_err());
//! # Ok::<(), resource_timeline::Error>(())
//! ```

use crate::storage::{Seq, Storage};
use crate::{
    ActivityId, Bounds, Breakpoint, Container, Error, Impact, Kind, LevelProfile, StateImpact,
    StateTimeline, Target, Timeline,
};

/// The format version written and accepted.
pub const VERSION: u8 = 1;

/// Magic of an encoded [`LevelProfile`].
const PROFILE: [u8; 4] = *b"RTPF";
/// Magic of an encoded [`Timeline`].
const TIMELINE: [u8; 4] = *b"RTTL";
/// Magic of an encoded [`StateTimeline`].
const STATE: [u8; 4] = *b"RTST";

/// Bytes of the magic, version and flags.
const HEADER: usize = 6;
/// Bytes of the checksum.
const CHECKSUM: usize = 4;
/// Bytes of one encoded breakpoint.
const BREAKPOINT: usize = 24;
/// Bytes of one encoded numeric impact.
const IMPACT: usize = 29;
/// Bytes of one encoded target.
const TARGET: usize = 16;
/// Bytes of one encoded state impact.
const STATE_IMPACT: usize = 25;
/// Bytes of the fixed part of a timeline body.
const TIMELINE_FIELDS: usize = 1 + 4 * 8 + 8 + 4 + 4;
/// Bytes of the fixed part of a state timeline body.
const STATE_FIELDS: usize = 8 + 4 + 8 + 4;

impl<S: Storage> LevelProfile<S> {
    /// The number of bytes [`encode`](Self::encode) writes.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        HEADER + 4 + self.len() * BREAKPOINT + CHECKSUM
    }

    /// Writes the profile into `out` and returns the number of bytes written; see the
    /// [`codec`](crate::codec) module for the format.
    ///
    /// # Errors
    ///
    /// [`Error::BufferTooSmall`] if `out` is shorter than [`encoded_len`](Self::encoded_len);
    /// nothing is written then. [`Error::Overflow`] if the breakpoint count exceeds `u32`.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Error> {
        let mut writer = Writer::start(out, self.encoded_len(), PROFILE)?;
        writer.u32(count(self.len())?);
        for point in self.breakpoints() {
            writer.i64(point.at);
            writer.i64(point.level);
            writer.i64(point.slope);
        }

        Ok(writer.finish())
    }

    /// Reads a profile written by [`encode`](Self::encode), into storage `S`.
    ///
    /// # Errors
    ///
    /// - [`Error::Malformed`] if the magic, the length or the content is wrong (breakpoints
    ///   must be strictly increasing in tick).
    /// - [`Error::UnsupportedVersion`] for another format version.
    /// - [`Error::ChecksumMismatch`] if the bytes were corrupted.
    /// - [`Error::CapacityExceeded`] or [`Error::OutOfMemory`] if `S` cannot hold the
    ///   breakpoints.
    pub fn decode_in(bytes: &[u8]) -> Result<Self, Error> {
        let mut reader = Reader::open(bytes, PROFILE)?;
        let len = reader.count(BREAKPOINT)?;
        let mut points = (0..len).map(|_| Breakpoint {
            at: reader.i64_unchecked(),
            level: reader.i64_unchecked(),
            slope: reader.i64_unchecked(),
        });
        let origin = points.next().ok_or(Error::Malformed)?;
        let mut changes = <S::DoubleList<Breakpoint> as Seq<_>>::EMPTY;
        let mut previous = origin.at;
        for point in points {
            if point.at <= previous {
                return Err(Error::Malformed);
            }

            previous = point.at;
            changes.try_push(point, Container::ChangePoints)?;
        }

        reader.end()?;
        Ok(Self::from_parts(origin, changes))
    }
}

impl<S: Storage> Timeline<S> {
    /// The number of bytes [`encode`](Self::encode) writes.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        HEADER
            + TIMELINE_FIELDS
            + self.impacts().len() * IMPACT
            + self.targets().len() * TARGET
            + CHECKSUM
    }

    /// Writes the timeline (kind, start, initial level, bounds, budget, impacts in insertion
    /// order, targets) into `out` and returns the number of bytes written; see the
    /// [`codec`](crate::codec) module for the format and an example.
    ///
    /// # Errors
    ///
    /// [`Error::BufferTooSmall`] if `out` is shorter than [`encoded_len`](Self::encoded_len);
    /// nothing is written then. [`Error::Overflow`] if a count exceeds `u32`.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Error> {
        let mut writer = Writer::start(out, self.encoded_len(), TIMELINE)?;
        writer.u8(match self.kind() {
            Kind::Depletable => 0,
            Kind::Claimable => 1,
        });
        writer.i64(self.start());
        writer.i64(self.initial());
        writer.i64(self.bounds().min());
        writer.i64(self.bounds().max());
        writer.u64(u64::try_from(self.budget()).map_err(|_| Error::Overflow)?);
        writer.u32(count(self.impacts().len())?);
        writer.u32(count(self.targets().len())?);
        for &(activity, impact) in self.impacts() {
            writer.u32(activity.0);
            let (tag, a, b, c) = match impact {
                Impact::Step { at, delta } => (0, at, delta, 0),
                Impact::Claim { start, end, amount } => (1, start, end, amount),
                Impact::Rate {
                    start,
                    end,
                    per_tick,
                } => (2, start, end, per_tick),
            };
            writer.u8(tag);
            writer.i64(a);
            writer.i64(b);
            writer.i64(c);
        }

        for target in self.targets() {
            writer.i64(target.at);
            writer.i64(target.min);
        }

        Ok(writer.finish())
    }

    /// Reads a timeline written by [`encode`](Self::encode), into storage `S`.
    ///
    /// # Errors
    ///
    /// - [`Error::Malformed`], [`Error::UnsupportedVersion`] or [`Error::ChecksumMismatch`]
    ///   as for [`LevelProfile::decode_in`].
    /// - The errors of [`Timeline::claimable_in`], [`Timeline::add`] and
    ///   [`Timeline::add_target`] for the decoded content, including capacity errors if `S`
    ///   is smaller than the encoded timeline.
    pub fn decode_in(bytes: &[u8]) -> Result<Self, Error> {
        let mut reader = Reader::open(bytes, TIMELINE)?;
        reader.need(TIMELINE_FIELDS)?;
        let kind = reader.u8_unchecked();
        let (start, initial) = (reader.i64_unchecked(), reader.i64_unchecked());
        let (min, max) = (reader.i64_unchecked(), reader.i64_unchecked());
        let budget = usize::try_from(reader.u64_unchecked()).map_err(|_| Error::Malformed)?;
        let impacts = usize::try_from(reader.u32_unchecked()).map_err(|_| Error::Malformed)?;
        let targets = usize::try_from(reader.u32_unchecked()).map_err(|_| Error::Malformed)?;
        reader.need(
            impacts
                .saturating_mul(IMPACT)
                .saturating_add(targets.saturating_mul(TARGET)),
        )?;

        let mut timeline = match kind {
            0 => Self::depletable_in(start, initial, Bounds::new(min, max)?, budget)?,
            1 if min == 0 && initial == max => Self::claimable_in(start, max, budget)?,
            _ => return Err(Error::Malformed),
        };
        for _ in 0..impacts {
            let activity = ActivityId(reader.u32_unchecked());
            let tag = reader.u8_unchecked();
            let (a, b, c) = (
                reader.i64_unchecked(),
                reader.i64_unchecked(),
                reader.i64_unchecked(),
            );
            let impact = match tag {
                0 if c == 0 => Impact::Step { at: a, delta: b },
                1 => Impact::Claim {
                    start: a,
                    end: b,
                    amount: c,
                },
                2 => Impact::Rate {
                    start: a,
                    end: b,
                    per_tick: c,
                },
                _ => return Err(Error::Malformed),
            };
            timeline.add(activity, impact)?;
        }

        for _ in 0..targets {
            let target = Target {
                at: reader.i64_unchecked(),
                min: reader.i64_unchecked(),
            };
            timeline.add_target(target)?;
        }

        reader.end()?;
        Ok(timeline)
    }
}

impl<S: Storage> StateTimeline<S> {
    /// The number of bytes [`encode`](Self::encode) writes.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        HEADER + STATE_FIELDS + self.impacts().len() * STATE_IMPACT + CHECKSUM
    }

    /// Writes the state timeline (start, initial value, budget, impacts in insertion order)
    /// into `out` and returns the number of bytes written; see the [`codec`](crate::codec)
    /// module for the format.
    ///
    /// # Errors
    ///
    /// [`Error::BufferTooSmall`] if `out` is shorter than [`encoded_len`](Self::encoded_len);
    /// nothing is written then. [`Error::Overflow`] if the impact count exceeds `u32`.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Error> {
        let mut writer = Writer::start(out, self.encoded_len(), STATE)?;
        writer.i64(self.start());
        writer.u32(self.initial());
        writer.u64(u64::try_from(self.budget()).map_err(|_| Error::Overflow)?);
        writer.u32(count(self.impacts().len())?);
        for &(activity, impact) in self.impacts() {
            writer.u32(activity.0);
            let (tag, a, b, value) = match impact {
                StateImpact::Set { at, value } => (0, at, 0, value),
                StateImpact::Require { start, end, value } => (1, start, end, value),
            };
            writer.u8(tag);
            writer.i64(a);
            writer.i64(b);
            writer.u32(value);
        }

        Ok(writer.finish())
    }

    /// Reads a state timeline written by [`encode`](Self::encode), into storage `S`.
    ///
    /// # Errors
    ///
    /// - [`Error::Malformed`], [`Error::UnsupportedVersion`] or [`Error::ChecksumMismatch`]
    ///   as for [`LevelProfile::decode_in`].
    /// - The errors of [`StateTimeline::add`] for the decoded content, including capacity
    ///   errors if `S` is smaller than the encoded timeline.
    pub fn decode_in(bytes: &[u8]) -> Result<Self, Error> {
        let mut reader = Reader::open(bytes, STATE)?;
        reader.need(STATE_FIELDS)?;
        let start = reader.i64_unchecked();
        let initial = reader.u32_unchecked();
        let budget = usize::try_from(reader.u64_unchecked()).map_err(|_| Error::Malformed)?;
        let impacts = usize::try_from(reader.u32_unchecked()).map_err(|_| Error::Malformed)?;
        reader.need(impacts.saturating_mul(STATE_IMPACT))?;

        let mut timeline = Self::new_in(start, initial, budget);
        for _ in 0..impacts {
            let activity = ActivityId(reader.u32_unchecked());
            let tag = reader.u8_unchecked();
            let (a, b) = (reader.i64_unchecked(), reader.i64_unchecked());
            let value = reader.u32_unchecked();
            let impact = match tag {
                0 if b == 0 => StateImpact::Set { at: a, value },
                1 => StateImpact::Require {
                    start: a,
                    end: b,
                    value,
                },
                _ => return Err(Error::Malformed),
            };
            timeline.add(activity, impact)?;
        }

        reader.end()?;
        Ok(timeline)
    }
}

#[cfg(feature = "alloc")]
impl LevelProfile {
    /// [`decode_in`](Self::decode_in) into heap storage.
    ///
    /// # Errors
    ///
    /// As for [`decode_in`](Self::decode_in).
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        Self::decode_in(bytes)
    }
}

#[cfg(feature = "alloc")]
impl Timeline {
    /// [`decode_in`](Self::decode_in) into heap storage.
    ///
    /// # Errors
    ///
    /// As for [`decode_in`](Self::decode_in).
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        Self::decode_in(bytes)
    }
}

#[cfg(feature = "alloc")]
impl StateTimeline {
    /// [`decode_in`](Self::decode_in) into heap storage.
    ///
    /// # Errors
    ///
    /// As for [`decode_in`](Self::decode_in).
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        Self::decode_in(bytes)
    }
}

/// `len` as a `u32` count.
fn count(len: usize) -> Result<u32, Error> {
    u32::try_from(len).map_err(|_| Error::Overflow)
}

/// Writes into a buffer that was checked to be long enough.
struct Writer<'a> {
    /// The output, exactly as long as the encoding.
    out: &'a mut [u8],
    /// The next byte to write.
    at: usize,
}

impl<'a> Writer<'a> {
    /// Checks the room and writes the header.
    fn start(out: &'a mut [u8], len: usize, magic: [u8; 4]) -> Result<Self, Error> {
        let out = out
            .get_mut(..len)
            .ok_or(Error::BufferTooSmall { needed: len })?;
        let mut writer = Self { out, at: 0 };
        writer.bytes(&magic);
        writer.u8(VERSION);
        writer.u8(0);
        Ok(writer)
    }

    /// Appends raw bytes.
    fn bytes(&mut self, bytes: &[u8]) {
        self.out[self.at..self.at + bytes.len()].copy_from_slice(bytes);
        self.at += bytes.len();
    }

    /// Appends a byte.
    fn u8(&mut self, value: u8) {
        self.bytes(&[value]);
    }

    /// Appends a little-endian `u32`.
    fn u32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }

    /// Appends a little-endian `u64`.
    fn u64(&mut self, value: u64) {
        self.bytes(&value.to_le_bytes());
    }

    /// Appends a little-endian `i64`.
    fn i64(&mut self, value: i64) {
        self.bytes(&value.to_le_bytes());
    }

    /// Appends the checksum and returns the total length.
    fn finish(mut self) -> usize {
        let checksum = crc32(&self.out[..self.at]);
        self.u32(checksum);
        self.at
    }
}

/// Reads a buffer whose header, length and checksum were verified.
struct Reader<'a> {
    /// The body, without header and checksum.
    body: &'a [u8],
    /// The next byte to read.
    at: usize,
}

impl<'a> Reader<'a> {
    /// Verifies the magic, the version and the checksum, and positions after the header.
    fn open(bytes: &'a [u8], magic: [u8; 4]) -> Result<Self, Error> {
        if bytes.len() < HEADER + CHECKSUM || bytes[..4] != magic {
            return Err(Error::Malformed);
        }

        if bytes[4] != VERSION {
            return Err(Error::UnsupportedVersion { version: bytes[4] });
        }

        if bytes[5] != 0 {
            return Err(Error::Malformed);
        }

        let (content, checksum) = bytes.split_at(bytes.len() - CHECKSUM);
        let expected = u32::from_le_bytes(checksum.try_into().map_err(|_| Error::Malformed)?);
        if crc32(content) != expected {
            return Err(Error::ChecksumMismatch);
        }

        Ok(Self {
            body: &content[HEADER..],
            at: 0,
        })
    }

    /// Fails unless `len` more bytes are left.
    fn need(&self, len: usize) -> Result<(), Error> {
        if self.body.len() - self.at < len {
            return Err(Error::Malformed);
        }

        Ok(())
    }

    /// Reads an element count and checks that that many elements of `size` bytes follow.
    fn count(&mut self, size: usize) -> Result<usize, Error> {
        self.need(4)?;
        let len = usize::try_from(self.u32_unchecked()).map_err(|_| Error::Malformed)?;
        self.need(len.saturating_mul(size))?;
        Ok(len)
    }

    /// Fails unless every byte was read.
    fn end(&self) -> Result<(), Error> {
        if self.at == self.body.len() {
            Ok(())
        } else {
            Err(Error::Malformed)
        }
    }

    /// The next `N` bytes; [`need`](Self::need) must have checked them.
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0; N];
        out.copy_from_slice(&self.body[self.at..self.at + N]);
        self.at += N;
        out
    }

    /// The next byte.
    fn u8_unchecked(&mut self) -> u8 {
        self.take::<1>()[0]
    }

    /// The next little-endian `u32`.
    fn u32_unchecked(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }

    /// The next little-endian `u64`.
    fn u64_unchecked(&mut self) -> u64 {
        u64::from_le_bytes(self.take())
    }

    /// The next little-endian `i64`.
    fn i64_unchecked(&mut self) -> i64 {
        i64::from_le_bytes(self.take())
    }
}

/// The CRC-32 lookup table (IEEE 802.3, reflected polynomial `0xEDB8_8320`).
const CRC_TABLE: [u32; 256] = {
    let mut table = [0_u32; 256];
    let (mut index, mut seed) = (0_usize, 0_u32);
    while index < 256 {
        let mut value = seed;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 1 {
                (value >> 1) ^ 0xEDB8_8320
            } else {
                value >> 1
            };
            bit += 1;
        }

        table[index] = value;
        index += 1;
        seed += 1;
    }

    table
};

/// The CRC-32 (IEEE) of `bytes`, as used by zlib and Ethernet.
#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(byte)) & 0xFF) as usize];
    }

    !crc
}

#[cfg(test)]
mod tests {
    //! The checksum against the standard check value.

    use super::crc32;

    /// The CRC-32 check value of the ASCII digits 1 to 9.
    #[test]
    fn check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
