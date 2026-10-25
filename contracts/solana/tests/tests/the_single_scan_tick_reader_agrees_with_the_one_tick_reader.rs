//! The single-scan tick reader `TickArray::each_initialized` agrees with the
//! one-tick reader `TickArray::tick`, over mainnet's five fixed arrays (and
//! random and edge-case ones) and the same arrays laid out as Orca's dynamic
//! kind, from every offset in both directions; a truncated or mis-tagged
//! dynamic array is refused, not read out of bounds.

use anchor_lang::prelude::*;
use shonin_orca_driver::network::WHIRLPOOL_PROGRAM;
use shonin_orca_driver::orca::{TickArray, DYNAMIC_TICK_ARRAY, TICKS_PER_ARRAY};

const FIXED_TICK_LEN: usize = 113;
const DYNAMIC_TICK_LEN: usize = 112;

fn account(data: Vec<u8>, owner: &'static Pubkey) -> &'static AccountInfo<'static> {
    let key: &'static Pubkey = Box::leak(Box::new(Pubkey::new_unique()));
    let lamports: &'static mut u64 = Box::leak(Box::new(1));
    let data: &'static mut [u8] = Box::leak(data.into_boxed_slice());
    Box::leak(Box::new(AccountInfo::new(
        key, false, false, lamports, data, owner, false, 0,
    )))
}

fn fixtures() -> Vec<&'static [u8]> {
    vec![
        include_bytes!(
            "../../programs/shonin-orca-driver/fixtures/mainnet-sol-usdc/tick-array-22880.bin"
        ),
        include_bytes!(
            "../../programs/shonin-orca-driver/fixtures/mainnet-sol-usdc/tick-array-23232.bin"
        ),
        include_bytes!(
            "../../programs/shonin-orca-driver/fixtures/mainnet-sol-usdc/tick-array-23584.bin"
        ),
        include_bytes!(
            "../../programs/shonin-orca-driver/fixtures/mainnet-sol-usdc/tick-array-23936.bin"
        ),
        include_bytes!(
            "../../programs/shonin-orca-driver/fixtures/mainnet-sol-usdc/tick-array-24288.bin"
        ),
    ]
}

/// A fixed array's bytes laid out as Orca's dynamic kind: header (the
/// discriminator, the start, the pool, the bitmap), then one tag byte per
/// tick, the initialized ones followed by their 112 bytes of data.
fn as_dynamic(fixed: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&DYNAMIC_TICK_ARRAY);
    out.extend_from_slice(&fixed[8..12]);
    out.extend_from_slice(&fixed[9_956..9_988]);
    let mut bitmap: u128 = 0;
    let mut ticks = Vec::new();
    for at in 0..TICKS_PER_ARRAY as usize {
        let base = 12 + at * FIXED_TICK_LEN;
        if fixed[base] != 0 {
            bitmap |= 1u128 << at;
            ticks.push(1u8);
            ticks.extend_from_slice(&fixed[base + 1..base + 1 + DYNAMIC_TICK_LEN]);
        } else {
            ticks.push(0u8);
        }
    }
    out.extend_from_slice(&bitmap.to_le_bytes());
    assert_eq!(out.len(), 60);
    out.extend(ticks);
    out
}

/// A fixed array with the initialized ticks `bits` says, each with a
/// distinct net.
fn fixed_with(start: i32, bits: u128) -> Vec<u8> {
    let mut out = vec![0u8; 9_988];
    out[..8].copy_from_slice(&shonin_orca_driver::orca::TICK_ARRAY);
    out[8..12].copy_from_slice(&start.to_le_bytes());
    for at in 0..TICKS_PER_ARRAY as usize {
        if bits & (1u128 << at) != 0 {
            let base = 12 + at * FIXED_TICK_LEN;
            out[base] = 1;
            let net: i128 = (at as i128 + 1) * 1_000 * if at % 3 == 0 { -1 } else { 1 };
            out[base + 1..base + 17].copy_from_slice(&net.to_le_bytes());
        }
    }
    out
}

