//! RISC Zero's Groth16 seal, checked with Solana's alt_bn128 syscalls.
//!
//! A RISC Zero receipt compressed to Groth16 is a proof for one circuit, the
//! same one RISC Zero's EVM verifier checks (`risc0-ethereum` 3.0.1, the
//! version the EVM checker uses), whose five public inputs are the control
//! root (in two 128-bit halves), the receipt claim's digest (in two halves),
//! and the BN254 control id. The claim is "this guest image ran to a halt
//! and committed a journal with this digest, assuming nothing". This module
//! rebuilds that claim, prepares the inputs, and runs the pairing check, with
//! the verifying key and control ids fixed below.
//!
//! The seal is the EVM's: a 4-byte selector, then `a` (G1), `b` (G2) and `c`
//! (G1), each coordinate a 32-byte big-endian number and `b` in the order
//! the alt_bn128 precompiles take. The selector is the first four bytes of
//! the hash of the verifier's parameters (the control root, the BN254 control
//! id and the verifying key), so a seal made for any other parameters is
//! refused before any curve arithmetic. [`selector`] computes it from the
//! constants here, and a test holds it to the selector RISC Zero's prover
//! puts on real seals, which checks every constant at once.

use solana_bn254::prelude::{alt_bn128_addition, alt_bn128_multiplication, alt_bn128_pairing};
use solana_sha256_hasher::hashv;

/// A 4-byte selector, then the 256-byte proof.
pub const SEAL_LENGTH: usize = 260;

/// The set of RISC Zero recursion circuits accepted (RISC Zero 3.0).
pub const CONTROL_ROOT: [u8; 32] =
    hex32("a54dc85ac99f851c92d7c96d7318af41dbe7c0194edfcc37eb4d422a998c1f56");
/// The BN254 control id, as a big-endian number (the byte order RISC Zero's
/// EVM verifier takes it in).
pub const BN254_CONTROL_ID: [u8; 32] =
    hex32("04446e66d300eb7fb45c9726bb53c793dda407a62e9601618bb43c5c14657ac0");

/// The base field's modulus.
const Q: [u8; 32] = hex32("30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47");
/// The scalar field's modulus: every public input must be below it.
const R: [u8; 32] = hex32("30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001");

// RISC Zero's Groth16 verifying key (the 3.0 circuits), each point's
// coordinates big-endian, a G2 point's in the precompiles' order.
const ALPHA: [u8; 64] = hex64(
    "2d4d9aa7e302d9df41749d5507949d05dbea33fbb16c643b22f599a2be6df2e2\
     14bedd503c37ceb061d8ec60209fe345ce89830a19230301f076caff004d1926",
);
const BETA: [u8; 128] = hex128(
    "0967032fcbf776d1afc985f88877f182d38480a653f2decaa9794cbc3bf3060c\
     0e187847ad4c798374d0d6732bf501847dd68bc0e071241e0213bc7fc13db7ab\
     304cfbd1e08a704a99f5e847d93f8c3caafddec46b7a0d379da69a4d112346a7\
     1739c1b1a457a8c7313123d24d2f9192f896b7c63eea05a9d57f06547ad0cec8",
);
const GAMMA: [u8; 128] = hex128(
    "198e9393920d483a7260bfb731fb5d25f1aa493335a9e71297e485b7aef312c2\
     1800deef121f1e76426a00665e5c4479674322d4f75edadd46debd5cd992f6ed\
     090689d0585ff075ec9e99ad690c3395bc4b313370b38ef355acdadcd122975b\
     12c85ea5db8c6deb4aab71808dcb408fe3d1e7690c43d37b4ce6cc0166fa7daa",
);
const DELTA: [u8; 128] = hex128(
    "03b03cd5effa95ac9bee94f1f5ef907157bda4812ccf0b4c91f42bb629f83a1c\
     1aa085ff28179a12d922dba0547057ccaae94b9d69cfaa4e60401fea7f3e0333\
     110c10134f200b19f6490846d518c9aea868366efb7228ca5c91d2940d030762\
     1e60f31fcbf757e837e867178318832d0b2d74d59e2fea1c7142df187d3fc6d3",
);
const IC: [[u8; 64]; 6] = [
    hex64(
        "12ac9a25dcd5e1a832a9061a082c15dd1d61aa9c4d553505739d0f5d65dc3be4\
         025aa744581ebe7ad91731911c898569106ff5a2d30f3eee2b23c60ee980acd4",
    ),
    hex64(
        "0707b920bc978c02f292fae2036e057be54294114ccc3c8769d883f688a1423f\
         2e32a094b7589554f7bc357bf63481acd2d55555c203383782a4650787ff6642",
    ),
    hex64(
        "0bca36e2cbe6394b3e249751853f961511011c7148e336f4fd974644850fc347\
         2ede7c9acf48cf3a3729fa3d68714e2a8435d4fa6db8f7f409c153b1fcdf9b8b",
    ),
    hex64(
        "1b8af999dbfbb3927c091cc2aaf201e488cbacc3e2c6b6fb5a25f9112e04f2a7\
         2b91a26aa92e1b6f5722949f192a81c850d586d81a60157f3e9cf04f679cccd6",
    ),
    hex64(
        "2b5f494ed674235b8ac1750bdfd5a7615f002d4a1dcefeddd06eda5a076ccd0d\
         2fe520ad2020aab9cbba817fcbb9a863b8a76ff88f14f912c5e71665b2ad5e82",
    ),
    hex64(
        "0f1c3c0d5d9da0fa03666843cde4e82e869ba5252fce3c25d5940320b1c4d493\
         214bfcff74f425f6fe8c0d07b307482d8bc8bb2f3608f68287aa01bd0b69e809",
    ),
];

