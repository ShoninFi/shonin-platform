//! `/agents`: the leaderboard (Leaderboard-Desktop, -Tablet, -Mobile).
//!
//! One list of every live agent: those with 30 days of history ranked by
//! their 30-day return, new ones (under 30 days) marked New and showing the
//! period they have instead. Winding-down and closed agents sit in a quieter
//! section under it. Shown as a real `<table>` from 600px up, sortable by 30
//! days or money in vault with `aria-sort` on the sorted column only, or as
//! a grid of cards (the reader's choice, remembered in this browser); a list
//! of cards on a phone, where sort and filters open as sheets. Rows never
//! reorder because a figure moved: only when someone sorts, filters, or an
//! agent joins or leaves a list.
//!
//! `?author=<chain>:<address>` narrows both lists to one Author.

use std::collections::BTreeSet;
use std::rc::Rc;

use futures_signals::map_ref;
use futures_signals::signal::{Mutable, SignalExt};
use futures_signals::signal_vec::{MutableVec, SignalVecExt};
use shared::Chain;
use ui::icons::Icon;

use crate::atoms::button::{Button, Kind, Size, on_click};
use crate::atoms::check::{checkbox, radio};
use crate::atoms::delta::{delta_from, missing_figure};
use crate::atoms::dialog::{self, Dialog};
use crate::atoms::gauge::gauge;
use crate::atoms::hash::hash_plain;
use crate::atoms::notice::{NoticeTone, notice};
use crate::atoms::skeleton::skeleton;
use crate::atoms::sparkline::sparkline;
use crate::atoms::{Tone, badge, icon, review_chip};
use crate::format;
use crate::model::{AuthorKey, Group, days_until};
use crate::route::DOCS_DEPOSITORS;
use crate::store::{self, Agent};
use crate::view::{El, View, el, svg, text};
use crate::web::app_link;

use super::Page;

/// The two lists: live agents, and those winding down or closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    Live,
    Closing,
}

impl List {
    fn holds(self, group: Group) -> bool {
        match self {
            List::Live => matches!(group, Group::Open | Group::New),
            List::Closing => group == Group::Closing,
        }
    }
}

/// How the live list is drawn from 600px up: a table or a grid of cards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    List,
    Grid,
}

impl Layout {
    const KEY: &'static str = "agents.layout";

    fn word(self) -> &'static str {
        match self {
            Layout::List => "list",
            Layout::Grid => "grid",
        }
    }

    fn from_word(word: &str) -> Option<Self> {
        match word {
            "list" => Some(Layout::List),
            "grid" => Some(Layout::Grid),
            _ => None,
        }
    }

    /// The reader's last choice in this browser, or the table. Storage can
    /// fail or be empty anywhere (a private window, cleared data), and the
    /// pre-render has none: the table then.
    fn remembered() -> Self {
        if !cfg!(target_arch = "wasm32") {
            return Layout::List;
        }
        crate::web::storage_get(Self::KEY)
            .and_then(|word| Self::from_word(&word))
            .unwrap_or(Layout::List)
    }

    fn remember(self) {
        crate::web::storage_set(Self::KEY, self.word());
    }
}

/// The two sortable columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Return30,
    InVault,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    pub key: SortKey,
    pub descending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            key: SortKey::Return30,
            descending: true,
        }
    }
}

