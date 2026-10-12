//! The store takes each snapshot in field by field: what didn't change isn't
//! touched, so a page bound to it redraws only the figures that moved, and a
//! list keeps its order until someone asks for another.

use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use app::model::Group;
use app::pages::agents::{Filters, ListTab, Sort, SortKey, select};
use app::store::Store;
use futures::Stream;
use futures::task::noop_waker_ref;
use futures_signals::signal::{Mutable, SignalExt};

fn snapshot() -> shared::Snapshot {
    serde_json::from_str(include_str!("snapshot.json")).expect("the captured snapshot decodes")
}

/// A signal's changes, as a stream that answers at once whether one is
/// waiting.
struct Changes<S>(Pin<Box<S>>);

impl<S: Stream + Unpin> Changes<S> {
    fn pending(&mut self) -> usize {
        let mut context = Context::from_waker(noop_waker_ref());
        let mut count = 0;
        while let Poll::Ready(Some(_)) = self.0.as_mut().poll_next(&mut context) {
            count += 1;
        }
        count
    }
}

fn watch<T: Clone + 'static>(value: &Mutable<T>) -> Changes<impl Stream<Item = T> + Unpin> {
    Changes(Box::pin(value.signal_cloned().to_stream()))
}

fn bump(snapshot: &mut shared::Snapshot) {
    snapshot.version += 1;
}

#[test]
fn a_new_snapshot_touches_only_the_fields_that_changed() {
    let store = Store::new();
    let mut snapshot = snapshot();
    assert!(store.apply(&snapshot));
    let slug = snapshot.strategies[0].slug.clone();
    let agent = store.agent(&slug).expect("the agent is in the store");

    let mut title = watch(&agent.title);
    let mut in_vault = watch(&agent.in_vault);
    assert_eq!(title.pending(), 1, "a signal starts with its value");
    assert_eq!(in_vault.pending(), 1);

    bump(&mut snapshot);
    snapshot.strategies[0].vault.value += 1_000.0;
    assert!(store.apply(&snapshot));

    assert_eq!(in_vault.pending(), 1, "the money in the vault changed");
    assert_eq!(
        title.pending(),
        0,
        "the title didn't, so nothing redraws it"
    );
    let same = store.agent(&slug).unwrap();
    assert!(
        Rc::ptr_eq(&agent, &same),
        "the agent is updated in place, never replaced"
    );
}

#[test]
fn a_snapshot_already_held_changes_nothing() {
    let store = Store::new();
    let snapshot = snapshot();
    assert!(store.apply(&snapshot));
    let members = store.membership.get();
    assert!(!store.apply(&snapshot));
    assert_eq!(store.membership.get(), members);
}

#[test]
fn a_figure_moving_never_reorders_a_list_but_an_agent_leaving_does() {
    let store = Store::new();
    let mut snapshot = snapshot();
    store.apply(&snapshot);
    let members = store.membership.get();

    bump(&mut snapshot);
    for strategy in &mut snapshot.strategies {
        strategy.vault.value *= 2.0;
    }
    store.apply(&snapshot);
    assert_eq!(
        store.membership.get(),
        members,
        "figures moved; the lists stay as they are"
    );

    bump(&mut snapshot);
    let gone = snapshot.strategies.remove(0).slug;
    store.apply(&snapshot);
    assert!(
        store.membership.get() > members,
        "an agent left, so lists are redrawn"
    );
    assert!(store.agent(&gone).is_none());
}

#[test]
fn a_vault_not_read_yet_is_counted_not_shown_as_an_agent() {
    let store = Store::new();
    let mut snapshot = snapshot();
    let listed = snapshot.strategies.len();
    assert!(listed >= 2, "the captured snapshot has vaults to mark");
    let unread = snapshot.strategies[0].slug.clone();
    snapshot.strategies[0].stale_as_of = Some(time::PrimitiveDateTime::new(
        time::OffsetDateTime::UNIX_EPOCH.date(),
        time::OffsetDateTime::UNIX_EPOCH.time(),
    ));
    // A vault kept as last read whole is still shown, marked stale.
    let stale = snapshot.strategies[1].slug.clone();
    snapshot.strategies[1].stale_as_of = Some(snapshot.as_of);
    store.apply(&snapshot);

    assert_eq!(store.unread.get(), 1);
    assert_eq!(store.all().len(), listed - 1);
    assert!(store.agent(&unread).is_none());
    assert_eq!(
        store.agent(&stale).unwrap().stale_as_of.get(),
        Some(snapshot.as_of),
        "a stale vault says when its figures are from"
    );
}