/// The digest of a halted system state with no memory: every halted
/// receipt's post-state.
const SYSTEM_STATE_ZERO_DIGEST: [u8; 32] =
    hex32("a3acc27117418996340b84e5a90f3ef4c49d22c79e44aad822ec9c313e1eb8e2");

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    hashv(parts).to_bytes()
}

/// RISC Zero's tagged struct hash: `sha256(sha256(tag) ‖ down… ‖ data ‖
/// len(down) as u16 little-endian)`.
fn tagged(tag: &str, down: &[&[u8; 32]], data: &[u8]) -> [u8; 32] {
    let tag = sha256(&[tag.as_bytes()]);
    let len = (down.len() as u16).to_le_bytes();
    let mut parts: Vec<&[u8]> = Vec::with_capacity(down.len() + 3);
    parts.push(&tag);
    for word in down {
        parts.push(word.as_slice());
    }
    parts.push(data);
    parts.push(&len);
    sha256(&parts)
}

/// The digest of the claim that `image_id` ran to a halt and committed a
/// journal whose `sha256` is `journal_digest`, with no assumptions: the
/// claim a seal for that guest and journal proves.
pub fn claim_digest(image_id: &[u8; 32], journal_digest: &[u8; 32]) -> [u8; 32] {
    let output = tagged("risc0.Output", &[journal_digest, &[0; 32]], &[]);
    // The exit code: halted (system 0, user 0), each a u32 shifted into its
    // top byte and written big-endian.
    let exit = [0u8; 8];
    tagged(
        "risc0.ReceiptClaim",
        &[&[0; 32], image_id, &SYSTEM_STATE_ZERO_DIGEST, &output],
        &exit,
    )
}

/// The selector a seal for these parameters starts with.
pub fn selector() -> [u8; 4] {
    let ic: Vec<[u8; 32]> = IC.iter().map(|point| sha256(&[point])).collect();
    // A tagged list, folded from its end.
    let mut list = [0u8; 32];
    for item in ic.iter().rev() {
        list = tagged("risc0_groth16.VerifyingKey.IC", &[item, &list], &[]);
    }
    let key = tagged(
        "risc0_groth16.VerifyingKey",
        &[
            &sha256(&[&ALPHA]),
            &sha256(&[&BETA]),
            &sha256(&[&GAMMA]),
            &sha256(&[&DELTA]),
            &list,
        ],
        &[],
    );
    let mut control_id = BN254_CONTROL_ID;
    control_id.reverse();
    let parameters = tagged(
        "risc0.Groth16ReceiptVerifierParameters",
        &[&CONTROL_ROOT, &control_id, &key],
        &[],
    );
    [parameters[0], parameters[1], parameters[2], parameters[3]]
}

/// A digest as the circuit takes it: its bytes reversed, read as a 256-bit
/// number, and split into its low and high 128 bits, each a scalar.
fn split(digest: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let mut reversed = *digest;
    reversed.reverse();
    let mut low = [0u8; 32];
    let mut high = [0u8; 32];
    low[16..].copy_from_slice(&reversed[16..]);
    high[16..].copy_from_slice(&reversed[..16]);
    (low, high)
}

