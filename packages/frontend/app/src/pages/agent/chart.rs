//! The share-price chart: the agent's share price (or the money in its
//! vault) day by day, with its high-water mark, a range control, a crosshair
//! that snaps to the day, keys that move it, and a table of the same days for
//! anyone who can't or won't read a chart. The plot is drawn as SVG in the
//! pre-render, so it shows with JavaScript off; the app adds the crosshair.
//!
//! No inline styles (the policy forbids them): everything placed by a figure
//! is placed by an SVG attribute in percent, and the tooltip by CSSOM.

use std::cell::Cell;
use std::rc::Rc;

use futures_signals::map_ref;
use futures_signals::signal::{Mutable, SignalExt};
use shared::{Point, Strategy};
use time::Date;
use ui::icons::Icon;
use wasm_bindgen::JsCast;

use super::facts::{self, Anchor, Mode, Range, Scale};
use crate::atoms::badge::{Tone, badge};
use crate::atoms::segmented::segmented;
use crate::format;
use crate::store::Agent;
use crate::view::{El, View, el, svg};

/// Everything the plot draws for one range and mode.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub mode: Mode,
    pub points: Vec<(Date, f64)>,
    pub scale: Scale,
    pub path: Option<String>,
    /// The high-water mark as the vault holds it now, on the price chart.
    pub mark: Option<f64>,
    pub summary: String,
    pub this_year: i32,
}

impl Model {
    pub fn of(strategy: &Strategy, range: Range, mode: Mode) -> Model {
        let empty = Vec::new();
        let series: &[Point] = match (&strategy.record, mode) {
            (Some(record), Mode::Price) => &record.value_per_share,
            (Some(record), Mode::Money) => &record.aum,
            (None, _) => &empty,
        };
        let points = facts::window(series, range);
        let mark = (mode == Mode::Price && strategy.vault.high_water_mark > 0.0)
            .then_some(strategy.vault.high_water_mark);
        let mut values: Vec<f64> = points.iter().map(|point| point.value).collect();
        if let Some(mark) = mark {
            values.push(mark);
        }
        let scale = Scale::of(&values, facts::min_step(mode));
        let this_year = points.last().map(|point| point.date.year()).unwrap_or(2026);
        Model {
            mode,
            points: points
                .iter()
                .map(|point| (point.date, point.value))
                .collect(),
            path: facts::path(points, &scale),
            mark,
            summary: facts::summary(points, mode),
            scale,
            this_year,
        }
    }

    fn first_last(&self) -> Option<(Date, Date)> {
        Some((self.points.first()?.0, self.points.last()?.0))
    }

    /// Where day `index` sits: across (0–1) and down (0–1).
    fn at(&self, index: usize) -> Option<(f64, f64)> {
        let (first, last) = self.first_last()?;
        let (date, value) = *self.points.get(index)?;
        Some((facts::x(date, first, last), self.scale.y(value)))
    }

    fn day_words(&self, index: usize) -> Option<(String, String)> {
        let (date, value) = *self.points.get(index)?;
        Some((facts::full_day(date), facts::figure(value, self.mode)))
    }
}

/// The chart's own state, kept for as long as the page is open.
#[derive(Clone)]
pub struct ChartState {
    pub range: Mutable<Range>,
    pub mode: Mutable<Mode>,
    /// The day the crosshair is on, as an index into the model's points.
    pub cursor: Mutable<Option<usize>>,
    pub model: Mutable<Rc<Model>>,
    /// Whether the line has drawn in already on this page view.
    drawn: Rc<Cell<bool>>,
    slug: String,
}

impl ChartState {
    pub fn new(agent: &Agent) -> Self {
        let range = Range::Quarter;
        let mode = Mode::Price;
        let model = Model::of(&agent.strategy.lock_ref(), range, mode);
        Self {
            range: Mutable::new(range),
            mode: Mutable::new(mode),
            cursor: Mutable::new(None),
            model: Mutable::new(Rc::new(model)),
            drawn: Rc::new(Cell::new(false)),
            slug: agent.slug.clone(),
        }
    }
}

fn pct(fraction: f64) -> String {
    format!("{:.3}%", fraction * 100.0)
}

