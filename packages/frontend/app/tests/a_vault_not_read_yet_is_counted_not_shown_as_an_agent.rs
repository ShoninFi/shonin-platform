//! A vault the indexer has never read whole is in the snapshot marked stale
//! as of the Unix epoch, with no figures. The app doesn't show it as an agent
//! (it has nothing to show of one) and counts it as not read yet instead.

use app::{data, fixture};

#[test]
fn a_vault_not_read_yet_is_counted_not_shown_as_an_agent() {
    let mut snapshot = fixture::snapshot();
    let listed = snapshot.strategies.len();
    assert!(listed >= 2, "the captured snapshot has vaults to mark");
    let unread = snapshot.strategies[0].slug.clone();
    snapshot.strategies[0].stale_as_of = Some(time::PrimitiveDateTime::new(
        time::OffsetDateTime::UNIX_EPOCH.date(),
        time::OffsetDateTime::UNIX_EPOCH.time(),
    ));
    // A vault kept as last read whole is still shown, marked stale.
    snapshot.strategies[1].stale_as_of = Some(snapshot.as_of);
    data::set(snapshot);

    assert_eq!(data::unread(), 1);
    assert_eq!(data::vaults().len(), listed - 1);
    assert!(data::strategy(&unread).is_none());
}