#[test]
fn sorting_by_money_in_vault_puts_the_largest_first_and_can_be_reversed() {
    let store = Store::new();
    let mut snapshot = snapshot();
    // Make every agent ranked, so the open list has them all.
    for strategy in &mut snapshot.strategies {
        strategy.standing = shared::Standing::Listed;
    }
    store.apply(&snapshot);
    let agents = store.all();
    let sort = Sort {
        key: SortKey::InVault,
        descending: true,
    };
    let rows = select(&agents, ListTab::Open, None, &Filters::default(), "", sort);
    let open = agents
        .iter()
        .filter(|agent| agent.group.get() == Group::Open)
        .count();
    assert!(open > 1, "the captured snapshot has open vaults to sort");
    assert_eq!(rows.len(), open);
    for pair in rows.windows(2) {
        assert!(pair[0].in_vault.get() >= pair[1].in_vault.get());
    }
    let rows = select(
        &agents,
        ListTab::Open,
        None,
        &Filters::default(),
        "",
        Sort {
            descending: false,
            ..sort
        },
    );
    for pair in rows.windows(2) {
        assert!(pair[0].in_vault.get() <= pair[1].in_vault.get());
    }
}

#[test]
fn an_agent_with_no_30_day_figure_sorts_last_whichever_way() {
    let store = Store::new();
    let mut snapshot = snapshot();
    for strategy in &mut snapshot.strategies {
        strategy.standing = shared::Standing::Listed;
    }
    store.apply(&snapshot);
    let agents = store.all();
    for descending in [true, false] {
        let rows = select(
            &agents,
            ListTab::Open,
            None,
            &Filters::default(),
            "",
            Sort {
                key: SortKey::Return30,
                descending,
            },
        );
        let first_missing = rows
            .iter()
            .position(|agent| agent.figures.get().return_30d.is_none())
            .unwrap_or(rows.len());
        assert!(
            rows[first_missing..]
                .iter()
                .all(|agent| agent.figures.get().return_30d.is_none()),
            "once one is missing, the rest are too"
        );
    }
}

#[test]
fn the_new_list_holds_only_new_agents_and_search_finds_by_name_or_author() {
    let store = Store::new();
    let snapshot = snapshot();
    store.apply(&snapshot);
    let agents = store.all();
    let new = select(
        &agents,
        ListTab::New,
        None,
        &Filters::default(),
        "",
        Sort::default(),
    );
    assert!(new.iter().all(|agent| agent.group.get() == Group::New));

    let name = agents[0].title.get_cloned();
    let found = select(
        &agents,
        agents[0].group.get().list(),
        None,
        &Filters::default(),
        &name.to_uppercase(),
        Sort::default(),
    );
    assert!(found.iter().any(|agent| Rc::ptr_eq(agent, &agents[0])));
    let none = select(
        &agents,
        ListTab::New,
        None,
        &Filters::default(),
        "no agent is called this",
        Sort::default(),
    );
    assert!(none.is_empty());
}

#[test]
fn an_author_link_narrows_the_lists_to_that_author_on_that_chain() {
    let store = Store::new();
    let snapshot = snapshot();
    store.apply(&snapshot);
    let agents = store.all();
    let key = agents[0]
        .author_key
        .get_cloned()
        .expect("the author has an address");
    let tab = agents[0].group.get().list();
    let mine = select(
        &agents,
        tab,
        Some(&key),
        &Filters::default(),
        "",
        Sort::default(),
    );
    assert!(!mine.is_empty());
    assert!(
        mine.iter()
            .all(|agent| agent.author_key.get_cloned().as_deref() == Some(key.as_str()))
    );
    let nobody = select(
        &agents,
        tab,
        Some("base:0x0000000000000000000000000000000000000001"),
        &Filters::default(),
        "",
        Sort::default(),
    );
    assert!(nobody.is_empty());
}

/// The list a group's agents are shown in.
trait List {
    fn list(self) -> ListTab;
}

impl List for Group {
    fn list(self) -> ListTab {
        match self {
            Group::Open => ListTab::Open,
            Group::New => ListTab::New,
            Group::Closing | Group::Hidden => ListTab::Closing,
        }
    }
}
