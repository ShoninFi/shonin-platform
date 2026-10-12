//! `/_atoms`, dev builds only: every token, type style, icon and atom on one
//! page, to check them against the Foundations and Components boards.

use futures_signals::signal::Mutable;
use shared::Rating;
use ui::icons::Icon;

use crate::atoms::button::{Button, Kind, Size, icon_button, on_click};
use crate::atoms::check::{checkbox, radio};
use crate::atoms::delta::{delta, delta_arrow, delta_from};
use crate::atoms::dialog::{self, Dialog};
use crate::atoms::gauge::gauge;
use crate::atoms::hash::hash;
use crate::atoms::notice::{NoticeTone, lead, notice, notice_with};
use crate::atoms::segmented::segmented;
use crate::atoms::skeleton::{skeleton, skeleton_block};
use crate::atoms::sparkline::sparkline;
use crate::atoms::stat::{stat, stat_tile};
use crate::atoms::switch::switch;
use crate::atoms::toast::{self, ToastKind};
use crate::atoms::tooltip::tooltip;
use crate::atoms::{Tone, badge, icon, rating, review_chip};
use crate::format;
use crate::model::{ReviewSummary, TankState};
use crate::view::{El, View, el, text};

use super::Page;

fn section(id: &'static str, title: &'static str, body: impl IntoIterator<Item = El>) -> El {
    el("section")
        .class("at-section")
        .attr("aria-labelledby", id)
        .child(el("h2").attr("id", id).class("at-h2").text(title))
        .children(body)
}

fn row(label: &'static str, items: impl IntoIterator<Item = El>) -> El {
    el("div")
        .class("at-row")
        .child(el("p").class("at-row__label").text(label))
        .child(el("div").class("at-row__items").children(items))
}

/// Set a CSS property through the CSS object model once the element is in
/// the page (an inline `style` attribute would be refused by the policy).
fn style(element: El, property: &'static str, value: String) -> El {
    element.hook(move |dom| {
        dom.after_inserted(move |element: web_sys::Element| {
            if let Some(element) = wasm_bindgen::JsCast::dyn_ref::<web_sys::HtmlElement>(&element) {
                let _ = element.style().set_property(property, &value);
            }
        })
    })
}

fn tokens() -> El {
    let tokens = ui::Tokens::parse(ui::TOKENS).expect("tokens.json parses");
    let swatches = tokens.color.tokens.iter().map(|token| {
        el("div")
            .class("at-swatch")
            .child(style(
                el("span").class("at-swatch__chip"),
                "background",
                format!("var(--{})", token.name),
            ))
            .child(
                el("span")
                    .class("at-swatch__name")
                    .text(format!("--{}", token.name)),
            )
            .child(
                el("span")
                    .class("at-swatch__value")
                    .text(ui::alias(&token.value)),
            )
    });
    let spaces = tokens.spacing.tokens.iter().map(|token| {
        el("div")
            .class("at-space")
            .child(style(
                el("span").class("at-space__bar"),
                "width",
                format!("var(--{})", token.name),
            ))
            .child(
                el("span")
                    .class("at-swatch__name")
                    .text(format!("--{} · {}", token.name, token.value)),
            )
    });
    let radii = tokens.radius.tokens.iter().map(|token| {
        el("div")
            .class("at-radius")
            .child(style(
                el("span").class("at-radius__box"),
                "border-radius",
                format!("var(--{})", token.name),
            ))
            .child(
                el("span")
                    .class("at-swatch__name")
                    .text(format!("--{} · {}", token.name, token.value)),
            )
    });
    let motion = tokens
        .motion
        .iter()
        .flat_map(|group| group.tokens.iter())
        .map(|token| {
            el("p")
                .class("at-mono")
                .text(format!("--{}: {}", token.name, token.value))
        });
    section(
        "at-tokens",
        "Tokens",
        [
            row(
                "Colour",
                [el("div").class("at-swatches").children(swatches)],
            ),
            row("Spacing", spaces.collect::<Vec<_>>()),
            row("Radius", radii.collect::<Vec<_>>()),
            row(
                "Shadow",
                [el("div")
                    .class("at-float")
                    .text("--shadow-float: only for what floats")],
            ),
            row("Motion", motion.collect::<Vec<_>>()),
        ],
    )
}

