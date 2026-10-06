//! A withdrawal on its way is counted once, however often the explorer's
//! list names it.
//!
//! The list is read in pages by offset, newest first. A withdrawal carried
//! out between two page reads pushes every older one down by one, so the
//! last entry of one page comes back as the first of the next; and a list
//! filtered by two log kinds may name one transaction under both. Either
//! way the same hash reaches the tally twice. The dollars left the account
//! once, so the post names them once: named twice, the station would be
//! valued at that much more than it holds until the withdrawal's batch is
//! executed.

use shonin_lighter::withdrawals::{DOLLAR_ASSET, OnTheirWay, Withdrawal};

fn withdrawal(hash: &str, height: u64, applied_us: u64, amount: u128) -> Withdrawal {
    Withdrawal {
        hash: hash.to_string(),
        height,
        applied_us,
        asset: DOLLAR_ASSET,
        amount,
        failed: false,
    }
}

#[test]
fn a_withdrawal_the_list_names_twice_is_counted_once() {
    // The chain has credited everything up to L2 block 100; the account was
    // read at 1_000.
    let mut tally = OnTheirWay::new(1_000, 100);
    // 5,000 dollars carried out in block 120, before the read, in a batch
    // not yet executed: on their way.
    let on_its_way = withdrawal("aa11", 120, 900, 5_000_000_000);

    // The last entry of one page...
    tally.take(&on_its_way).unwrap();
    // ...and, the list having grown by one meanwhile, the first of the next.
    tally.take(&on_its_way).unwrap();
    // The older, credited withdrawal that ends the read.
    tally.take(&withdrawal("bb22", 90, 500, 1)).unwrap();

    assert!(tally.done());
    assert_eq!(
        tally.total(),
        5_000_000_000,
        "5,000 dollars left the account once"
    );
}