/// The headline over the chart: the latest figure, or the day under the
/// crosshair while someone reads the chart.
fn headline(agent: &Agent, state: &ChartState) -> El {
    let strategy = agent.strategy.clone();
    let now_text = |strategy: &Strategy, model: &Model, cursor: Option<usize>| -> String {
        if let Some((_, value)) = cursor.and_then(|index| model.points.get(index)) {
            return facts::figure(*value, model.mode);
        }
        match model.mode {
            Mode::Price => format::price(strategy.vault.share_price),
            Mode::Money => format::money_whole(crate::model::in_vault(strategy)),
        }
    };
    let now = now_text(
        &strategy.lock_ref(),
        &state.model.lock_ref(),
        state.cursor.get(),
    );
    let signal = map_ref! {
        let strategy = strategy.signal_cloned(),
        let model = state.model.signal_cloned(),
        let cursor = state.cursor.signal() => now_text(strategy, model, *cursor)
    };
    let label_now = match state.mode.get() {
        Mode::Price => "Share price",
        Mode::Money => "Money in vault",
    };
    el("div")
        .class("ag-chart__headline")
        .child(
            el("h2")
                .class("sh-label")
                .attr("id", "ag-chart-title")
                .text_signal(
                    label_now,
                    state.mode.signal().map(|mode| {
                        match mode {
                            Mode::Price => "Share price",
                            Mode::Money => "Money in vault",
                        }
                        .to_string()
                    }),
                ),
        )
        .child(
            el("p")
                .class("ag-chart__figure")
                .child(el("span").class("ag-chart__value").text_signal(now, signal))
                .child_from(&agent.figures, |figures| {
                    figures
                        .since_opening
                        .map(|since| since_opening(since).into())
                }),
        )
}

/// The share price's change since the vault opened, as a chip with its sign,
/// its arrow and its period.
pub(super) fn since_opening(since: f64) -> El {
    let (tone, glyph) = match format::direction(since) {
        format::Direction::Up => (Tone::Gain, Some(Icon::Up)),
        format::Direction::Down => (Tone::Loss, Some(Icon::Down)),
        format::Direction::Flat => (Tone::Neutral, None),
    };
    badge(tone, glyph, format!("{} since opening", format::ret(since)))
}

/// The chart card.
pub fn chart(agent: &Rc<Agent>, state: &ChartState) -> El {
    let model_signal = {
        let state = state.clone();
        map_ref! {
            let strategy = agent.strategy.signal_cloned(),
            let range = state.range.signal(),
            let mode = state.mode.signal() => Model::of(strategy, *range, *mode)
        }
        .dedupe_cloned()
    };
    let follow = {
        let state = state.clone();
        move |dom: dominator::DomBuilder<web_sys::Element>| {
            dom.future(model_signal.for_each(move |model| {
                let model = Rc::new(model);
                if *state.model.lock_ref() != model {
                    state.cursor.set(None);
                    state.model.set(model);
                }
                async {}
            }))
        }
    };
    let legend_mark = |model: &Rc<Model>| {
        model.mark.map(|mark| {
            el("span")
                .class("ag-chart__key")
                .child(
                    el("span")
                        .class("ag-chart__swatch ag-chart__swatch--mark")
                        .attr("aria-hidden", "true"),
                )
                .text(format!("High-water mark {}", format::price(mark)))
                .into()
        })
    };
    let price_label = |mode: &Mode| match mode {
        Mode::Price => "Share price".to_string(),
        Mode::Money => "Money in vault".to_string(),
    };
    el("section")
        .class("sh-card")
        .class("ag-chart")
        .class("ag-o-chart")
        .attr("aria-labelledby", "ag-chart-title")
        .hook(follow)
        .child(
            el("div")
                .class("ag-chart__head")
                .child(headline(agent, state))
                .child(
                    el("div")
                        .class("ag-chart__controls")
                        .child(segmented("Time range", &Range::ALL, &state.range))
                        .child(segmented(
                            "Chart shows",
                            &[(Mode::Price, "Price"), (Mode::Money, "Money in vault")],
                            &state.mode,
                        )),
                ),
        )
        .child(
            el("div")
                .class("ag-chart__legend")
                .child(
                    el("span")
                        .class("ag-chart__key")
                        .child(
                            el("span")
                                .class("ag-chart__swatch")
                                .attr("aria-hidden", "true"),
                        )
                        .text_signal(
                            price_label(&state.mode.get()),
                            state.mode.signal_ref(price_label),
                        ),
                )
                .child_from(&state.model, legend_mark),
        )
        .child(el("div").class("sh-contents").child_from(&state.model, {
            let state = state.clone();
            move |model| Some(plot(model, &state).into())
        }))
        .child(
            el("div")
                .class("ag-chart__foot")
                .child(
                    el("p")
                        .class("ag-chart__note")
                        .child(
                            el("span")
                                .class("ag-chart__note-wide")
                                .text("Read from the vault contract. After fees."),
                        )
                        .child(
                            el("span")
                                .class("ag-chart__note-phone")
                                .text("After fees. Touch and hold to read a day."),
                        ),
                )
                .child(
                    el("div")
                        .class("sh-contents")
                        .child_from(&state.model, |model| Some(table(model).into())),
                ),
        )
}