fn type_styles() -> El {
    let tokens = ui::Tokens::parse(ui::TOKENS).expect("tokens.json parses");
    let samples = tokens.typography.groups.iter().flat_map(|group| {
        group.styles.iter().map(|style| {
            el("div")
                .class("at-type")
                .child(el("p").class("at-type__name").text(format!(
                    ".t-{} · {}/{}",
                    style.name, style.font_size, style.line_height
                )))
                .child(el("p").class(format!("t-{}", style.name)).text(
                    if style.name.starts_with("figure") {
                        "$1.1402 +14.0%".to_string()
                    } else {
                        "Trading agents you can check.".to_string()
                    },
                ))
        })
    });
    section("at-type", "Type", samples.collect::<Vec<_>>())
}

fn icons() -> El {
    section(
        "at-icons",
        "Icons",
        [el("div")
            .class("at-icons")
            .children(Icon::ALL.iter().map(|glyph| {
                el("div")
                    .class("at-icon")
                    .child(icon(*glyph, 24))
                    .child(el("span").class("at-icon__name").text(glyph.name()))
                    .child(el("span").class("at-icon__meaning").text(glyph.meaning()))
            }))],
    )
}

fn buttons() -> El {
    let busy = Mutable::new(true);
    let kinds = [
        (Kind::Primary, "Deposit"),
        (Kind::Secondary, "Withdraw"),
        (Kind::Quiet, "Cancel"),
        (Kind::Danger, "Retire agent"),
    ];
    let mut rows: Vec<El> = kinds
        .iter()
        .map(|(kind, label)| {
            row(
                label,
                [
                    Button::new(*kind, *label).size(Size::Small).build(),
                    Button::new(*kind, *label).build(),
                    Button::new(*kind, *label).size(Size::Large).build(),
                    Button::new(*kind, *label).disabled(true).build(),
                    Button::new(*kind, *label).busy(&busy).build(),
                ],
            )
        })
        .collect();
    rows.push(row(
        "Hero",
        [el("div")
            .class("sh-hero")
            .class("at-hero")
            .child(
                Button::new(Kind::Hero, "Open the app")
                    .icon_after(Icon::ArrowRight)
                    .build(),
            )
            .child(Button::new(Kind::Hero2, "Publish an agent").build())],
    ));
    rows.push(row(
        "Icon and link",
        [
            Button::new(Kind::Secondary, "See all agents")
                .icon_after(Icon::ArrowRight)
                .build(),
            icon_button(Icon::Close, "Close dialog"),
            icon_button(Icon::Menu, "Open menu"),
            el("a")
                .class("sh-link")
                .attr("href", "#")
                .text("Read the guide")
                .child(icon(Icon::ArrowRight, 18)),
        ],
    ));
    section("at-buttons", "Buttons", rows)
}

fn chips() -> El {
    section(
        "at-chips",
        "Badge, rating, delta",
        [
            row(
                "Ratings",
                [
                    Rating::LooksFine,
                    Rating::SomeConcerns,
                    Rating::SeriousConcerns,
                    Rating::CouldntTell,
                ]
                .map(rating),
            ),
            row(
                "Review summary",
                [
                    review_chip(&ReviewSummary::AllFine),
                    review_chip(&ReviewSummary::Worst {
                        rating: Rating::SomeConcerns,
                        areas: 1,
                    }),
                    review_chip(&ReviewSummary::Worst {
                        rating: Rating::SeriousConcerns,
                        areas: 2,
                    }),
                    review_chip(&ReviewSummary::Worst {
                        rating: Rating::CouldntTell,
                        areas: 1,
                    }),
                ],
            ),
            row(
                "State",
                [
                    badge(Tone::Neutral, None, "Activating"),
                    badge(Tone::Brand, None, "New · 12 days"),
                    badge(Tone::Caution, Some(Icon::Clock), "Retiring · 4 days left"),
                    badge(Tone::Loss, Some(Icon::Pause), "Frozen"),
                    badge(Tone::Loss, Some(Icon::Pause), "Killed"),
                    badge(Tone::Caution, Some(Icon::Concern), "Tank low · 5 days"),
                    badge(Tone::Loss, Some(Icon::Pause), "Stopped · tank empty"),
                ],
            ),
            row(
                "Delta",
                [
                    delta(0.032),
                    delta(-0.062),
                    delta(0.0),
                    delta_arrow(0.14),
                    delta_arrow(-0.053),
                    delta_from(&Mutable::new(None), "Needs 30 days"),
                ],
            ),
        ],
    )
}

