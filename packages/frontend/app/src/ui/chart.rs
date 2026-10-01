//! The app's charts: inline SVG, no dependency, and no canvas.
//!
//! Paths are drawn in a fixed coordinate space and stretched to the container,
//! with non-scaling strokes so lines keep their weight. Everything that must
//! not stretch — axis labels, the hover cursor and its readout — is HTML
//! positioned over the plot, so text stays upright and legible at any width.

use std::rc::Rc;

use dominator::{Dom, clone, events, html, svg, text};
use futures_signals::signal::{Mutable, SignalExt};
use shared::Point;

use crate::format as fmt;

/// The coordinate space paths are drawn in before being stretched.
const WIDTH: f64 = 1000.0;
const HEIGHT: f64 = 300.0;

/// One line on a chart.
pub struct Series {
    pub label: &'static str,
    pub points: Vec<Point>,
    /// A second line reads as a reference: thinner, dashed, muted.
    pub muted: bool,
}

impl Series {
    pub fn new(label: &'static str, points: Vec<Point>) -> Self {
        Self {
            label,
            points,
            muted: false,
        }
    }

    pub fn reference(label: &'static str, points: Vec<Point>) -> Self {
        Self {
            label,
            points,
            muted: true,
        }
    }
}

struct Bounds {
    low: f64,
    high: f64,
}

impl Bounds {
    fn of(series: &[Series]) -> Self {
        let mut low = f64::MAX;
        let mut high = f64::MIN;
        for point in series.iter().flat_map(|series| series.points.iter()) {
            low = low.min(point.value);
            high = high.max(point.value);
        }
        if !low.is_finite() || !high.is_finite() {
            return Self {
                low: 0.0,
                high: 1.0,
            };
        }
        // A flat series would divide by zero, and a chart with no headroom
        // reads as though it is touching its own edges.
        let padding = ((high - low) * 0.08).max(f64::EPSILON.max(high.abs() * 0.002));
        Self {
            low: low - padding,
            high: high + padding,
        }
    }

    fn y(&self, value: f64) -> f64 {
        let span = (self.high - self.low).max(f64::MIN_POSITIVE);
        HEIGHT - ((value - self.low) / span) * HEIGHT
    }

    /// Where a value sits in the plot, from the top, as a percentage.
    fn y_percent(&self, value: f64) -> f64 {
        self.y(value) / HEIGHT * 100.0
    }
}

fn x(index: usize, count: usize) -> f64 {
    if count <= 1 {
        return 0.0;
    }
    index as f64 / (count - 1) as f64 * WIDTH
}