/// The plot: axis labels, gridlines, the high-water mark, the line, and the
/// crosshair with its tooltip.
fn plot(model: &Rc<Model>, state: &ChartState) -> El {
    let Some(path) = &model.path else {
        let days = model.points.len();
        return el("p").class("ag-chart__empty").text(if days == 0 {
            "No record yet. The chart starts with the vault's first valuation.".to_string()
        } else {
            format!(
                "Only {} of record so far. The chart draws from the second day.",
                format::days(days as u32)
            )
        });
    };
    let draw = cfg!(target_arch = "wasm32") && !state.drawn.replace(true);
    let scale = &model.scale;

    let y_axis = svg("svg")
        .class("ag-plot__y")
        .attr("aria-hidden", "true")
        .children(model.scale.ticks.iter().map(|tick| {
            svg("text")
                .attr("x", "100%")
                .attr("y", pct(scale.y(*tick)))
                .attr("dy", "0.35em")
                .attr("text-anchor", "end")
                .text(facts::axis_label(*tick, scale.step, model.mode))
        }));

    let (first, last) = model.first_last().expect("a path has points");
    let x_axis = svg("svg")
        .class("ag-plot__x")
        .attr("aria-hidden", "true")
        .children(
            facts::x_labels(first, last)
                .into_iter()
                .map(|(at, day, anchor)| {
                    svg("text")
                        .attr("x", pct(at))
                        .attr("y", "12")
                        .attr(
                            "text-anchor",
                            match anchor {
                                Anchor::Start => "start",
                                Anchor::Middle => "middle",
                                Anchor::End => "end",
                            },
                        )
                        .text(facts::short_day(day, model.this_year))
                }),
        );

    let grid = scale.ticks.iter().map(|tick| {
        let y = pct(scale.y(*tick));
        svg("line")
            .class("ag-plot__grid")
            .attr("x1", "0")
            .attr("x2", "100%")
            .attr("y1", y.clone())
            .attr("y2", y)
    });

    let mark = model.mark.map(|mark| {
        let y = pct(scale.y(mark));
        svg("line")
            .class("ag-plot__mark")
            .class_if(draw, "ag-plot__mark--fade")
            .attr("x1", "0")
            .attr("x2", "100%")
            .attr("y1", y.clone())
            .attr("y2", y)
    });

    let line = svg("svg")
        .class("ag-plot__lines")
        .attr("viewBox", "0 0 1000 1000")
        .attr("preserveAspectRatio", "none")
        .attr("width", "100%")
        .attr("height", "100%")
        .child(
            svg("path")
                .class("ag-plot__line")
                .class_if(draw, "sh-draw")
                .attr("d", path.clone())
                .attr("pathLength", "1"),
        );

    let cursor = state.cursor.clone();
    let model_rc = model.clone();
    let at = {
        let model = model.clone();
        move |index: &Option<usize>| index.and_then(|index| model.at(index))
    };
    let crosshair = svg("line")
        .class("ag-plot__cross")
        .attr("y1", "0")
        .attr("y2", "100%")
        .class_from("ag-plot__on", &cursor, |index| index.is_some())
        .attr_from("x1", &cursor, {
            let at = at.clone();
            move |index| at(index).map(|(x, _)| pct(x))
        })
        .attr_from("x2", &cursor, {
            let at = at.clone();
            move |index| at(index).map(|(x, _)| pct(x))
        });
    let dot = svg("circle")
        .class("ag-plot__dot")
        .attr("r", "4")
        .class_from("ag-plot__on", &cursor, |index| index.is_some())
        .attr_from("cx", &cursor, {
            let at = at.clone();
            move |index| at(index).map(|(x, _)| pct(x))
        })
        .attr_from("cy", &cursor, {
            let at = at.clone();
            move |index| at(index).map(|(_, y)| pct(y))
        });

    let words = {
        let model = model.clone();
        move |index: &Option<usize>| index.and_then(|index| model.day_words(index))
    };
    let series_word = match model.mode {
        Mode::Price => "Share price",
        Mode::Money => "Money in vault",
    };
    let tip = el("div")
        .class("ag-plot__tip")
        .attr("aria-hidden", "true")
        .class_from("ag-plot__on", &cursor, |index| index.is_some())
        .child(el("span").class("ag-plot__tip-day").text_from(&cursor, {
            let words = words.clone();
            move |index| words(index).map(|(day, _)| day).unwrap_or_default()
        }))
        .child(
            el("span")
                .class("ag-plot__tip-row")
                .child(
                    el("span")
                        .class("ag-chart__swatch")
                        .attr("aria-hidden", "true"),
                )
                .text(series_word)
                .child(el("strong").text_from(&cursor, {
                    let words = words.clone();
                    move |index| words(index).map(|(_, value)| value).unwrap_or_default()
                })),
        )
        .hook({
            let cursor = cursor.clone();
            let model = model.clone();
            move |dom| {
                dominator::with_node!(dom, node => {
                    .future(cursor.signal().for_each(move |index| {
                        place_tip(&node, index.and_then(|index| model.at(index)));
                        async {}
                    }))
                })
            }
        });

    let reading =
        el("span")
            .class("sh-vh")
            .attr("role", "status")
            .text_from(&cursor, move |index| {
                words(index)
                    .map(|(day, value)| format!("{day}: {value}"))
                    .unwrap_or_default()
            });

    let area = el("div")
        .class("ag-plot__area")
        .attr("tabindex", "0")
        .attr("role", "img")
        .attr("aria-label", model.summary.clone())
        .attr(
            "aria-description",
            "Arrow keys move a day, Shift with an arrow a week, Home and End go to the ends.",
        )
        .child(
            svg("svg")
                .class("ag-plot__svg")
                .attr("aria-hidden", "true")
                .attr("width", "100%")
                .attr("height", "100%")
                .children(grid)
                .child(View::from(mark.map(View::from)))
                .child(line)
                .child(crosshair)
                .child(dot),
        )
        .child(tip)
        .hook(super::transition_name(format!(
            "agent-{}-spark",
            state.slug
        )))
        .hook(move |dom| interact(dom, model_rc, cursor));

    el("div")
        .class("ag-plot")
        .child(y_axis)
        .child(area)
        .child(reading)
        .child(
            el("span")
                .class("ag-plot__corner")
                .attr("aria-hidden", "true"),
        )
        .child(x_axis)
}