fn figures() -> El {
    let spark = Mutable::new(vec![
        1.0, 1.01, 1.005, 1.02, 1.03, 1.025, 1.04, 1.06, 1.05, 1.07,
    ]);
    let flat = Mutable::new(vec![1.0]);
    let healthy = Mutable::new(TankState {
        balance: 1212.4,
        days: 41.0,
    });
    let low = Mutable::new(TankState {
        balance: 147.5,
        days: 5.2,
    });
    let empty = Mutable::new(TankState {
        balance: 0.0,
        days: 0.0,
    });
    section(
        "at-figures",
        "Stat, sparkline, tank, hash",
        [
            row(
                "Stat",
                [
                    stat_tile("Return", delta(0.032), "30 days"),
                    stat_tile("Since opening", delta(0.14), "91 days"),
                    stat_tile(
                        "In vault",
                        text(format::money_whole(2_418_330.0)),
                        "USDC on Hyperliquid",
                    ),
                    stat_tile("30 days", text(format::MISSING), "Needs 30 days"),
                    stat(
                        "Share price",
                        text(format::price(1.1402)),
                        "As of 14:29 UTC",
                    ),
                ],
            ),
            row("Sparkline", [sparkline(&spark), sparkline(&flat)]),
            row(
                "Tank gauge",
                [healthy, low, empty]
                    .iter()
                    .map(|tank| {
                        el("div")
                            .class("at-gauge")
                            .child(gauge(tank, false))
                            .child(el("p").class("sh-muted").text(tank.get().sentence()))
                    })
                    .collect::<Vec<_>>(),
            ),
            row(
                "Hash",
                [
                    hash(
                        "0x4e1d30877432d1026706d7e805da846a32c3bb81e3c29b62179273c8eb5ba90b",
                        Some("build"),
                        "build hash",
                    ),
                    hash(
                        "0x2d19b682575ec87a171ac826a6fce48478dcb4e8",
                        None,
                        "address",
                    ),
                ],
            ),
            row(
                "Skeleton",
                [
                    skeleton("140", 16),
                    skeleton("96", 28),
                    skeleton_block("240", 48),
                ],
            ),
        ],
    )
}

fn notices() -> El {
    section(
        "at-notices",
        "Notice",
        [
            notice(
                NoticeTone::Info,
                "Your deposit is priced at the next valuation time, a few seconds after you ask, at prices nobody could see when you asked.",
            ),
            notice(
                NoticeTone::Caution,
                lead(
                    "The tank is running low: about 5 days left.",
                    "The tank pays to run the agent, never from your money. If it empties, the agent stops and its positions sit unmanaged until someone tops it up.",
                ),
            ),
            notice(
                NoticeTone::Danger,
                lead(
                    "This vault is frozen.",
                    "A booked withdrawal went unpaid for 7 days, so it only winds down now. Deposits are closed. You can still withdraw.",
                ),
            ),
            notice_with(
                NoticeTone::Caution,
                Icon::Clock,
                "Retiring agents take no new deposits.",
            ),
        ],
    )
}