fn path_of(points: &[Point], bounds: &Bounds) -> String {
    let count = points.len();
    points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            format!(
                "{}{:.2} {:.2}",
                if index == 0 { "M" } else { "L" },
                x(index, count),
                bounds.y(point.value)
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn area_of(points: &[Point], bounds: &Bounds, baseline: f64) -> String {
    if points.is_empty() {
        return String::new();
    }
    let count = points.len();
    let base = bounds.y(baseline);
    format!(
        "M0 {base:.2} {} L{:.2} {base:.2} Z",
        points
            .iter()
            .enumerate()
            .map(|(index, point)| format!("L{:.2} {:.2}", x(index, count), bounds.y(point.value)))
            .collect::<Vec<_>>()
            .join(" "),
        WIDTH
    )
}

/// A line chart with a hover readout: value per share, the money in a vault,
/// or anything else measured day by day.
pub fn line_chart(series: Vec<Series>, format: impl Fn(f64) -> String + 'static) -> Dom {
    chart(series, Rc::new(format), false)
}

/// The same, with the area under the line filled: for a drawdown, where the
/// distance from zero is the point.
pub fn area_chart(series: Vec<Series>, format: impl Fn(f64) -> String + 'static) -> Dom {
    chart(series, Rc::new(format), true)
}

type Format = Rc<dyn Fn(f64) -> String>;

fn chart(series: Vec<Series>, format: Format, filled: bool) -> Dom {
    if series.iter().all(|series| series.points.len() < 2) {
        return html!("div", {
            .class("empty")
            .text("Not enough history to draw yet.")
        });
    }

    let bounds = Rc::new(Bounds::of(&series));
    let series = Rc::new(series);
    // Which day the pointer is over, if any.
    let hover: Mutable<Option<usize>> = Mutable::new(None);
    let count = series
        .iter()
        .map(|series| series.points.len())
        .max()
        .unwrap_or(0);

    let paths: Vec<Dom> = series
        .iter()
        .flat_map(|one| {
            let mut drawn = Vec::new();
            if filled {
                let baseline = if bounds.low <= 0.0 && bounds.high >= 0.0 {
                    0.0
                } else {
                    bounds.low
                };
                drawn.push(svg!("path", {
                    .class("chart__area")
                    .apply_if(one.points.iter().any(|point| point.value < 0.0), |dom| {
                        dom.class("chart__area--down")
                    })
                    .attr("d", &area_of(&one.points, &bounds, baseline))
                }));
            }
            drawn.push(svg!("path", {
                .class("chart__line")
                .apply_if(one.muted, |dom| dom.class("chart__line--muted"))
                .attr("d", &path_of(&one.points, &bounds))
            }));
            drawn
        })
        .collect();

    let grid: Vec<Dom> = (0..=4)
        .map(|step| {
            let y = f64::from(step) / 4.0 * HEIGHT;
            svg!("line", {
                .class("chart__grid")
                .attr("x1", "0")
                .attr("x2", &WIDTH.to_string())
                .attr("y1", &format!("{y:.1}"))
                .attr("y2", &format!("{y:.1}"))
            })
        })
        .collect();

    let label = format!(
        "Chart: {}. The figures beside it are the values on the day under the pointer.",
        series
            .iter()
            .map(|one| one.label)
            .collect::<Vec<_>>()
            .join(" against ")
    );

    html!("div", {
        .class(["chart", "stack-sm"])
        .attr("role", "img")
        .attr("aria-label", &label)
        .child(readout(&series, &hover, &format))
        .child(html!("div", {
            .class("chart__plot")
            .child(svg!("svg", {
                .class("chart__svg")
                .attr("viewBox", &format!("0 0 {WIDTH} {HEIGHT}"))
                .attr("preserveAspectRatio", "none")
                .attr("aria-hidden", "true")
                .children(grid)
                .children(paths)
            }))
            .children(y_labels(&bounds, &format))
            .child(cursor(&series, &bounds, &hover))
            // The pointer target is the whole plot, so the readout follows the
            // pointer anywhere over it rather than only on the line.
            .event(clone!(hover => move |event: events::MouseMove| {
                let Some(element) = event.dyn_target::<web_sys::Element>() else {
                    return;
                };
                let width = element.get_bounding_client_rect().width();
                if width <= 0.0 || count == 0 {
                    return;
                }
                let fraction = (f64::from(event.offset_x()) / width).clamp(0.0, 1.0);
                let index = (fraction * (count - 1) as f64).round() as usize;
                hover.set_neq(Some(index.min(count - 1)));
            }))
            .event(clone!(hover => move |_: events::MouseLeave| hover.set_neq(None)))
        }))
        .child(x_labels(&series))
        .apply_if(series.len() > 1, |dom| dom.child(legend(&series)))
    })
}

fn readout(series: &Rc<Vec<Series>>, hover: &Mutable<Option<usize>>, format: &Format) -> Dom {
    let series = series.clone();
    let format = format.clone();

    html!("div", {
        .class("chart__readout")
        .children_signal_vec(hover.signal().map(move |hovered| {
            let index = hovered.unwrap_or_else(|| {
                series
                    .first()
                    .map_or(0, |one| one.points.len().saturating_sub(1))
            });

            let mut parts = Vec::new();
            if let Some(point) = series.first().and_then(|one| one.points.get(index)) {
                parts.push(html!("span", {
                    .class("faint")
                    .text(&fmt::date(point.date))
                }));
            }
            for one in series.iter() {
                if let Some(point) = one.points.get(index) {
                    parts.push(html!("span", {
                        .child(html!("span", { .class("faint").text(one.label) }))
                        .child(text(" "))
                        .child(html!("span", { .text(&format(point.value)) }))
                    }));
                }
            }
            parts
        }).to_signal_vec())
    })
}

fn y_labels(bounds: &Rc<Bounds>, format: &Format) -> Vec<Dom> {
    let steps = [0.0, 0.5, 1.0];
    steps
        .into_iter()
        .map(|fraction| {
            let value = bounds.low + (bounds.high - bounds.low) * (1.0 - fraction);
            html!("span", {
                .class("chart__ylabel")
                .style("top", &format!("{:.1}%", fraction * 100.0))
                .text(&format(value))
            })
        })
        .collect()
}

fn x_labels(series: &Rc<Vec<Series>>) -> Dom {
    let points = series
        .first()
        .map(|one| one.points.as_slice())
        .unwrap_or_default();
    let labels: Vec<String> = [0.0, 0.5, 1.0]
        .into_iter()
        .filter_map(|fraction| {
            let index = ((points.len().saturating_sub(1)) as f64 * fraction) as usize;
            points.get(index).map(|point| fmt::day_month(point.date))
        })
        .collect();

    html!("div", {
        .class("chart__xlabels")
        .children(labels.into_iter().map(|label| {
            html!("span", { .text(&label) })
        }))
    })
}

fn cursor(series: &Rc<Vec<Series>>, bounds: &Rc<Bounds>, hover: &Mutable<Option<usize>>) -> Dom {
    let series = series.clone();
    let bounds = bounds.clone();

    html!("div", {
        .children_signal_vec(hover.signal().map(move |hovered| {
            let Some(index) = hovered else {
                return Vec::new();
            };
            let count = series
                .first()
                .map_or(1, |one| one.points.len().max(1));
            let left = if count <= 1 {
                0.0
            } else {
                index as f64 / (count - 1) as f64 * 100.0
            };

            let mut parts = vec![html!("div", {
                .class("chart__cursor-line")
                .style("left", &format!("{left:.2}%"))
            })];
            for one in series.iter() {
                if one.muted {
                    continue;
                }
                if let Some(point) = one.points.get(index) {
                    parts.push(html!("div", {
                        .class("chart__cursor-dot")
                        .style("left", &format!("{left:.2}%"))
                        .style("top", &format!("{:.2}%", bounds.y_percent(point.value)))
                    }));
                }
            }
            parts
        }).to_signal_vec())
    })
}

fn legend(series: &Rc<Vec<Series>>) -> Dom {
    let keys: Vec<Dom> = series
        .iter()
        .map(|one| {
            html!("span", {
                .class("legend__key")
                .child(html!("span", {
                    .class("legend__swatch")
                    .apply_if(one.muted, |dom| dom.class("legend__swatch--muted"))
                }))
                .child(text(one.label))
            })
        })
        .collect();

    html!("div", { .class("legend").children(keys) })
}

/// A bar for each month, up or down. Values are fractions, not levels.
pub fn bar_chart(points: Vec<Point>) -> Dom {
    if points.is_empty() {
        return html!("div", { .class("empty").text("No months yet.") });
    }

    let extent = points
        .iter()
        .map(|point| point.value.abs())
        .fold(0.0, f64::max)
        .max(f64::MIN_POSITIVE);
    let count = points.len() as f64;
    let gap = (WIDTH / count) * 0.25;
    let bar_width = (WIDTH / count) - gap;
    let middle = HEIGHT / 2.0;

    let bars: Vec<Dom> = points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            let height = (point.value.abs() / extent) * (HEIGHT / 2.0 - 4.0);
            let y = if point.value >= 0.0 {
                middle - height
            } else {
                middle
            };
            svg!("rect", {
                .class("chart__bar")
                .apply_if(point.value < 0.0, |dom| dom.class("chart__bar--down"))
                .attr("x", &format!("{:.2}", index as f64 * (WIDTH / count) + gap / 2.0))
                .attr("y", &format!("{y:.2}"))
                .attr("width", &format!("{bar_width:.2}"))
                .attr("height", &format!("{:.2}", height.max(1.0)))
            })
        })
        .collect();

    let labels: Vec<Dom> = points
        .iter()
        .map(|point| {
            html!("span", {
                .attr("title", &format!(
                    "{}: {}",
                    fmt::month_year(point.date),
                    fmt::percent_signed(point.value, 2)
                ))
                .text(&fmt::month_year(point.date).replace(' ', "\u{00a0}"))
            })
        })
        .collect();

    html!("div", {
        .class(["chart", "stack-sm"])
        .attr("role", "img")
        .attr("aria-label", "Chart: return in each month covered by the record.")
        .child(html!("div", {
            .class(["chart__plot", "chart__plot--short"])
            .child(svg!("svg", {
                .class("chart__svg")
                .attr("viewBox", &format!("0 0 {WIDTH} {HEIGHT}"))
                .attr("preserveAspectRatio", "none")
                .attr("aria-hidden", "true")
                .child(svg!("line", {
                    .class("chart__grid")
                    .attr("x1", "0")
                    .attr("x2", &WIDTH.to_string())
                    .attr("y1", &middle.to_string())
                    .attr("y2", &middle.to_string())
                }))
                .children(bars)
            }))
        }))
        .child(html!("div", { .class("chart__xlabels").children(labels) }))
    })
}

