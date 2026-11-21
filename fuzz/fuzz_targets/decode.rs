//! Decoder: arbitrary bytes, and arbitrary bodies behind a valid header and checksum.
//!
//! Raw bytes almost never pass the CRC, so they only exercise the framing checks. In the
//! framed modes the harness writes a correct magic, version, flags and CRC around
//! fuzzer-chosen body bytes, so the body parser and the validating constructors are reached.
//! Checked for profiles, timelines and state timelines: no panic; the fixed-capacity decoder
//! agrees with the heap decoder, unless it runs out of room first; accepted values
//! survive an encode-decode round trip and answer queries without panicking.

#![no_main]

use libfuzzer_sys::fuzz_target;
use resource_timeline::codec::{VERSION, crc32};
use resource_timeline::storage::Fixed;
use resource_timeline::{Error, LevelProfile, StateTimeline, Timeline};

/// Capacity of the fixed-storage decoders; larger inputs must fail with a capacity error.
const N: usize = 16;

/// Magics of the three encodings, selected by the framed modes.
const MAGICS: [[u8; 4]; 3] = [*b"RTPF", *b"RTTL", *b"RTST"];

fuzz_target!(|data: &[u8]| {
    let framed;
    let bytes = match data.split_first() {
        Some((&mode, body)) if mode % 4 != 0 => {
            framed = frame(MAGICS[usize::from(mode % 4) - 1], body);
            &framed[..]
        }
        _ => data,
    };

    check_profile(bytes);
    check_timeline(bytes);
    check_states(bytes);
});

/// Wraps `body` into a header with `magic` and the current version, and a correct CRC.
fn frame(magic: [u8; 4], body: &[u8]) -> Vec<u8> {
    let mut bytes = magic.to_vec();
    bytes.extend([VERSION, 0]);
    bytes.extend(body);
    bytes.extend(crc32(&bytes).to_le_bytes());
    bytes
}

/// Checks that the fixed decoder agrees with the heap decoder: both accept the same content
/// (`same`) or reject with the same error, unless the fixed one ran out of room first.
fn agree<H, F>(heap: &Result<H, Error>, fixed: &Result<F, Error>, same: impl Fn(&H, &F) -> bool) {
    match (heap, fixed) {
        (_, Err(Error::CapacityExceeded { .. })) => {}
        (Ok(heap), Ok(fixed)) => assert!(same(heap, fixed), "heap and fixed decode differ"),
        (Err(heap), Err(fixed)) => assert_eq!(heap, fixed, "heap and fixed reject differently"),
        (Ok(_), Err(error)) => panic!("fixed rejected what heap accepted: {error}"),
        (Err(error), Ok(_)) => panic!("fixed accepted what heap rejected: {error}"),
    }
}

/// Decodes `bytes` as a level profile and checks the decoders and a round trip.
fn check_profile(bytes: &[u8]) {
    let heap = LevelProfile::decode(bytes);
    let fixed = LevelProfile::<Fixed<N>>::decode_in(bytes);
    agree(&heap, &fixed, |heap, fixed| {
        heap.breakpoints().eq(fixed.breakpoints())
    });

    let Ok(profile) = heap else { return };
    let mut out = vec![0; profile.encoded_len()];
    assert_eq!(profile.encode(&mut out), Ok(out.len()));
    assert_eq!(LevelProfile::decode(&out), Ok(profile.clone()));

    for point in profile.breakpoints() {
        let _ = profile.level_at(point.at);
    }
}

/// Decodes `bytes` as a numeric timeline and checks the decoders, a round trip and that both
/// copies report the same first violation.
fn check_timeline(bytes: &[u8]) {
    let heap = Timeline::decode(bytes);
    let fixed = Timeline::<Fixed<N>>::decode_in(bytes);
    agree(&heap, &fixed, |heap, fixed| {
        heap.impacts() == fixed.impacts() && heap.targets() == fixed.targets()
    });

    let Ok(timeline) = heap else { return };
    let mut out = vec![0; timeline.encoded_len()];
    assert_eq!(timeline.encode(&mut out), Ok(out.len()));
    assert_eq!(Timeline::decode(&out), Ok(timeline.clone()));

    let violation = timeline.first_violation();
    if let Ok(fixed) = fixed {
        let fixed = fixed.first_violation();
        assert_eq!(
            violation
                .as_ref()
                .map(|found| found.as_ref().map(|v| (v.at, v.level, v.bound))),
            fixed
                .as_ref()
                .map(|found| found.as_ref().map(|v| (v.at, v.level, v.bound))),
            "heap and fixed report different violations"
        );
    }

    let _ = timeline.export();
}

/// Decodes `bytes` as a state timeline and checks the decoders and a round trip.
fn check_states(bytes: &[u8]) {
    let heap = StateTimeline::decode(bytes);
    let fixed = StateTimeline::<Fixed<N>>::decode_in(bytes);
    agree(&heap, &fixed, |heap, fixed| {
        heap.impacts() == fixed.impacts()
    });

    let Ok(states) = heap else { return };
    let mut out = vec![0; states.encoded_len()];
    assert_eq!(states.encode(&mut out), Ok(out.len()));
    assert_eq!(StateTimeline::decode(&out), Ok(states.clone()));
    let _ = states.first_violation();
}