/// `Q - y` for a coordinate below `Q`: the y of the point's negation.
fn negate(y: &[u8; 32]) -> Option<[u8; 32]> {
    if y >= &Q {
        return None;
    }
    if y == &[0; 32] {
        return Some([0; 32]);
    }
    let mut out = [0u8; 32];
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let mut digit = Q[i] as i16 - y[i] as i16 - borrow;
        borrow = 0;
        if digit < 0 {
            digit += 256;
            borrow = 1;
        }
        out[i] = digit as u8;
    }
    Some(out)
}

/// Whether `seal` proves that `image_id` committed a journal whose `sha256`
/// is `journal_digest`. It answers, and never fails, whatever it is given.
pub fn verify(seal: &[u8], image_id: &[u8; 32], journal_digest: &[u8; 32]) -> bool {
    check(seal, image_id, journal_digest).unwrap_or(false)
}

fn check(seal: &[u8], image_id: &[u8; 32], journal_digest: &[u8; 32]) -> Option<bool> {
    if seal.len() != SEAL_LENGTH || seal[..4] != selector() {
        return Some(false);
    }
    let proof = &seal[4..];
    let (a, b, c) = (&proof[..64], &proof[64..192], &proof[192..256]);

    let (root_low, root_high) = split(&CONTROL_ROOT);
    let (claim_low, claim_high) = split(&claim_digest(image_id, journal_digest));
    let inputs = [root_low, root_high, claim_low, claim_high, BN254_CONTROL_ID];
    if inputs.iter().any(|input| input >= &R) {
        return Some(false);
    }

    // vk_x = IC[0] + Σ input_i · IC[i + 1]
    let mut vk_x: [u8; 64] = IC[0];
    for (input, point) in inputs.iter().zip(&IC[1..]) {
        let product = alt_bn128_multiplication(&[point.as_slice(), input].concat()).ok()?;
        let sum = alt_bn128_addition(&[product.as_slice(), &vk_x].concat()).ok()?;
        vk_x = sum.try_into().ok()?;
    }

    // e(-a, b) · e(alpha, beta) · e(vk_x, gamma) · e(c, delta) == 1
    let mut minus_a = [0u8; 64];
    minus_a[..32].copy_from_slice(&a[..32]);
    minus_a[32..].copy_from_slice(&negate(a[32..].try_into().ok()?)?);
    let input = [
        minus_a.as_slice(),
        b,
        &ALPHA,
        &BETA,
        &vk_x,
        &GAMMA,
        c,
        &DELTA,
    ]
    .concat();
    let result = alt_bn128_pairing(&input).ok()?;
    let mut one = [0u8; 32];
    one[31] = 1;
    Some(result == one)
}

// Hex constants, decoded at compile time.

const fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => panic!("not lowercase hex"),
    }
}

const fn hex<const N: usize>(text: &str) -> [u8; N] {
    let bytes = text.as_bytes();
    assert!(bytes.len() == 2 * N, "wrong length");
    let mut out = [0u8; N];
    let mut i = 0;
    while i < N {
        out[i] = nibble(bytes[2 * i]) << 4 | nibble(bytes[2 * i + 1]);
        i += 1;
    }
    out
}

const fn hex32(text: &str) -> [u8; 32] {
    hex::<32>(text)
}

const fn hex64(text: &str) -> [u8; 64] {
    hex::<64>(text)
}

const fn hex128(text: &str) -> [u8; 128] {
    hex::<128>(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_selector_is_the_one_risc_zero_puts_on_its_seals() {
        // RISC Zero 3.0's Groth16 selector, as on every seal its prover makes
        // for these parameters (the real token's seal included).
        assert_eq!(selector(), [0x73, 0xc4, 0x57, 0xba]);
    }

    #[test]
    fn a_negated_point_adds_up_to_the_modulus() {
        let y = hex32("0000000000000000000000000000000000000000000000000000000000000001");
        let minus = negate(&y).unwrap();
        assert_eq!(minus[31], 0x46);
        assert_eq!(minus[..31], Q[..31]);
        assert_eq!(negate(&[0; 32]), Some([0; 32]));
        assert_eq!(negate(&Q), None);
    }

    #[test]
    fn a_seal_of_the_wrong_length_or_selector_is_refused() {
        assert!(!verify(&[], &[0; 32], &[0; 32]));
        let mut seal = vec![0u8; SEAL_LENGTH];
        assert!(!verify(&seal, &[0; 32], &[0; 32]));
        seal[..4].copy_from_slice(&selector());
        // Zeros are the points at infinity: well formed, and no proof.
        assert!(!verify(&seal, &[0; 32], &[0; 32]));
        seal.push(0);
        assert!(!verify(&seal, &[0; 32], &[0; 32]));
    }
}
