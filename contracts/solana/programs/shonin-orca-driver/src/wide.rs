//! Unsigned integers of four 64-bit words, with only the operations the
//! driver needs, each worked a word at a time in 64-bit arithmetic.
//!
//! A program pays for every instruction, and general-purpose 256-bit
//! arithmetic (a full product, a division of 256 bits by 256) costs tens of
//! thousands of units an operation there. Every figure here fits a narrower
//! shape: a product of two 128-bit numbers, a division by one word. The
//! tests check each operation against the general one.

use core::cmp::Ordering;

/// Four words, least significant first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct W(pub [u64; 4]);

impl Ord for W {
    fn cmp(&self, other: &Self) -> Ordering {
        for i in (0..4).rev() {
            match self.0[i].cmp(&other.0[i]) {
                Ordering::Equal => continue,
                unequal => return unequal,
            }
        }
        Ordering::Equal
    }
}

impl PartialOrd for W {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// `a * b`, in full.
fn mul_words(a: u64, b: u64) -> (u64, u64) {
    let product = a as u128 * b as u128;
    ((product >> 64) as u64, product as u64)
}

impl W {
    pub const ZERO: W = W([0; 4]);
    pub const MAX: W = W([u64::MAX; 4]);

    pub fn from_u128(value: u128) -> Self {
        W([value as u64, (value >> 64) as u64, 0, 0])
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0; 4]
    }

    /// The value if it fits 128 bits.
    pub fn to_u128(&self) -> Option<u128> {
        (self.0[2] == 0 && self.0[3] == 0).then(|| self.0[0] as u128 | (self.0[1] as u128) << 64)
    }

    /// `a * b` for two 128-bit numbers: always fits.
    pub fn mul(a: u128, b: u128) -> Self {
        let (a0, a1) = (a as u64, (a >> 64) as u64);
        let (b0, b1) = (b as u64, (b >> 64) as u64);
        let mut out = [0u64; 4];
        for (i, x) in [a0, a1].into_iter().enumerate() {
            let mut carry = 0u64;
            for (j, y) in [b0, b1].into_iter().enumerate() {
                let (high, low) = mul_words(x, y);
                let sum = out[i + j] as u128 + low as u128 + carry as u128;
                out[i + j] = sum as u64;
                carry = high + (sum >> 64) as u64;
            }
            out[i + 2] = carry;
        }
        W(out)
    }

    /// `self * m` for a one-word `m`; `None` past four words.
    pub fn mul_word(&self, m: u64) -> Option<Self> {
        let mut out = [0u64; 4];
        let mut carry = 0u64;
        for (i, word) in self.0.iter().enumerate() {
            let (high, low) = mul_words(*word, m);
            let sum = low as u128 + carry as u128;
            out[i] = sum as u64;
            carry = high + (sum >> 64) as u64;
        }
        (carry == 0).then_some(W(out))
    }

    /// `self * 2^(64 * words)`; `None` past four words.
    pub fn shl_words(&self, words: usize) -> Option<Self> {
        if self.0[4 - words..].iter().any(|word| *word != 0) {
            return None;
        }
        let mut out = [0u64; 4];
        out[words..].copy_from_slice(&self.0[..4 - words]);
        Some(W(out))
    }

    /// `self / d` and the remainder, for a one-word `d`.
    pub fn div_word(&self, d: u64) -> (Self, u64) {
        let mut quotient = [0u64; 4];
        let mut remainder = 0u64;
        for i in (0..4).rev() {
            // A word below the divisor with nothing carried divides to
            // nothing: no need to work it out.
            if remainder == 0 && self.0[i] < d {
                remainder = self.0[i];
                continue;
            }
            (quotient[i], remainder) = div_two_words(remainder, self.0[i], d);
        }
        (W(quotient), remainder)
    }

    /// `floor(self * a / 2^128)` for a 128-bit `a`, saturating at 128 bits.
    pub fn mul_shr128(&self, a: u128) -> u128 {
        let (a0, a1) = (a as u64, (a >> 64) as u64);
        let mut out = [0u64; 6];
        for (i, x) in [a0, a1].into_iter().enumerate() {
            let mut carry = 0u64;
            for (j, y) in self.0.into_iter().enumerate() {
                let (high, low) = mul_words(x, y);
                let sum = out[i + j] as u128 + low as u128 + carry as u128;
                out[i + j] = sum as u64;
                carry = high + (sum >> 64) as u64;
            }
            out[i + 4] = carry;
        }
        if out[4] != 0 || out[5] != 0 {
            return u128::MAX;
        }
        out[2] as u128 | (out[3] as u128) << 64
    }

