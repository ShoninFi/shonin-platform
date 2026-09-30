//! The small pieces every page is built from: cards, sections, stats, badges,
//! key-value lists, notices and the bits that make a hash or a threshold
//! readable.

use dominator::{Dom, events, html, link, svg, text};
use futures_signals::signal::{Mutable, SignalExt};
use shared::{AuditStatus, HealthLevel, ScoreBand};

use crate::format as fmt;
use crate::ui::icon;

/// How something should read: neutral, good, worth noticing, or wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Ok,
    Warn,
    Error,
    Info,
}

impl Tone {
    fn badge_class(self) -> &'static str {
        match self {
            Self::Neutral => "badge",
            Self::Accent => "badge--accent",
            Self::Ok => "badge--ok",
            Self::Warn => "badge--warn",
            Self::Error => "badge--err",
            Self::Info => "badge--info",
        }
    }

    fn notice_class(self) -> &'static str {
        match self {
            Self::Ok | Self::Accent | Self::Neutral => "notice--accent",
            Self::Warn => "notice--warn",
            Self::Error => "notice--err",
            Self::Info => "notice--info",
        }
    }

    fn icon(self) -> Dom {
        match self {
            Self::Error => icon::error(),
            Self::Warn => icon::warning(),
            Self::Ok => icon::check(),
            _ => icon::info(),
        }
    }

    /// The tone a health level reads in.
    pub fn from_level(level: HealthLevel) -> Self {
        match level {
            HealthLevel::Ok => Self::Ok,
            HealthLevel::Warning => Self::Warn,
            HealthLevel::Error => Self::Error,
        }
    }

    /// The tone an audit's status reads in.
    pub fn from_audit(status: AuditStatus) -> Self {
        match status {
            AuditStatus::Audited => Self::Ok,
            AuditStatus::InProgress => Self::Warn,
            AuditStatus::Unaudited => Self::Neutral,
        }
    }
}

/// A panel with a hairline border.
pub fn card(children: Vec<Dom>) -> Dom {
    html!("div", {
        .class(["card", "stack"])
        .children(children)
    })
}

/// A card with its own header row.
pub fn card_titled(title: &str, aside: Option<Dom>, children: Vec<Dom>) -> Dom {
    html!("div", {
        .class(["card", "card--flush"])
        .child(html!("div", {
            .class("card__head")
            .child(html!("h3", { .text(title) }))
            .apply(|dom| match aside {
                Some(aside) => dom.child(aside),
                None => dom,
            })
        }))
        .child(html!("div", {
            .class(["card__body", "stack"])
            .children(children)
        }))
    })
}

/// A titled part of a page, with a rule under the heading.
pub fn section(title: &str, children: Vec<Dom>) -> Dom {
    section_with_aside(title, None, children)
}

/// The same, with something on the right of the heading.
pub fn section_with_aside(title: &str, aside: Option<Dom>, children: Vec<Dom>) -> Dom {
    html!("section", {
        .class("stack")
        .child(html!("div", {
            .class("section-head")
            .child(html!("h2", { .text(title) }))
            .apply(|dom| match aside {
                Some(aside) => dom.child(aside),
                None => dom,
            })
        }))
        .children(children)
    })
}

/// A heading with no rule, for a subsection.
pub fn subheading(title: &str) -> Dom {
    html!("h3", { .class("subtitle").text(title) })
}

/// A figure with a label over it.
pub fn stat(label: &str, value: &str, note: Option<&str>) -> Dom {
    stat_toned(label, value, note, Tone::Neutral)
}

/// The same, coloured by how it should read.
pub fn stat_toned(label: &str, value: &str, note: Option<&str>, tone: Tone) -> Dom {
    html!("div", {
        .class("stack-sm")
        .child(html!("div", { .class("stat__label").text(label) }))
        .child(html!("div", {
            .class("stat__value")
            .apply(|dom| match tone {
                Tone::Ok => dom.class("up"),
                Tone::Error => dom.class("down"),
                Tone::Warn => dom.style("color", "var(--warn)"),
                _ => dom,
            })
            .text(value)
        }))
        .apply(|dom| match note {
            Some(note) => dom.child(html!("div", { .class("stat__note").text(note) })),
            None => dom,
        })
    })
}