impl Sort {
    /// What a screen reader hears: "Sorted by 30 days, highest first".
    fn words(self) -> &'static str {
        match (self.key, self.descending) {
            (SortKey::Return30, true) => "30 days, highest first",
            (SortKey::Return30, false) => "30 days, lowest first",
            (SortKey::InVault, true) => "In vault, largest first",
            (SortKey::InVault, false) => "In vault, smallest first",
        }
    }

    fn short(self) -> &'static str {
        match self.key {
            SortKey::Return30 => "30 days",
            SortKey::InVault => "In vault",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExitFilter {
    #[default]
    Any,
    Direct,
    Served,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReviewFilter {
    #[default]
    Any,
    NoSerious,
    AllFine,
}

/// What narrows the lists.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filters {
    pub chains: BTreeSet<Chain>,
    pub exit: ExitFilter,
    pub review: ReviewFilter,
}

impl Filters {
    fn count(&self) -> usize {
        usize::from(!self.chains.is_empty())
            + usize::from(self.exit != ExitFilter::Any)
            + usize::from(self.review != ReviewFilter::Any)
    }

    fn keeps(&self, agent: &Agent) -> bool {
        if !self.chains.is_empty() && !self.chains.contains(&agent.chain) {
            return false;
        }
        let served = agent.served.get();
        match self.exit {
            ExitFilter::Direct if served => return false,
            ExitFilter::Served if !served => return false,
            _ => {}
        }
        let review = agent.review.lock_ref();
        match self.review {
            ReviewFilter::NoSerious if review.has_serious() => false,
            ReviewFilter::AllFine if *review != crate::model::ReviewSummary::AllFine => false,
            _ => true,
        }
    }
}

/// Whether an agent matches what was typed: its name, or its Author's
/// handle or address.
fn matches_search(agent: &Agent, search: &str) -> bool {
    let search = search.trim().to_lowercase();
    if search.is_empty() {
        return true;
    }
    agent.title.lock_ref().to_lowercase().contains(&search)
        || agent.author.lock_ref().to_lowercase().contains(&search)
        || agent
            .author_key
            .lock_ref()
            .as_ref()
            .is_some_and(|key| key.to_lowercase().contains(&search))
}

/// The rows of one list, in order: the agents in `list` that pass the
/// author, filters and search, sorted.
///
/// Live agents follow `sort`. By 30 days, an agent with 30 days of history
/// goes ahead of every new one (a shorter period isn't comparable), and the
/// new ones follow by how long they have run, longest first. By money in
/// vault, every live agent is compared alike. Winding-down agents go by how
/// soon they stop.
pub fn select(
    agents: &[Rc<Agent>],
    list: List,
    author: Option<&str>,
    filters: &Filters,
    search: &str,
    sort: Sort,
) -> Vec<Rc<Agent>> {
    let mut rows: Vec<Rc<Agent>> = agents
        .iter()
        .filter(|agent| list.holds(agent.group.get()))
        .filter(|agent| {
            author.is_none_or(|key| agent.author_key.lock_ref().as_deref() == Some(key))
        })
        .filter(|agent| filters.keeps(agent) && matches_search(agent, search))
        .cloned()
        .collect();
    let by_name = |a: &Rc<Agent>, b: &Rc<Agent>| a.title.lock_ref().cmp(&b.title.lock_ref());
    let directed = |order: std::cmp::Ordering| {
        if sort.descending {
            order.reverse()
        } else {
            order
        }
    };
    match list {
        List::Live => rows.sort_by(|a, b| {
            match sort.key {
                SortKey::Return30 => {
                    let (x, y) = (a.figures.get().return_30d, b.figures.get().return_30d);
                    match (x, y) {
                        // Agents with no 30-day figure go last, whichever way.
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (Some(x), Some(y)) => directed(x.total_cmp(&y)),
                        (None, None) => b.figures.get().days.cmp(&a.figures.get().days),
                    }
                }
                SortKey::InVault => directed(a.in_vault.get().total_cmp(&b.in_vault.get())),
            }
            .then_with(|| by_name(a, b))
        }),
        List::Closing => rows.sort_by(|a, b| {
            let (x, y) = (a.wind_down_ends.get(), b.wind_down_ends.get());
            match (x, y) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
            .then_with(|| by_name(a, b))
        }),
    }
    rows
}

/// The page's own state. Lives as long as the page.
struct Board {
    author: Option<String>,
    layout: Mutable<Layout>,
    sort: Mutable<Sort>,
    filters: Mutable<Filters>,
    search: Mutable<String>,
    rows: MutableVec<Rc<Agent>>,
    closing_rows: MutableVec<Rc<Agent>>,
    /// How many agents each list holds for this author, before filters.
    counts: Mutable<[usize; 2]>,
    /// How many live agents pass the filters and search, and how many of
    /// those are new.
    shown: Mutable<(usize, usize)>,
    popover: Mutable<Option<Popover>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Popover {
    Chain,
    Exit,
    Review,
}

impl Board {
    fn new(author: Option<String>) -> Rc<Self> {
        let board = Rc::new(Self {
            author,
            layout: Mutable::new(Layout::remembered()),
            sort: Mutable::new(Sort::default()),
            filters: Mutable::new(Filters::default()),
            search: Mutable::new(String::new()),
            rows: MutableVec::new(),
            closing_rows: MutableVec::new(),
            counts: Mutable::new([0; 2]),
            shown: Mutable::new((0, 0)),
            popover: Mutable::new(None),
        });
        board.recompute();
        board
    }

    fn recompute(&self) {
        let agents = store::get().all();
        let author = self.author.as_deref();
        let filters = self.filters.get_cloned();
        let search = self.search.get_cloned();
        let none = Filters::default();
        let counts = [List::Live, List::Closing]
            .map(|list| select(&agents, list, author, &none, "", Sort::default()).len());
        self.counts.set_neq(counts);
        let rows = select(
            &agents,
            List::Live,
            author,
            &filters,
            &search,
            self.sort.get(),
        );
        let new = rows
            .iter()
            .filter(|agent| agent.group.get() == Group::New)
            .count();
        self.shown.set_neq((rows.len(), new));
        replace(&self.rows, rows);
        let closing = select(
            &agents,
            List::Closing,
            author,
            &filters,
            &search,
            Sort::default(),
        );
        replace(&self.closing_rows, closing);
    }

    fn filtered(&self) -> bool {
        self.filters.lock_ref().count() > 0 || !self.search.lock_ref().trim().is_empty()
    }

    fn clear(&self) {
        self.filters.set(Filters::default());
        self.search.set(String::new());
    }
}

/// Replace a list only when its members or their order changed, so a row
/// that stays put is never redrawn.
fn replace(list: &MutableVec<Rc<Agent>>, rows: Vec<Rc<Agent>>) {
    let same = {
        let held = list.lock_ref();
        held.len() == rows.len() && held.iter().zip(&rows).all(|(a, b)| Rc::ptr_eq(a, b))
    };
    if !same {
        list.lock_mut().replace_cloned(rows);
    }
}

pub fn page(author: Option<String>, strip_dismissed: bool) -> Page {
    let author_key = author.as_deref().and_then(AuthorKey::parse);
    let board = Board::new(author_key.as_ref().map(AuthorKey::key));
    let store = store::get();

    // Recompute the rows when the reader asks (sort, filters, search) or the
    // set of agents changes; never because a figure moved.
    let watch = {
        let board = board.clone();
        map_ref! {
            let _sort = board.sort.signal(),
            let _filters = board.filters.signal_cloned(),
            let _search = board.search.signal_cloned(),
            let _members = store.membership.signal() => ()
        }
        .for_each(move |()| {
            board.recompute();
            async {}
        })
    };

    let head =
        el("div")
            .class("lb-head")
            .child(
                el("div")
                    .class("lb-head__text")
                    .child(el("h1").class("lb-title").text("Agents"))
                    .child(el("p").class("lb-lede").text(
                        "Every agent’s review, record and exit, read from its own contracts.",
                    )),
            )
            .child(how_to_read().class("lb-howto--top"));

    let body = el("div")
        .class("lb")
        .class_from("lb--grid", &board.layout, |layout| *layout == Layout::Grid)
        .child(head)
        .child(author_line(author.as_deref(), author_key.as_ref()))
        .child(controls(&board))
        .child(panel(&board))
        .child(closing_section(&board))
        .child(how_to_read().class("lb-howto--bottom"))
        .hook(move |dom| dom.future(watch));

    Page {
        top: Some(crate::components::strip::strip(strip_dismissed)),
        body: body.into(),
    }
}

fn how_to_read() -> El {
    el("a")
        .class("sh-link")
        .class("lb-howto")
        .attr("href", DOCS_DEPOSITORS)
        .text("How to read an agent page")
        .child(icon(Icon::External, 16))
}

/// "Agents by 0x07cc…f31a on Base · Show every agent", for an author link.
fn author_line(raw: Option<&str>, key: Option<&AuthorKey>) -> View {
    match (raw, key) {
        (None, _) => View::Empty,
        (Some(_), Some(key)) => el("p")
            .class("lb-author")
            .text("Agents by ")
            .child(hash_plain(&key.address.to_string()))
            .text(format!(" on {}", key.chain.label()))
            .child(
                app_link("/agents")
                    .class("lb-author__all")
                    .text("Show every agent"),
            )
            .into(),
        (Some(_), None) => notice(
            NoticeTone::Caution,
            View::Many(vec![
                text("That Author link isn’t one we can read. "),
                app_link("/agents").text("Show every agent").into(),
            ]),
        )
        .into(),
    }
}

fn controls(board: &Rc<Board>) -> El {
    el("div")
        .class("lb-controls")
        .child(
            el("div")
                .class("lb-tools")
                .child(search_field(board, "lb-search"))
                .child(filter_menus(board))
                .child(filters_button(board).class("lb-tools__filters"))
                .child(sort_button(board))
                .child(summary(board))
                .child(layout_picker(board)),
        )
        .child(active_filters(board))
}

/// A 16px glyph for the layout picker: rows, or a two-by-two grid.
fn layout_glyph(layout: Layout) -> El {
    let path = match layout {
        Layout::List => "M4 6h16M4 12h16M4 18h16",
        Layout::Grid => "M4 4h7v7H4zM13 4h7v7h-7zM4 13h7v7H4zM13 13h7v7h-7z",
    };
    svg("svg")
        .class("sh-icon")
        .attr("viewBox", "0 0 24 24")
        .attr("width", "16")
        .attr("height", "16")
        .attr("aria-hidden", "true")
        .attr("fill", "none")
        .attr("stroke", "currentColor")
        .attr("stroke-width", "2")
        .attr("stroke-linejoin", "round")
        .attr("stroke-linecap", "round")
        .child(svg("path").attr("d", path))
}

/// List or grid, from 600px up (a phone always shows cards): a segmented
/// control, buttons with `aria-pressed` in a labelled group.
fn layout_picker(board: &Rc<Board>) -> El {
    el("div")
        .class("sh-seg")
        .class("lb-layout")
        .attr("role", "group")
        .attr("aria-label", "Show agents as")
        .children(
            [(Layout::List, "List"), (Layout::Grid, "Grid")]
                .into_iter()
                .map(|(value, words)| {
                    let selected = board.layout.clone();
                    el("button")
                        .attr("type", "button")
                        .attr_from("aria-pressed", &board.layout, move |current| {
                            Some((*current == value).to_string())
                        })
                        .child(layout_glyph(value))
                        .text(words)
                        .hook(move |dom| {
                            dom.event(move |_: dominator::events::Click| {
                                selected.set_neq(value);
                                // Kept for the reader's next visit.
                                value.remember();
                            })
                        })
                }),
        )
}

fn search_field(board: &Rc<Board>, id: &'static str) -> El {
    let typed = board.search.clone();
    el("div")
        .class("lb-search")
        .child(el("label").class("sh-vh").attr("for", id).text("Search agents or Authors"))
        .child(
            el("div")
                .class("sh-input")
                .class("sh-input--s")
                .child(icon(Icon::Search, 18))
                .child(
                    el("input")
                        .attr("id", id)
                        .attr("type", "search")
                        .attr("placeholder", "Search agents or Authors")
                        .attr("autocomplete", "off")
                        .hook(move |dom| {
                            dominator::with_node!(dom, input => {
                                .prop_signal("value", typed.signal_cloned())
                                .event(move |_: dominator::events::Input| {
                                    if let Some(input) = wasm_bindgen::JsCast::dyn_ref::<web_sys::HtmlInputElement>(&input) {
                                        typed.set_neq(input.value());
                                    }
                                })
                            })
                        }),
                ),
        )
}

fn chain_options() -> Vec<Chain> {
    let mut chains: Vec<Chain> = store::get().all().iter().map(|agent| agent.chain).collect();
    chains.sort();
    chains.dedup();
    chains
}

/// One filter's choices, as a fieldset. `prefix` keeps ids unique between
/// the menus and the sheet.
fn fieldset(board: &Rc<Board>, which: Popover, prefix: &'static str) -> El {
    let legend = match which {
        Popover::Chain => "Vault chain",
        Popover::Exit => "Exit",
        Popover::Review => "Review",
    };
    let set = el("fieldset")
        .class("sh-fieldset")
        .child(el("legend").text(legend));
    match which {
        Popover::Chain => set.children(chain_options().into_iter().map(|chain| {
            let on = Mutable::new(board.filters.lock_ref().chains.contains(&chain));
            let filters = board.filters.clone();
            let follow = on.clone();
            checkbox(
                format!("{prefix}-chain-{}", chain.name()),
                chain.label(),
                &on,
            )
            .hook(move |dom| {
                dom.future(follow.signal().for_each(move |checked| {
                    let mut next = filters.get_cloned();
                    if checked {
                        next.chains.insert(chain);
                    } else {
                        next.chains.remove(&chain);
                    }
                    filters.set_neq(next);
                    async {}
                }))
            })
        })),
        Popover::Exit => {
            let choice = Mutable::new(board.filters.lock_ref().exit);
            let filters = board.filters.clone();
            set.children(
                [
                    (ExitFilter::Any, "Any exit"),
                    (ExitFilter::Direct, "Direct exit"),
                    (ExitFilter::Served, "Served exit"),
                ]
                .into_iter()
                .enumerate()
                .map(|(index, (value, label))| {
                    radio(
                        "lb-exit",
                        format!("{prefix}-exit-{index}"),
                        label,
                        value,
                        &choice,
                    )
                }),
            )
            .hook(move |dom| {
                dom.future(choice.signal().for_each(move |exit| {
                    let mut next = filters.get_cloned();
                    next.exit = exit;
                    filters.set_neq(next);
                    async {}
                }))
            })
        }
        Popover::Review => {
            let choice = Mutable::new(board.filters.lock_ref().review);
            let filters = board.filters.clone();
            set.children(
                [
                    (ReviewFilter::Any, "Any review"),
                    (ReviewFilter::NoSerious, "No serious concerns"),
                    (ReviewFilter::AllFine, "Looks fine in every area"),
                ]
                .into_iter()
                .enumerate()
                .map(|(index, (value, label))| {
                    radio(
                        "lb-review",
                        format!("{prefix}-review-{index}"),
                        label,
                        value,
                        &choice,
                    )
                }),
            )
            .hook(move |dom| {
                dom.future(choice.signal().for_each(move |review| {
                    let mut next = filters.get_cloned();
                    next.review = review;
                    filters.set_neq(next);
                    async {}
                }))
            })
        }
    }
}

/// The three filter menus, on a desktop.
fn filter_menus(board: &Rc<Board>) -> El {
    el("div")
        .class("lb-menus")
        .attr("role", "group")
        .attr("aria-label", "Filters")
        .children(
            [
                (Popover::Chain, "Vault chain"),
                (Popover::Exit, "Exit"),
                (Popover::Review, "Review"),
            ]
            .into_iter()
            .map(|(which, label)| {
                let toggle = board.popover.clone();
                let board_for_menu = board.clone();
                el("div")
                    .class("lb-menu")
                    .child(
                        Button::new(Kind::Secondary, label)
                            .size(Size::Small)
                            .icon_after(Icon::ChevronDown)
                            .build()
                            .attr("aria-haspopup", "true")
                            .attr_from("aria-expanded", &board.popover, move |open| {
                                Some((*open == Some(which)).to_string())
                            })
                            .hook(move |dom| {
                                dom.event(move |_: dominator::events::Click| {
                                    toggle.replace_with(|open| {
                                        if *open == Some(which) {
                                            None
                                        } else {
                                            Some(which)
                                        }
                                    });
                                })
                            }),
                    )
                    .child_from(&board.popover, move |open| {
                        (*open == Some(which)).then(|| {
                            let close = board_for_menu.popover.clone();
                            fieldset(&board_for_menu, which, "lb-menu")
                                .class("lb-pop")
                                .hook(move |dom| {
                                    dom.event(move |event: dominator::events::KeyDown| {
                                        if event.key() == "Escape" {
                                            close.set(None);
                                        }
                                    })
                                })
                                .into()
                        })
                    })
            }),
        )
        .hook({
            let popover = board.popover.clone();
            move |dom| {
                dom.global_event(move |event: dominator::events::Click| {
                    let inside = event
                        .dyn_target::<web_sys::Element>()
                        .and_then(|target| target.closest(".lb-menu").ok().flatten())
                        .is_some();
                    if !inside {
                        popover.set_neq(None);
                    }
                })
            }
        })
}

/// "Filters", which opens every filter in a dialog (a sheet on a phone).
fn filters_button(board: &Rc<Board>) -> El {
    let count = board.filters.clone();
    let open = board.clone();
    Button::new(
        Kind::Secondary,
        View::Many(vec![
            text("Filters"),
            el("span")
                .class("lb-count")
                .attr("aria-hidden", "true")
                .class_from("lb-count--off", &count, |filters| filters.count() == 0)
                .text_from(&count, |filters| filters.count().to_string())
                .into(),
            el("span")
                .class("sh-vh")
                .text_from(&count, |filters| match filters.count() {
                    0 => String::new(),
                    n => format!(", {n} on"),
                })
                .into(),
        ]),
    )
    .size(Size::Small)
    .build()
    .attr("aria-haspopup", "dialog")
    .hook(move |dom| {
        dom.event(move |_: dominator::events::Click| {
            let board = open.clone();
            let clear = open.clone();
            dialog::open(Dialog {
                title: "Filters".to_string(),
                body: View::Many(vec![
                    search_field(&board, "lb-search-sheet")
                        .class("lb-sheet-search")
                        .into(),
                    fieldset(&board, Popover::Chain, "lb-sheet").into(),
                    fieldset(&board, Popover::Exit, "lb-sheet").into(),
                    fieldset(&board, Popover::Review, "lb-sheet").into(),
                ]),
                foot: Some(View::Many(vec![
                    on_click(
                        Button::new(Kind::Secondary, "Clear filters").build(),
                        move || {
                            clear.clear();
                            dialog::close();
                        },
                    )
                    .into(),
                    on_click(
                        Button::new(Kind::Primary, "Show agents").build(),
                        dialog::close,
                    )
                    .into(),
                ])),
                wide: false,
            });
        })
    })
}

/// "Sort: 30 days", which opens the sort choices as a sheet on a phone.
fn sort_button(board: &Rc<Board>) -> El {
    let open = board.clone();
    Button::new(
        Kind::Secondary,
        el("span").text_from(&board.sort, |sort| format!("Sort: {}", sort.short())),
    )
    .size(Size::Small)
    .icon_before(Icon::Sort)
    .build()
    .class("lb-tools__sort")
    .attr("aria-haspopup", "dialog")
    .hook(move |dom| {
        dom.event(move |_: dominator::events::Click| {
            let choice = Mutable::new(open.sort.get());
            let sort = open.sort.clone();
            let follow = choice.clone();
            let options = [
                Sort {
                    key: SortKey::Return30,
                    descending: true,
                },
                Sort {
                    key: SortKey::Return30,
                    descending: false,
                },
                Sort {
                    key: SortKey::InVault,
                    descending: true,
                },
                Sort {
                    key: SortKey::InVault,
                    descending: false,
                },
            ];
            dialog::open(Dialog {
                title: "Sort agents".to_string(),
                body: el("fieldset")
                    .class("sh-fieldset")
                    .child(el("legend").class("sh-vh").text("Sort by"))
                    .children(options.into_iter().enumerate().map(|(index, option)| {
                        radio(
                            "lb-sort",
                            format!("lb-sort-{index}"),
                            option.words(),
                            option,
                            &choice,
                        )
                    }))
                    .hook(move |dom| {
                        dom.future(follow.signal().for_each(move |picked| {
                            sort.set_neq(picked);
                            async {}
                        }))
                    })
                    .into(),
                foot: Some(
                    on_click(Button::new(Kind::Primary, "Done").build(), dialog::close).into(),
                ),
                wide: false,
            });
        })
    })
}

/// "5 agents, sorted by 30 days, highest first", announced politely as it
/// changes.
fn summary(board: &Rc<Board>) -> El {
    let words = map_ref! {
        let counts = board.counts.signal(),
        let shown = board.shown.signal(),
        let sort = board.sort.signal(),
        let filters = board.filters.signal_cloned(),
        let search = board.search.signal_cloned() =>
        summary_words(counts[0], *shown, *sort, filters.count() > 0 || !search.trim().is_empty())
    };
    let now = summary_words(
        board.counts.get()[0],
        board.shown.get(),
        board.sort.get(),
        board.filtered(),
    );
    el("p")
        .class("lb-summary")
        .attr("aria-live", "polite")
        .text_signal(now, words)
}

fn agents(count: usize) -> String {
    if count == 1 {
        "1 agent".to_string()
    } else {
        format!("{count} agents")
    }
}

/// `shown` is how many live agents pass the filters, and how many of those
/// are new.
fn summary_words(total: usize, shown: (usize, usize), sort: Sort, filtered: bool) -> String {
    let (count, new) = shown;
    if filtered {
        return format!("Showing {count} of {}", agents(total));
    }
    match sort.key {
        SortKey::Return30 if count > 0 && new == count => {
            format!("{}, all under 30 days, so not ranked yet", agents(count))
        }
        SortKey::Return30 if new > 0 => format!(
            "{}, sorted by {}; new agents after",
            agents(count),
            sort.words()
        ),
        _ => format!("{}, sorted by {}", agents(count), sort.words()),
    }
}

/// The filters that are on, each removable, and "Clear filters".
fn active_filters(board: &Rc<Board>) -> El {
    let view_board = board.clone();
    el("div").class("lb-active").child_signal(
        pills(board, &board.filters.get_cloned()),
        board
            .filters
            .signal_cloned()
            .map(move |filters| pills(&view_board, &filters)),
    )
}

fn pills(board: &Rc<Board>, filters: &Filters) -> Option<View> {
    if filters.count() == 0 {
        return None;
    }
    let mut row = el("div").class("lb-pills");
    let mut pill = |label: String, change: Box<dyn Fn(&mut Filters)>| {
        let filters = board.filters.clone();
        row = std::mem::replace(&mut row, el("div")).child(
            el("button")
                .attr("type", "button")
                .class("lb-pill")
                .attr("aria-label", format!("Remove filter: {label}"))
                .text(label)
                .child(icon(Icon::Close, 16))
                .hook(move |dom| {
                    dom.event(move |_: dominator::events::Click| {
                        let mut next = filters.get_cloned();
                        change(&mut next);
                        filters.set_neq(next);
                    })
                }),
        );
    };
    if !filters.chains.is_empty() {
        let names: Vec<&str> = filters.chains.iter().map(|chain| chain.label()).collect();
        pill(
            format!("Vault chain: {}", names.join(", ")),
            Box::new(|filters| filters.chains.clear()),
        );
    }
    match filters.exit {
        ExitFilter::Any => {}
        ExitFilter::Direct => pill(
            "Exit: direct".into(),
            Box::new(|f| f.exit = ExitFilter::Any),
        ),
        ExitFilter::Served => pill(
            "Exit: served".into(),
            Box::new(|f| f.exit = ExitFilter::Any),
        ),
    }
    match filters.review {
        ReviewFilter::Any => {}
        ReviewFilter::NoSerious => pill(
            "Review: no serious concerns".into(),
            Box::new(|f| f.review = ReviewFilter::Any),
        ),
        ReviewFilter::AllFine => pill(
            "Review: looks fine".into(),
            Box::new(|f| f.review = ReviewFilter::Any),
        ),
    }
    let clear = board.clone();
    Some(
        row.child(
            el("button")
                .attr("type", "button")
                .class("lb-clear")
                .text("Clear filters")
                .hook(move |dom| dom.event(move |_: dominator::events::Click| clear.clear())),
        )
        .into(),
    )
}

/// The live list: the table (600px up, in the list layout) or the cards
/// (the grid layout, and a phone), and what to do when it's empty.
fn panel(board: &Rc<Board>) -> El {
    let store = store::get();
    let loaded = store.loaded.clone();
    let empty_board = board.clone();
    let empty_now = board.rows.lock_ref().is_empty() && store.loaded.get();
    el("section")
        .class("lb-list")
        .attr("aria-label", "Live agents")
        .child(
            el("div")
                .class("sh-card")
                .class("lb-panel")
                .attr("id", "lb-panel")
                .attr_from("aria-busy", &loaded, |loaded| (!loaded).then(|| "true".to_string()))
                .child(table(board, List::Live))
                .child(cards(board, List::Live))
                .child_signal(
                    empty(&empty_board, empty_now),
                    {
                        let board = board.clone();
                        map_ref! {
                            let empty = board.rows.signal_vec_cloned().len().map(|len| len == 0),
                            let loaded = loaded.signal(),
                            let _filters = board.filters.signal_cloned(),
                            let _search = board.search.signal_cloned() => *empty && *loaded
                        }
                        .map(move |empty_now| empty(&empty_board, empty_now))
                    },
                ),
        )
        .child(
            el("p")
                .class("lb-note")
                .text("Returns are after fees and read straight from each vault’s contract. An agent with under 30 days shows its return over the days it has, marked New, and isn’t ranked against 30-day figures. A review isn’t a prediction, and a track record isn’t a promise.")
                .child_from(&store.unread, |unread| {
                    (*unread > 0).then(|| {
                        text(format!(
                            " {} {} been read whole yet, so {} shown.",
                            agents(*unread).replace("agent", "vault"),
                            if *unread == 1 { "hasn’t" } else { "haven’t" },
                            if *unread == 1 { "it isn’t" } else { "they aren’t" },
                        ))
                    })
                }),
        )
}

fn empty(board: &Rc<Board>, is_empty: bool) -> Option<View> {
    if !is_empty {
        return None;
    }
    if board.filtered() {
        let search = board.search.get_cloned();
        let words = if search.trim().is_empty() {
            "No agents match these filters.".to_string()
        } else {
            format!(
                "No agents match ‘{}’. Try a different name or clear the filters.",
                search.trim()
            )
        };
        let clear = board.clone();
        return Some(
            el("div")
                .class("lb-empty")
                .child(el("p").text(words))
                .child(on_click(
                    Button::new(Kind::Secondary, "Clear filters")
                        .size(Size::Small)
                        .build(),
                    move || clear.clear(),
                ))
                .into(),
        );
    }
    let words = if board.author.is_some() {
        "This Author has no live agent right now."
    } else {
        "No agent is live right now."
    };
    Some(
        el("div")
            .class("lb-empty")
            .child(el("p").text(words))
            .into(),
    )
}

fn caption(list: List, sort: Sort) -> String {
    match list {
        List::Live => format!(
            "Live agents, sorted by {}",
            match (sort.key, sort.descending) {
                (SortKey::Return30, true) => "return over 30 days, highest first, new agents after",
                (SortKey::Return30, false) => "return over 30 days, lowest first, new agents after",
                (SortKey::InVault, true) => "money in vault, largest first",
                (SortKey::InVault, false) => "money in vault, smallest first",
            }
        ),
        List::Closing => "Agents winding down and closed".to_string(),
    }
}

/// A header cell: plain, or a sort button with `aria-sort` when sorted.
fn header_cell(
    board: &Rc<Board>,
    label: &'static str,
    key: Option<SortKey>,
    class: &'static str,
) -> El {
    let th = el("th").attr("scope", "col").class(class);
    let Some(key) = key else {
        return th.text(label);
    };
    let sorted = move |sort: &Sort| {
        (sort.key == key).then(|| {
            if sort.descending {
                "descending"
            } else {
                "ascending"
            }
            .to_string()
        })
    };
    let click = board.sort.clone();
    th.attr_from("aria-sort", &board.sort, sorted).child(
        el("button")
            .attr("type", "button")
            .class("sh-sort")
            .text(label)
            .child_from(&board.sort, move |sort| {
                Some(
                    icon(
                        if sort.key == key {
                            Icon::ChevronDown
                        } else {
                            Icon::Sort
                        },
                        16,
                    )
                    .class_if(sort.key == key && !sort.descending, "lb-sort--up")
                    .into(),
                )
            })
            .hook(move |dom| {
                dom.event(move |_: dominator::events::Click| {
                    click.replace_with(|sort| Sort {
                        key,
                        descending: if sort.key == key {
                            !sort.descending
                        } else {
                            true
                        },
                    });
                })
            }),
    )
}

fn table(board: &Rc<Board>, list: List) -> El {
    let store = store::get();
    let sortable = |key: SortKey| (list == List::Live).then_some(key);
    let caption_now = caption(list, board.sort.get());
    let words = board.sort.signal().map(move |sort| caption(list, sort));
    let rows = match list {
        List::Live => &board.rows,
        List::Closing => &board.closing_rows,
    };
    el("div").class("lb-table-wrap").child(
        el("table")
            .class("sh-table")
            .class("lb-table")
            .child(el("caption").class("sh-vh").text_signal(caption_now, words))
            .child(
                el("thead").child(
                    el("tr")
                        .child(header_cell(board, "Agent", None, "lb-c-agent"))
                        .child(header_cell(board, "Review", None, "lb-c-review"))
                        .child(header_cell(
                            board,
                            "30 days",
                            sortable(SortKey::Return30),
                            "lb-c-r30 sh-r",
                        ))
                        .child(header_cell(
                            board,
                            "Since opening",
                            None,
                            "lb-c-since sh-r lb-wide",
                        ))
                        .child(header_cell(
                            board,
                            "Deepest drop",
                            None,
                            "lb-c-drop sh-r lb-wide",
                        ))
                        .child(header_cell(
                            board,
                            "In vault",
                            sortable(SortKey::InVault),
                            "lb-c-vault sh-r",
                        ))
                        .child(header_cell(board, "Tank", None, "lb-c-tank lb-wide"))
                        .child(
                            el("th")
                                .attr("scope", "col")
                                .class("lb-c-open")
                                .child(el("span").class("sh-vh").text("Open")),
                        ),
                ),
            )
            .child(
                el("tbody")
                    .children_from(rows, row)
                    .child_from(&store.loaded, move |loaded| {
                        (!loaded && list == List::Live)
                            .then(|| View::Many((0..4).map(|_| skeleton_row()).collect()))
                    }),
            ),
    )
}

fn skeleton_row() -> View {
    let cell = |width: &'static str| el("td").class("sh-r").child(skeleton(width, 16));
    el("tr")
        .class("lb-skel")
        .child(
            el("td").child(
                el("span")
                    .class("lb-name")
                    .child(skeleton("96", 28).class("lb-spark"))
                    .child(
                        el("span")
                            .class("lb-name__text")
                            .child(skeleton("140", 16))
                            .child(skeleton("100", 12)),
                    ),
            ),
        )
        .child(el("td").child(skeleton("128", 24)))
        .child(cell("48"))
        .child(cell("56").class("lb-wide"))
        .child(cell("48").class("lb-wide"))
        .child(cell("88"))
        .child(el("td").class("lb-wide").child(skeleton("48", 16)))
        .child(el("td"))
        .into()
}

/// What sits where the sparkline would, for an agent that is winding down.
fn closing_chip(agent: &Agent) -> El {
    el("span")
        .class("lb-state")
        .child_from(&agent.state, |state| {
            use shared::VaultState::*;
            Some(
                match state {
                    WindingDown => badge(Tone::Caution, Some(Icon::Clock), "Retiring"),
                    Frozen => badge(Tone::Loss, Some(Icon::Pause), "Frozen"),
                    Killed => badge(Tone::Loss, Some(Icon::Pause), "Killed"),
                    Closed => badge(Tone::Neutral, None, "Closed"),
                    _ => badge(Tone::Neutral, None, "Stopped"),
                }
                .into(),
            )
        })
}

/// "4 days left · no new deposits" under a retiring agent's name.
fn closing_line(agent: &Agent) -> El {
    let as_of = store::get().as_of.clone();
    let ends = agent.wind_down_ends.clone();
    el("span").class("lb-closing").child_signal(
        closing_words(ends.get(), as_of.get()).map(text),
        map_ref! {
            let ends = ends.signal(),
            let as_of = as_of.signal() => closing_words(*ends, *as_of).map(text)
        },
    )
}

fn closing_words(
    ends: Option<time::PrimitiveDateTime>,
    now: Option<time::PrimitiveDateTime>,
) -> Option<String> {
    let (ends, now) = (ends?, now?);
    let left = days_until(ends, now);
    Some(match left {
        0 => "Last day · no new deposits".to_string(),
        1 => "1 day left · no new deposits".to_string(),
        left => format!("{left} days left · no new deposits"),
    })
}

/// "As of 14:02 UTC" when the indexer couldn't read this vault whole.
fn stale_line(agent: &Agent) -> El {
    el("span")
        .class("lb-stale")
        .child_from(&agent.stale_as_of, |at| {
            at.map(|at| {
                View::Many(vec![
                    icon(Icon::Clock, 14).into(),
                    text(format!("Figures as of {}", format::day_time(at, at.year()))),
                ])
            })
        })
}

fn tank_words(agent: &Agent) -> El {
    el("span")
        .class("lb-tank__words")
        .class_from("lb-tank__words--low", &agent.tank, |tank| {
            tank.is_low() || tank.is_empty()
        })
        .child_from(&agent.tank, |tank| {
            Some(if tank.is_low() || tank.is_empty() {
                View::Many(vec![icon(Icon::Concern, 14).into(), text(tank.short())])
            } else {
                text(tank.short())
            })
        })
}

fn days_words(agent: &Agent) -> El {
    el("span")
        .class("lb-sub")
        .text_from(&agent.figures, |figures| format::days(figures.days))
}

fn since_opening(agent: &Agent) -> El {
    let since = Mutable::new(agent.figures.get().since_opening);
    let figures = agent.figures.clone();
    let follow = since.clone();
    delta_from(&since, "No record yet").hook(move |dom| {
        dom.future(figures.signal().for_each(move |figures| {
            follow.set_neq(figures.since_opening);
            async {}
        }))
    })
}

fn return_30(agent: &Agent, missing: &'static str) -> El {
    let value = Mutable::new(agent.figures.get().return_30d);
    let figures = agent.figures.clone();
    let follow = value.clone();
    delta_from(&value, missing).hook(move |dom| {
        dom.future(figures.signal().for_each(move |figures| {
            follow.set_neq(figures.return_30d);
            async {}
        }))
    })
}

/// The deepest drop: never shown as a positive number, always in loss colour
/// once there is one.
fn deepest(agent: &Agent) -> El {
    el("span")
        .class("lb-drop")
        .class_from("sh-loss", &agent.figures, |figures| {
            figures
                .deepest_drop
                .is_some_and(|drop| format::ret(drop) != "0.0%")
        })
        .child_from(&agent.figures, |figures| {
            Some(match figures.deepest_drop {
                Some(drop) => text(format::ret(drop.min(0.0))),
                None => missing_figure("No record yet").into(),
            })
        })
}

fn in_vault(agent: &Agent) -> El {
    el("span")
        .class("lb-money")
        .text_from(&agent.in_vault, |value| format::money_whole(*value))
}

/// The agent's name and facts, as one link to its page.
fn name_link(agent: &Rc<Agent>, group_closing: bool) -> El {
    let link = app_link(agent.href()).class("lb-name");
    let link = if group_closing {
        link.child(closing_chip(agent))
    } else {
        link.child(sparkline(&agent.spark).class("lb-spark"))
    };
    link.child(
        el("span")
            .class("lb-name__text")
            .child(
                el("span")
                    .class("lb-name__title")
                    .text_from(&agent.title, Clone::clone),
            )
            .child(
                el("span")
                    .class("lb-meta")
                    .child(new_chip(agent))
                    .text_from(&agent.meta, Clone::clone)
                    .child(
                        el("span")
                            .class("lb-narrow")
                            .text(" · Tank ")
                            .child(tank_words(agent)),
                    ),
            )
            .child(
                el("span")
                    .class("lb-narrow")
                    .class("lb-meta")
                    .child(since_opening(agent))
                    .text(" since opening · ")
                    .child(deepest(agent))
                    .text(" deepest drop"),
            )
            .child(stale_line(agent)),
    )
}

/// "New", beside a live agent with under 30 days of history.
fn new_chip(agent: &Agent) -> El {
    el("span")
        .class("lb-newchip")
        .child_from(&agent.group, |group| {
            (*group == Group::New).then(|| badge(Tone::Brand, None, "New").into())
        })
}

/// The 30-day cell: the 30-day return. A new agent has no 30-day figure, so
/// it says plainly how far it has got ("5 of 30 days"); where the table has
/// no Since opening column (under 1024px), it also gives the return over
/// the days it has, with the period named.
fn period_figure(agent: &Agent) -> El {
    if agent.group.get() == Group::New {
        el("span")
            .class("lb-stack")
            .child(
                el("span")
                    .class("lb-wide")
                    .class("lb-period")
                    .text_from(&agent.figures, |figures| {
                        format!("{} of 30 days", figures.days)
                    }),
            )
            .child(
                el("span")
                    .class("lb-narrow")
                    .class("lb-stack")
                    .child(since_opening(agent).class("lb-fig"))
                    .child(
                        el("span")
                            .class("lb-sub")
                            .text_from(&agent.figures, |figures| {
                                format!("over {}", format::days(figures.days))
                            }),
                    ),
            )
    } else {
        return_30(agent, "Needs 30 days").class("lb-fig")
    }
}

fn row(agent: &Rc<Agent>) -> View {
    let closing = agent.group.get() == Group::Closing;
    let name = name_link(agent, closing);
    let name = if closing {
        name.child(closing_line(agent))
    } else {
        name
    };
    el("tr")
        .class("sh-row")
        .class("lb-row")
        .child(
            el("th")
                .attr("scope", "row")
                .class("lb-c-agent")
                .child(name),
        )
        .child(el("td").child_from(&agent.review, |review| Some(review_chip(review).into())))
        .child(el("td").class("sh-r").child(period_figure(agent)))
        .child(
            el("td").class("sh-r").class("lb-wide").child(
                el("span")
                    .class("lb-stack")
                    .child(since_opening(agent).class("lb-fig"))
                    .child(days_words(agent)),
            ),
        )
        .child(
            el("td")
                .class("sh-r")
                .class("lb-wide")
                .class("lb-fig")
                .child(deepest(agent)),
        )
        .child(
            el("td")
                .class("sh-r")
                .class("lb-fig")
                .child(in_vault(agent)),
        )
        .child(
            el("td").class("lb-wide").child(
                el("span")
                    .class("lb-tank")
                    .child(gauge(&agent.tank, true))
                    .child(tank_words(agent)),
            ),
        )
        .child(
            el("td").class("sh-r").class("lb-c-open").child(
                el("span")
                    .class("lb-view")
                    .attr("aria-hidden", "true")
                    .text("View")
                    .child(icon(Icon::ChevronRight, 16)),
            ),
        )
        .into()
}

/// The cards: every list on a phone, and the grid layout from 600px up.
fn cards(board: &Rc<Board>, list: List) -> El {
    let store = store::get();
    let rows = match list {
        List::Live => &board.rows,
        List::Closing => &board.closing_rows,
    };
    el("ul")
        .class("lb-cards")
        .attr("aria-label", caption(list, Sort::default()))
        .children_from(rows, card)
        .child_from(&store.loaded, move |loaded| {
            (!loaded && list == List::Live).then(|| {
                View::Many(
                    (0..4)
                        .map(|_| {
                            el("li")
                                .child(
                                    el("div")
                                        .class("sh-card")
                                        .class("lb-card")
                                        .child(skeleton("60%", 20))
                                        .child(skeleton("128", 24))
                                        .child(skeleton("100%", 40)),
                                )
                                .into()
                        })
                        .collect(),
                )
            })
        })
}

fn card_stat(label: &'static str, figure: El, period: Option<El>) -> El {
    let stat = el("span")
        .class("lb-cstat")
        .child(el("span").class("sh-label").text(label))
        .child(figure.class("lb-cstat__value"));
    match period {
        Some(period) => stat.child(period.class("lb-cstat__period")),
        None => stat,
    }
}

/// One agent as a card. A new agent's card carries its New chip and leads
/// with its return since opening, over the days it has.
fn card(agent: &Rc<Agent>) -> View {
    let group = agent.group.get();
    let is_new = group == Group::New;
    let top_right: El = if is_new {
        el("span")
            .class("sh-chip")
            .class("sh-chip-brand")
            .text_from(&agent.figures, |figures| {
                format!("New · {}", format::days(figures.days))
            })
    } else if group == Group::Closing {
        closing_chip(agent)
    } else {
        sparkline(&agent.spark).class("lb-spark")
    };
    let head = el("span")
        .class("lb-card__head")
        .child(
            el("span").class("lb-card__name").child(
                el("span")
                    .class("lb-name__text")
                    .child(
                        el("span")
                            .class("lb-name__title")
                            .text_from(&agent.title, Clone::clone),
                    )
                    .child(
                        el("span")
                            .class("lb-meta")
                            .text_from(&agent.meta, Clone::clone),
                    ),
            ),
        )
        .child(top_right);
    let stats = if is_new {
        el("span")
            .class("lb-card__stats")
            .child(card_stat(
                "Since opening",
                since_opening(agent),
                Some(days_words(agent)),
            ))
            .child(card_stat(
                "Deepest drop",
                deepest(agent),
                Some(el("span").text("Since opening").class("lb-wide-only")),
            ))
            .child(card_stat(
                "In vault",
                in_vault(agent),
                Some(el("span").class("lb-wide-only").text(format!(
                    "{} ({})",
                    agent.chain.label(),
                    agent.asset.get_cloned()
                ))),
            ))
    } else {
        el("span")
            .class("lb-card__stats")
            .child(card_stat(
                "30 days",
                return_30(agent, "Needs 30 days"),
                None,
            ))
            .child(card_stat(
                "Since opening",
                since_opening(agent),
                Some(days_words(agent)),
            ))
            .child(card_stat("In vault", in_vault(agent), None))
    };
    let tank_low = el("span")
        .class("lb-card__tank")
        .child_from(&agent.tank, |tank| {
            (tank.is_low() || tank.is_empty()).then(|| {
                View::Many(vec![
                    icon(Icon::Concern, 16).into(),
                    text(if tank.is_empty() {
                        "Stopped: tank empty".to_string()
                    } else {
                        format!(
                            "Tank low: {} of running left",
                            format::runway(tank.days).to_lowercase()
                        )
                    }),
                ])
            })
        });
    let link = app_link(agent.href())
        .class("sh-card")
        .class("sh-card-link")
        .class("lb-card")
        .child(head)
        .child(
            el("span")
                .class("sh-chips")
                .child_from(&agent.review, |review| Some(review_chip(review).into())),
        )
        .child(tank_low)
        .child(stale_line(agent));
    let link = if group == Group::Closing {
        link.child(closing_line(agent))
    } else {
        link
    };
    el("li").child(link.child(stats)).into()
}

/// Winding down and closed: under the live list, quieter, with what
/// leaving one means. Shown only when there is one to show.
fn closing_section(board: &Rc<Board>) -> El {
    let shown = board
        .closing_rows
        .signal_vec_cloned()
        .len()
        .map(|count| count > 0);
    let count_now = board.closing_rows.lock_ref().len();
    let now = count_now > 0;
    el("section")
        .class("lb-closed")
        .attr("aria-labelledby", "lb-closed-title")
        .class_signal("lb-closed--shown", now, shown)
        .child(
            el("div")
                .class("lb-closed__head")
                .child(
                    el("h2")
                        .attr("id", "lb-closed-title")
                        .class("lb-closed__title")
                        .text("Winding down and closed")
                        .child(
                            el("span")
                                .class("sh-chip")
                                .class("sh-chip-neutral")
                                .text_signal(
                                    count_now.to_string(),
                                    board
                                        .closing_rows
                                        .signal_vec_cloned()
                                        .len()
                                        .map(|count| count.to_string()),
                                ),
                        ),
                )
                .child(el("p").class("lb-closed__lede").text(
                    "They take no new deposits. If you’re in a retiring one, you can leave any time before its last day; after that its positions close at market, which can lock in a loss. A closed one pays out from its cash, with no deadline.",
                )),
        )
        .child(
            el("div")
                .class("sh-card")
                .class("lb-panel")
                .class("lb-panel--quiet")
                .child(table(board, List::Closing))
                .child(cards(board, List::Closing)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agents_html() -> String {
        let snapshot: shared::Snapshot =
            serde_json::from_str(include_str!("../../tests/snapshot.json"))
                .expect("the captured snapshot decodes");
        store::get().apply(&snapshot);
        page(None, false).body.html()
    }

    #[test]
    fn the_leaderboard_is_one_list_with_closing_agents_in_their_own_section() {
        let html = agents_html();
        assert!(!html.contains("role=\"tablist\""), "no tabs");
        assert!(!html.contains("Open for deposits"));
        let live = html
            .find("aria-label=\"Live agents\"")
            .expect("the live list");
        let closed = html
            .find("Winding down and closed")
            .expect("the closing section");
        assert!(live < closed);
        assert!(
            html.contains("lb-closed--shown"),
            "the capture has closed agents"
        );
        // Sorting still works: the sorted column says so.
        assert_eq!(html.matches("aria-sort=").count(), 1);
    }

    #[test]
    fn the_layout_picker_is_a_labelled_group_of_pressed_buttons() {
        let html = agents_html();
        let at = html
            .find("aria-label=\"Show agents as\"")
            .expect("the picker");
        let picker = &html[at..at + html[at..].find("</div>").unwrap()];
        assert_eq!(picker.matches("aria-pressed=\"true\"").count(), 1);
        assert_eq!(picker.matches("aria-pressed=\"false\"").count(), 1);
        assert!(picker.contains(">List<") || picker.contains("List"));
        assert!(picker.contains("Grid"));
    }

    #[test]
    fn the_summary_names_the_sort_and_says_when_new_agents_follow() {
        assert_eq!(
            summary_words(5, (5, 0), Sort::default(), false),
            "5 agents, sorted by 30 days, highest first"
        );
        assert_eq!(
            summary_words(5, (5, 2), Sort::default(), false),
            "5 agents, sorted by 30 days, highest first; new agents after"
        );
        assert_eq!(
            summary_words(3, (3, 3), Sort::default(), false),
            "3 agents, all under 30 days, so not ranked yet"
        );
        let by_vault = Sort {
            key: SortKey::InVault,
            descending: true,
        };
        assert_eq!(
            summary_words(5, (5, 2), by_vault, false),
            "5 agents, sorted by In vault, largest first"
        );
        assert_eq!(
            summary_words(5, (4, 1), Sort::default(), true),
            "Showing 4 of 5 agents"
        );
    }

    #[test]
    fn a_retiring_agent_counts_down_in_whole_days() {
        let now = time::macros::datetime!(2026-09-29 12:00);
        assert_eq!(
            closing_words(Some(time::macros::datetime!(2026-10-02 18:00)), Some(now)).as_deref(),
            Some("4 days left · no new deposits")
        );
        assert_eq!(
            closing_words(Some(now), Some(now)).as_deref(),
            Some("Last day · no new deposits")
        );
    }
}