/// The shape of a record, small enough to sit in a table row.
pub fn sparkline(points: &[Point]) -> Dom {
    if points.len() < 2 {
        return html!("span", { .class("faint").text("—") });
    }

    let bounds = Bounds {
        low: points
            .iter()
            .map(|point| point.value)
            .fold(f64::MAX, f64::min),
        high: points
            .iter()
            .map(|point| point.value)
            .fold(f64::MIN, f64::max),
    };
    let rising = points.last().map(|point| point.value).unwrap_or_default()
        >= points.first().map(|point| point.value).unwrap_or_default();

    // A sparkline is read in one glance, so it samples rather than drawing
    // every day of a long record.
    let step = (points.len() / 60).max(1);
    let sampled: Vec<Point> = points
        .iter()
        .step_by(step)
        .chain(points.last())
        .copied()
        .collect();

    svg!("svg", {
        .class("sparkline")
        .attr("viewBox", &format!("0 0 {WIDTH} {HEIGHT}"))
        .attr("preserveAspectRatio", "none")
        // The figures it summarises are in the same row, so a reader gains
        // nothing from it being announced.
        .attr("aria-hidden", "true")
        .child(svg!("path", {
            .class("sparkline__line")
            .apply_if(!rising, |dom| dom.class("sparkline__line--down"))
            .attr("d", &path_of(&sampled, &bounds))
        }))
    })
}