fn controls() -> El {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Range {
        Week,
        Month,
        Quarter,
        All,
    }
    let range = Mutable::new(Range::Quarter);
    let auto_fill = Mutable::new(true);
    let off = Mutable::new(false);
    let ticked = Mutable::new(false);
    let picked = Mutable::new(1u8);
    section(
        "at-controls",
        "Segmented, switch, checkbox, tooltip",
        [
            row(
                "Segmented",
                [segmented(
                    "Chart range",
                    &[
                        (Range::Week, "1W"),
                        (Range::Month, "1M"),
                        (Range::Quarter, "3M"),
                        (Range::All, "All"),
                    ],
                    &range,
                )],
            ),
            row(
                "Switch",
                [
                    switch(
                        "Auto-fill",
                        Some("Keep the tank at $1,500."),
                        &auto_fill,
                        false,
                    ),
                    switch("Auto-fill", Some("Off for sealed test vaults."), &off, true),
                ],
            ),
            row(
                "Checkbox and radio",
                [
                    checkbox(
                        "at-ack".into(),
                        "I understand the agent can lose money, and that a review isn’t a prediction.",
                        &ticked,
                    ),
                    radio("at-radio", "at-radio-1".into(), "Everything", 1u8, &picked),
                    radio("at-radio", "at-radio-2".into(), "Part of it", 2u8, &picked),
                ],
            ),
            row(
                "Tooltip",
                [tooltip(
                    Button::new(Kind::Secondary, "Hover or focus me").build(),
                    "Priced a few seconds after you ask.",
                    "at-tip",
                )],
            ),
        ],
    )
}

fn overlays() -> El {
    section(
        "at-overlays",
        "Toast and dialog",
        [
            row(
                "Toast",
                [
                    on_click(Button::new(Kind::Secondary, "Success").build(), || {
                        toast::push(
                            ToastKind::Success,
                            "You now hold 876.2 more shares",
                            "Basis Harvester · priced at $1.1402 a share.",
                        )
                    }),
                    on_click(Button::new(Kind::Secondary, "Info").build(), || {
                        toast::push(
                            ToastKind::Info,
                            "Claim booked",
                            "We’ll settle it as soon as the cash is home.",
                        )
                    }),
                    on_click(Button::new(Kind::Secondary, "Neutral").build(), || {
                        toast::push(
                            ToastKind::Neutral,
                            "Rejected in wallet",
                            "Nothing was sent.",
                        )
                    }),
                    on_click(Button::new(Kind::Secondary, "Error").build(), || {
                        toast::push(
                            ToastKind::Error,
                            "Transaction failed",
                            "The network didn’t accept it. Nothing moved.",
                        )
                    }),
                ],
            ),
            row(
                "Dialog (a sheet under 600px)",
                [on_click(
                    Button::new(Kind::Secondary, "Retire Basis Harvester?").build(),
                    || {
                        dialog::open(Dialog {
                    title: "Retire Basis Harvester?".to_string(),
                    body: View::Many(vec![
                        el("p").text("Retiring takes a week, on purpose, so Depositors get a week’s warning. You can’t undo it.").into(),
                        notice(NoticeTone::Caution, "Deposits stop at once. After seven days it stops for good and closes its positions at market, which can lock in a loss.").into(),
                    ]),
                    foot: Some(View::Many(vec![
                        on_click(Button::new(Kind::Secondary, "Keep it running").build(), dialog::close).into(),
                        on_click(Button::new(Kind::Danger, "Retire agent").build(), dialog::close).into(),
                    ])),
                    wide: false,
                });
                    },
                )],
            ),
        ],
    )
}

pub fn page() -> Page {
    Page {
        top: None,
        body: el("div")
            .class("at")
            .child(el("h1").class("at-title").text("Atoms"))
            .child(
                el("p")
                    .class("sh-muted")
                    .text("Every token, type style, icon and atom. Dev builds only."),
            )
            .child(tokens())
            .child(type_styles())
            .child(icons())
            .child(buttons())
            .child(chips())
            .child(figures())
            .child(notices())
            .child(controls())
            .child(overlays())
            .into(),
    }
}
