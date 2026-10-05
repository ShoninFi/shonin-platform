//! Restricted lists: short codes such as `US` or `SANCTIONED`, zero-padded to
//! 32 bytes, kept sorted and without repeats, as the EVM vault's `Codes`.

use shonin_common::digest::keccak;

/// `code` zero-padded, as `bytes32("code")` lays it out.
pub fn code(text: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..text.len()].copy_from_slice(text.as_bytes());
    out
}

/// The platform's baseline on every vault: US persons and sanctioned persons.
pub fn baseline() -> Vec<[u8; 32]> {
    vec![code("SANCTIONED"), code("US")]
}

/// Adds `code` to `list` in order, unless it is already there; answers
/// whether it was added.
pub fn insert(list: &mut Vec<[u8; 32]>, code: [u8; 32]) -> bool {
    match list.binary_search(&code) {
        Ok(_) => false,
        Err(at) => {
            list.insert(at, code);
            true
        }
    }
}

/// The hash a depositor's statement names: keccak256 of the list's Borsh
/// encoding (its length as a 32-bit little-endian number, then each code),
/// as the strategy's record hashes its own list.
pub fn hash(list: &[[u8; 32]]) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(4 + 32 * list.len());
    bytes.extend_from_slice(&(list.len() as u32).to_le_bytes());
    for code in list {
        bytes.extend_from_slice(code);
    }
    keccak(&[&bytes])
}

/// The list as a depositor reads it: codes, comma-separated.
pub fn text(list: &[[u8; 32]]) -> String {
    list.iter()
        .map(|code| {
            let end = code.iter().position(|b| *b == 0).unwrap_or(32);
            String::from_utf8_lossy(&code[..end]).into_owned()
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// The list a vault for a strategy is created with: the baseline, the
/// strategy's own list, and each driver's.
pub fn merged<'a>(
    record: &[[u8; 32]],
    drivers: impl IntoIterator<Item = &'a [[u8; 32]]>,
) -> Vec<[u8; 32]> {
    let mut list = baseline();
    for code in record {
        insert(&mut list, *code);
    }
    for codes in drivers {
        for code in codes {
            insert(&mut list, *code);
        }
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_stays_sorted_without_repeats() {
        let list = merged(
            &[code("US"), code("CA")],
            [[code("GB"), code("CA")].as_slice()],
        );
        assert_eq!(text(&list), "CA,GB,SANCTIONED,US");
    }

    #[test]
    fn the_hash_is_of_the_borsh_encoding() {
        let list = baseline();
        let borsh = anchor_lang::prelude::borsh::to_vec(&list).unwrap();
        assert_eq!(hash(&list), keccak(&[&borsh]));
    }
}