/// A row of figures that wraps on a narrow screen.
pub fn stats(children: Vec<Dom>) -> Dom {
    html!("div", { .class("stats").children(children) })
}

/// A number that reads as positive or negative.
pub fn signed(value: f64, text_value: &str) -> Dom {
    html!("span", {
        .class("num")
        .apply(|dom| {
            if value > 0.0 {
                dom.class("up")
            } else if value < 0.0 {
                dom.class("down")
            } else {
                dom
            }
        })
        .text(text_value)
    })
}

/// A small label.
pub fn badge(label: &str, tone: Tone) -> Dom {
    html!("span", {
        .class("badge")
        .apply_if(tone != Tone::Neutral, |dom| dom.class(tone.badge_class()))
        .text(label)
    })
}

/// A label with a dot, for a state that is either fine or not.
pub fn badge_dot(label: &str, tone: Tone) -> Dom {
    html!("span", {
        .class("badge")
        .apply_if(tone != Tone::Neutral, |dom| dom.class(tone.badge_class()))
        .child(html!("span", { .class("badge__dot") }))
        .child(text(label))
    })
}

/// A row of labels.
pub fn badges(children: Vec<Dom>) -> Dom {
    html!("div", { .class("badges").children(children) })
}

/// A review score, coloured by band, out of 100.
pub fn score(value: u8, band: ScoreBand) -> Dom {
    html!("span", {
        .class("score")
        .class(match band {
            ScoreBand::High => "score--high",
            ScoreBand::Middling => "score--mid",
            ScoreBand::Low => "score--low",
        })
        .child(html!("span", { .class("score__value").text(&value.to_string()) }))
        .child(html!("span", { .class("score__max").text("/100") }))
    })
}

/// A list of labels and values.
pub fn kv(pairs: Vec<(String, Dom)>) -> Dom {
    html!("dl", {
        .class("kv")
        .children(pairs.into_iter().flat_map(|(key, value)| {
            [
                html!("dt", { .text(&key) }),
                html!("dd", { .child(value) }),
            ]
        }))
    })
}

/// The same, stacked into rows, which reads better on a narrow screen.
pub fn kv_rows(pairs: Vec<(String, Dom)>) -> Dom {
    html!("dl", {
        .class(["kv", "kv--rows"])
        .children(pairs.into_iter().flat_map(|(key, value)| {
            [
                html!("dt", { .class("stat__label").text(&key) }),
                html!("dd", { .child(value) }),
            ]
        }))
    })
}

/// Plain text, as a value in a list.
pub fn value(text_value: &str) -> Dom {
    html!("span", { .text(text_value) })
}

/// A number, in the app's tabular figures.
pub fn number(text_value: &str) -> Dom {
    html!("span", { .class("num").text(text_value) })
}

/// Muted text, for a caveat beside something.
pub fn muted(text_value: &str) -> Dom {
    html!("span", { .class("muted").text(text_value) })
}

/// A paragraph.
pub fn para(text_value: &str) -> Dom {
    html!("p", { .text(text_value) })
}

/// A paragraph in the muted colour, for explanation under a heading.
pub fn note(text_value: &str) -> Dom {
    html!("p", { .class(["muted", "small"]).text(text_value) })
}

/// A bulleted list of plain strings.
pub fn bullets(items: impl IntoIterator<Item = String>) -> Dom {
    html!("ul", {
        .children(items.into_iter().map(|item| html!("li", { .text(&item) })))
    })
}

/// A message where content would be.
pub fn empty(text_value: &str) -> Dom {
    html!("div", { .class("empty").text(text_value) })
}

/// A box that says something the reader should not miss.
pub fn notice(tone: Tone, title: &str, body: &str) -> Dom {
    notice_with(tone, title, body, None)
}

/// The same, with an action or a link after the text.
pub fn notice_with(tone: Tone, title: &str, body: &str, action: Option<Dom>) -> Dom {
    html!("div", {
        .class("notice")
        .class(tone.notice_class())
        .child(tone.icon())
        .child(html!("div", {
            .class("stack-sm")
            .child(html!("div", { .class("notice__title").text(title) }))
            .child(html!("div", { .text(body) }))
            .apply(|dom| match action {
                Some(action) => dom.child(action),
                None => dom,
            })
        }))
    })
}