/// Put the tooltip beside the crosshair: to its right, or to its left within
/// 300px of the plot's right edge.
fn place_tip(node: &web_sys::Element, at: Option<(f64, f64)>) {
    let Some(tip) = node.dyn_ref::<web_sys::HtmlElement>() else {
        return;
    };
    let Some((x, _)) = at else {
        return;
    };
    let Some(area) = tip.parent_element() else {
        return;
    };
    let width = area.get_bounding_client_rect().width();
    let px = x * width;
    let style = tip.style();
    if width - px < 300.0 {
        let _ = style.set_property("left", "auto");
        let _ = style.set_property("right", &format!("{:.1}px", width - px + 12.0));
    } else {
        let _ = style.set_property("right", "auto");
        let _ = style.set_property("left", &format!("{:.1}px", px + 12.0));
    }
}

/// The crosshair follows the pointer (a press and drag on a touch screen) and
/// the keys: ← → a day, with Shift a week, Home and End the ends, Esc hides.
fn interact(
    dom: dominator::DomBuilder<web_sys::Element>,
    model: Rc<Model>,
    cursor: Mutable<Option<usize>>,
) -> dominator::DomBuilder<web_sys::Element> {
    use dominator::events::{
        Blur, Focus, KeyDown, PointerCancel, PointerDown, PointerLeave, PointerMove, PointerUp,
        TouchStart,
    };
    let count = model.points.len();
    let pick = {
        let model = model.clone();
        let cursor = cursor.clone();
        move |element: &web_sys::Element, client_x: f64| {
            let rect = element.get_bounding_client_rect();
            if rect.width() <= 0.0 {
                return;
            }
            let fraction = (client_x - rect.left()) / rect.width();
            let points: Vec<Point> = model
                .points
                .iter()
                .map(|(date, value)| Point {
                    date: *date,
                    value: *value,
                })
                .collect();
            cursor.set_neq(facts::nearest(&points, fraction));
        }
    };
    let touching = Rc::new(Cell::new(false));
    dominator::with_node!(dom, node => {
        .event({
            let pick = pick.clone();
            let node = node.clone();
            move |event: PointerDown| {
                pick(&node, f64::from(event.mouse_x()));
            }
        })
        .event({
            let touching = touching.clone();
            move |_: TouchStart| touching.set(true)
        })
        .event({
            let pick = pick.clone();
            let node = node.clone();
            move |event: PointerMove| {
                pick(&node, f64::from(event.mouse_x()));
            }
        })
        .event({
            let cursor = cursor.clone();
            let touching = touching.clone();
            move |_: PointerUp| {
                if touching.replace(false) {
                    cursor.set_neq(None);
                }
            }
        })
        .event({
            let cursor = cursor.clone();
            let touching = touching.clone();
            move |_: PointerCancel| {
                touching.set(false);
                cursor.set_neq(None);
            }
        })
        .event({
            let cursor = cursor.clone();
            let node = node.clone();
            move |_: PointerLeave| {
                let focused = crate::web::document()
                    .and_then(|document| document.active_element())
                    .is_some_and(|active| active == node);
                if !focused {
                    cursor.set_neq(None);
                }
            }
        })
        .event({
            let cursor = cursor.clone();
            move |_: Focus| {
                if cursor.get().is_none() && count > 0 {
                    cursor.set(Some(count - 1));
                }
            }
        })
        .event({
            let cursor = cursor.clone();
            move |_: Blur| cursor.set_neq(None)
        })
        .event_with_options(&dominator::EventOptions::preventable(), {
            let cursor = cursor.clone();
            move |event: KeyDown| {
                if count == 0 {
                    return;
                }
                let last = count - 1;
                let here = cursor.get().unwrap_or(last);
                let step = if event.shift_key() { 7 } else { 1 };
                let next = match event.key().as_str() {
                    "ArrowLeft" => Some(here.saturating_sub(step)),
                    "ArrowRight" => Some((here + step).min(last)),
                    "Home" => Some(0),
                    "End" => Some(last),
                    "Escape" => {
                        cursor.set_neq(None);
                        return;
                    }
                    _ => return,
                };
                event.prevent_default();
                cursor.set_neq(next);
            }
        })
    })
}