    /// `floor(sqrt(self))`, which fits 128 bits.
    pub fn sqrt(&self) -> u128 {
        if let Some(n) = self.to_u128() {
            return sqrt_u128(n);
        }
        // Digit by digit, two bits at a time from the top: shifts, additions
        // and comparisons only.
        let mut remainder = *self;
        let mut root = W::ZERO;
        let top = (0..4).rev().find(|i| self.0[*i] != 0).unwrap_or(0);
        let high_bit = 63 - self.0[top].leading_zeros();
        let mut bit = W::ZERO;
        bit.0[top] = 1 << (high_bit & !1);
        while !bit.is_zero() {
            let trial = root.add(&bit);
            if remainder >= trial {
                remainder = remainder.sub(&trial);
                root = root.shr(1).add(&bit);
            } else {
                root = root.shr(1);
            }
            bit = bit.shr(2);
        }
        root.to_u128().unwrap_or(u128::MAX)
    }

    fn add(&self, other: &Self) -> Self {
        let mut out = [0u64; 4];
        let mut carry = 0u64;
        for (i, (x, y)) in self.0.iter().zip(&other.0).enumerate() {
            let (sum, c1) = x.overflowing_add(*y);
            let (sum, c2) = sum.overflowing_add(carry);
            out[i] = sum;
            carry = u64::from(c1 || c2);
        }
        W(out)
    }

    fn sub(&self, other: &Self) -> Self {
        let mut out = [0u64; 4];
        let mut borrow = 0u64;
        for (i, (x, y)) in self.0.iter().zip(&other.0).enumerate() {
            let (difference, b1) = x.overflowing_sub(*y);
            let (difference, b2) = difference.overflowing_sub(borrow);
            out[i] = difference;
            borrow = u64::from(b1 || b2);
        }
        W(out)
    }

    fn shr(&self, bits: u32) -> Self {
        debug_assert!(bits > 0 && bits < 64);
        let mut out = [0u64; 4];
        for (i, word) in out.iter_mut().enumerate() {
            let high = if i + 1 < 4 {
                self.0[i + 1] << (64 - bits)
            } else {
                0
            };
            *word = (self.0[i] >> bits) | high;
        }
        W(out)
    }
}

/// `floor(sqrt(n))` of a 128-bit `n`.
pub fn sqrt_u128(n: u128) -> u128 {
    if n < 2 {
        return n;
    }
    // A power of two at or above the root.
    let start = 1u64 << (128 - n.leading_zeros()).div_ceil(2).min(63);
    sqrt_near(n, start)
}

/// `floor(sqrt(n))` of a 128-bit `n`, by Newton's method from `guess`: a
/// few divisions of two words by one when the guess is near the root, as a
/// root of a price moved by a band is near the price's own.
pub fn sqrt_near(n: u128, guess: u64) -> u128 {
    if n < 2 {
        return n;
    }
    let (high, low) = ((n >> 64) as u64, n as u64);
    let mut x = guess.max(1);
    // Newton's step from above converges down to the root; one from below
    // lands above it first.
    for _ in 0..64 {
        // n / x fits a word whenever x is above n / 2^64, which every x past
        // the first step is; a guess too small to divide by is raised.
        if high >= x {
            x = (high as u128 + 1).min(u64::MAX as u128) as u64;
            if high >= x {
                return u64::MAX as u128;
            }
        }
        let (quotient, _) = div_two_words(high, low, x);
        let next = ((x as u128 + quotient as u128) / 2) as u64;
        if next == x || next == x + 1 && quotient < x {
            break;
        }
        x = next;
    }
    // Settle on the floor exactly.
    let mut root = x as u128;
    while root * root > n {
        root -= 1;
    }
    while (root + 1)
        .checked_mul(root + 1)
        .is_some_and(|square| square <= n)
    {
        root += 1;
    }
    root
}