/// A bar showing how full something is.
pub fn meter(fraction: f64, tone: Tone) -> Dom {
    let width = format!("{:.1}%", (fraction.clamp(0.0, 1.0)) * 100.0);
    html!("div", {
        .class("meter")
        .child(html!("div", {
            .class("meter__fill")
            .apply(|dom| match tone {
                Tone::Warn => dom.class("meter__fill--warn"),
                Tone::Error => dom.class("meter__fill--err"),
                _ => dom,
            })
            .style("width", &width)
        }))
    })
}

/// A bar split into parts, for how a vault's value is spread across venues.
pub fn split_bar(parts: Vec<(f64, &'static str, String)>) -> Dom {
    html!("div", {
        .class("bar-split")
        .children(parts.into_iter().map(|(fraction, colour, label)| {
            html!("div", {
                .class("bar-split__part")
                .attr("title", &label)
                .style("width", &format!("{:.2}%", fraction.clamp(0.0, 1.0) * 100.0))
                .style("background", colour)
            })
        }))
    })
}

/// A hash or an address, elided, with a button that copies the whole thing.
/// A command to run, shown as one, and copyable because it is meant to be.
pub fn code_block(command: &str) -> Dom {
    html!("div", {
        .class(["code-block", "num"])
        .child(copyable(command, command))
    })
}

pub fn hash(full: &str) -> Dom {
    copyable(&fmt::short_hash(full), full)
}

/// The same for an address.
pub fn address(full: &str) -> Dom {
    copyable(&fmt::short_address(full), full)
}

/// Text that copies something longer when clicked.
pub fn copyable(shown: &str, full: &str) -> Dom {
    let full = full.to_string();
    let copied = Mutable::new(false);

    html!("button", {
        .class("copy")
        .attr("type", "button")
        .attr("title", &format!("Copy {full}"))
        .child(html!("span", { .text(shown) }))
        .child_signal(copied.signal().map(|copied| {
            Some(if copied {
                icon::check()
            } else {
                icon::copy()
            })
        }))
        .event(move |_: events::Click| {
            if let Some(clipboard) = web_sys::window().map(|window| window.navigator().clipboard()) {
                let _ = clipboard.write_text(&full);
            }
            copied.set(true);
        })
    })
}

/// A word with an explanation under it, for a term the reader may not know.
pub fn tip(term: &str, explanation: &str) -> Dom {
    html!("span", {
        .class("tip")
        .attr("tabindex", "0")
        .attr("role", "note")
        .text(term)
        .child(html!("span", { .class("tip__bubble").text(explanation) }))
    })
}

/// A link inside a sentence.
pub fn inline_link(path: &str, label: &str) -> Dom {
    link!(path.to_string(), { .text(label) })
}

/// A link that reads as an action.
pub fn action_link(path: &str, label: &str) -> Dom {
    link!(path.to_string(), {
        .class(["btn", "btn--ghost"])
        .text(label)
        .child(icon::arrow_right())
    })
}

/// A horizontal rule.
pub fn divider() -> Dom {
    html!("div", { .class("divider") })
}

/// A progress ring for a countdown, drawn as an arc.
pub fn ring(fraction: f64, label: &str) -> Dom {
    let radius = 14.0;
    let circumference = 2.0 * std::f64::consts::PI * radius;
    let filled = circumference * fraction.clamp(0.0, 1.0);

    html!("div", {
        .class("row")
        .child(svg!("svg", {
            .attr("viewBox", "0 0 36 36")
            .attr("width", "36")
            .attr("height", "36")
            .attr("aria-hidden", "true")
            .child(svg!("circle", {
                .attr("cx", "18")
                .attr("cy", "18")
                .attr("r", &radius.to_string())
                .attr("fill", "none")
                .attr("stroke", "var(--ink-4)")
                .attr("stroke-width", "3")
            }))
            .child(svg!("circle", {
                .attr("cx", "18")
                .attr("cy", "18")
                .attr("r", &radius.to_string())
                .attr("fill", "none")
                .attr("stroke", "var(--accent)")
                .attr("stroke-width", "3")
                .attr("stroke-linecap", "round")
                .attr("stroke-dasharray", &format!("{filled} {circumference}"))
                .attr("transform", "rotate(-90 18 18)")
            }))
        }))
        .child(html!("span", { .class(["num", "small"]).text(label) }))
    })
}