/// "View as table": the same days as a real table, in the page with
/// JavaScript off too.
fn table(model: &Rc<Model>) -> El {
    let (what, caption) = match model.mode {
        Mode::Price => ("Share price", "Share price by day, after fees"),
        Mode::Money => ("Money in vault", "Money in vault by day"),
    };
    let disclosure = el("details").class("ag-chart__table").child(
        el("summary")
            .class("ag-chart__table-toggle")
            .text("View as table"),
    );
    if model.points.is_empty() {
        return el("span");
    }
    disclosure.child(
        el("div").class("ag-chart__table-wrap").child(
            el("table")
                .class("sh-table")
                .child(el("caption").class("sh-vh").text(caption))
                .child(
                    el("thead").child(
                        el("tr")
                            .child(el("th").attr("scope", "col").text("Day"))
                            .child(el("th").attr("scope", "col").class("sh-r").text(what)),
                    ),
                )
                .child(
                    el("tbody").children(model.points.iter().rev().map(|(date, value)| {
                        el("tr")
                            .child(el("th").attr("scope", "row").text(facts::full_day(*date)))
                            .child(
                                el("td")
                                    .class("sh-r")
                                    .class("sh-fig")
                                    .text(facts::figure(*value, model.mode)),
                            )
                    })),
                ),
        ),
    )
}