/// `(high * 2^64 + low) / d` and its remainder, for `high < d`, in 64-bit
/// operations only: two 32-bit quotient digits, each estimated from the top
/// of the normalized divisor and corrected.
pub fn div_two_words(high: u64, low: u64, d: u64) -> (u64, u64) {
    debug_assert!(high < d);
    const B: u64 = 1 << 32;
    let shift = d.leading_zeros();
    let d = d << shift;
    let (d1, d0) = (d >> 32, d & (B - 1));
    let top = if shift == 0 {
        high
    } else {
        (high << shift) | (low >> (64 - shift))
    };
    let low = low << shift;
    let (n1, n0) = (low >> 32, low & (B - 1));
    let digit = |top: u64, next: u64| -> (u64, u64) {
        let mut q = top / d1;
        let mut r = top - q * d1;
        while q >= B || q * d0 > B * r + next {
            q -= 1;
            r += d1;
            if r >= B {
                break;
            }
        }
        // What is left, which is below the divisor.
        let left = top
            .wrapping_mul(B)
            .wrapping_add(next)
            .wrapping_sub(q.wrapping_mul(d));
        (q, left)
    };
    let (q1, rest) = digit(top, n1);
    let (q0, rest) = digit(rest, n0);
    (q1 * B + q0, rest >> shift)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::U256;

    #[allow(clippy::manual_div_ceil, clippy::assign_op_pattern)]
    mod u512 {
        uint::construct_uint! {
            pub struct U512(8);
        }
    }
    use u512::U512;

    fn u256(w: W) -> U256 {
        U256(w.0)
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        /// A number of a random width, so small and large both come up.
        fn wide(&mut self) -> u64 {
            let shift = self.next() % 64;
            self.next() >> shift
        }
        fn u128(&mut self) -> u128 {
            (self.wide() as u128) << (self.next() % 64) | self.next() as u128
        }
    }

    #[test]
    fn a_division_of_two_words_is_the_128_bit_division() {
        let mut rng = Rng(0x1234_5678_9abc_def1);
        let mut cases = vec![
            (0u64, 0u64, 1u64),
            (0, u64::MAX, 1),
            (u64::MAX - 1, u64::MAX, u64::MAX),
        ];
        for _ in 0..20_000 {
            let d = rng.wide().max(1);
            let high = rng.next() % d;
            cases.push((high, rng.next(), d));
        }
        for (high, low, d) in cases {
            let n = ((high as u128) << 64) | low as u128;
            let (q, r) = div_two_words(high, low, d);
            assert_eq!(
                (q as u128, r as u128),
                (n / d as u128, n % d as u128),
                "{high} {low} {d}"
            );
        }
    }

    #[test]
    fn every_operation_is_the_general_one() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for _ in 0..5_000 {
            let (a, b) = (rng.u128(), rng.u128());
            let product = W::mul(a, b);
            assert_eq!(u256(product), U256::from(a) * U256::from(b));
            let m = rng.wide();
            let expected = U256::from(a).checked_mul(U256::from(m));
            assert_eq!(W::from_u128(a).mul_word(m).map(u256), expected);
            let d = rng.wide().max(1);
            let (q, r) = product.div_word(d);
            let (q2, r2) = u256(product).div_mod(U256::from(d));
            assert_eq!((u256(q), U256::from(r)), (q2, r2));
            let x = W([rng.next(), rng.next(), rng.next() >> 1, 0]);
            let c = rng.u128();
            let mut words = [0u64; 8];
            words[..4].copy_from_slice(&x.0);
            let full = (U512(words) * U512::from(c)) >> 128;
            let expected = if full > U512::from(u128::MAX) {
                u128::MAX
            } else {
                full.low_u128()
            };
            assert_eq!(x.mul_shr128(c), expected);
            let root = product.sqrt();
            let r = U256::from(root);
            assert!(r * r <= u256(product) && (r + 1) * (r + 1) > u256(product));
            assert_eq!(
                product.cmp(&W::from_u128(a)),
                u256(product).cmp(&U256::from(a))
            );
            assert_eq!(
                W::from_u128(a).shl_words(1).map(u256),
                Some(U256::from(a) << 64)
            );
        }
        assert_eq!(W::MAX.sqrt(), u128::MAX);
        for n in [1u128, 2, 3, 4, 8, 9, u64::MAX as u128, u128::MAX] {
            let r = sqrt_u128(n);
            assert!(r * r <= n && (r + 1).checked_mul(r + 1).is_none_or(|s| s > n));
        }
        let mut rng = Rng(77);
        for _ in 0..5_000 {
            let n = rng.u128();
            let r = sqrt_u128(n);
            assert!(
                r * r <= n && (r + 1).checked_mul(r + 1).is_none_or(|s| s > n),
                "{n}"
            );
            let guess = rng.next();
            let near = sqrt_near(n, guess);
            assert_eq!(near, r, "{n} from {guess}");
        }
        assert_eq!(W::ZERO.sqrt(), 0);
    }
}