/// What `each_initialized` hands out from `offset` in a direction, with
/// the reference read tick by tick through `tick`.
fn compare(array: &TickArray, offset: i32, down: bool) {
    let mut got = Vec::new();
    let more = array
        .each_initialized(offset, down, |at, net| {
            got.push((at, net));
            true
        })
        .unwrap();
    assert!(more);
    let mut want = Vec::new();
    let range: Vec<i32> = if down {
        (0..=offset.min(TICKS_PER_ARRAY - 1)).rev().collect()
    } else {
        ((offset.max(-1) + 1).min(TICKS_PER_ARRAY)..TICKS_PER_ARRAY).collect()
    };
    for at in range {
        let tick = array.tick(array.start + at * 4, 4).unwrap();
        if tick.initialized {
            want.push((at, tick.liquidity_net));
        }
    }
    assert_eq!(
        got, want,
        "start {} offset {offset} down {down}",
        array.start
    );
    // Stopping early stops at that one.
    if want.len() > 1 {
        let mut seen = Vec::new();
        let more = array
            .each_initialized(offset, down, |at, net| {
                seen.push((at, net));
                seen.len() < 2
            })
            .unwrap();
        assert!(!more);
        assert_eq!(seen, want[..2].to_vec());
    }
}

#[test]
fn the_single_scan_reads_exactly_the_initialized_ticks_in_order_fixed_and_dynamic() {
    let mut images: Vec<Vec<u8>> = fixtures().into_iter().map(<[u8]>::to_vec).collect();
    let mut seed: u64 = 11;
    for _ in 0..40 {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let hi = seed as u128;
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let bits = (hi << 64 | seed as u128) & ((1u128 << 88) - 1);
        images.push(fixed_with(-352, bits));
    }
    images.push(fixed_with(0, 0));
    images.push(fixed_with(0, (1u128 << 88) - 1));
    images.push(fixed_with(0, 1 | 1 << 87));
    let mut initialized = 0;
    for image in images {
        let fixed = TickArray::load(account(image.clone(), &WHIRLPOOL_PROGRAM))
            .unwrap()
            .unwrap();
        let dynamic = TickArray::load(account(as_dynamic(&image), &WHIRLPOOL_PROGRAM))
            .unwrap()
            .unwrap();
        assert_eq!(
            (fixed.start, fixed.whirlpool),
            (dynamic.start, dynamic.whirlpool)
        );
        for at in 0..TICKS_PER_ARRAY {
            let (f, d) = (
                fixed.tick(fixed.start + at * 4, 4).unwrap(),
                dynamic.tick(fixed.start + at * 4, 4).unwrap(),
            );
            assert_eq!(f, d);
            initialized += usize::from(f.initialized);
        }
        for offset in -2..=TICKS_PER_ARRAY + 1 {
            for down in [true, false] {
                compare(&fixed, offset, down);
                compare(&dynamic, offset, down);
            }
        }
    }
    println!("{initialized} initialized ticks compared");
    assert!(initialized > 300);
}

#[test]
fn a_short_or_mis_tagged_dynamic_array_is_refused_not_read_past_its_end() {
    let image = as_dynamic(fixtures()[1]);
    // Truncated after the header and a few ticks: short of the data its
    // bitmap promises.
    for cut in [60 + 88, 60 + 88 + 50, image.len() - 1] {
        let short = image[..cut].to_vec();
        let array = TickArray::load(account(short, &WHIRLPOOL_PROGRAM))
            .unwrap()
            .unwrap();
        for down in [true, false] {
            let result = array.each_initialized(if down { 87 } else { -1 }, down, |_, _| true);
            assert!(result.is_err(), "cut {cut} down {down}");
        }
    }
    // A tag byte that says uninitialized where the bitmap says initialized.
    let mut wrong = image.clone();
    wrong[60] = 0;
    let array = TickArray::load(account(wrong, &WHIRLPOOL_PROGRAM))
        .unwrap()
        .unwrap();
    assert!(array.each_initialized(87, true, |_, _| true).is_err());
    // Not Orca's: not an array at all.
    let other: &'static Pubkey = Box::leak(Box::new(Pubkey::new_unique()));
    assert!(TickArray::load(account(image, other)).unwrap().is_none());
}
