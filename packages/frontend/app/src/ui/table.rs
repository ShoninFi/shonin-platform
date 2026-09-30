//! The app's tables. Dense, with figures in tabular monospace and every
//! numeric column aligned right, so a column of numbers can be read down.
//!
//! On a narrow screen a table can collapse into one record per row, with each
//! cell labelled by its column — the same data, read down instead of across.

use dominator::{Dom, events, html, link, routing};
use futures_signals::signal::Mutable;

use crate::ui::icon;

/// One column: what it is called, whether it holds figures, and the key the
/// table sorts by when its header is used.
pub struct Column {
    pub label: String,
    pub numeric: bool,
    pub sort_key: Option<&'static str>,
    /// On a narrow screen, whether this cell takes the full width.
    pub wide: bool,
}

impl Column {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            numeric: false,
            sort_key: None,
            wide: false,
        }
    }

    pub fn num(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            numeric: true,
            sort_key: None,
            wide: false,
        }
    }

    pub fn sortable(mut self, key: &'static str) -> Self {
        self.sort_key = Some(key);
        self
    }

    pub fn wide(mut self) -> Self {
        self.wide = true;
        self
    }
}

/// One row, and where clicking it goes.
pub struct Row {
    pub href: Option<String>,
    pub cells: Vec<Dom>,
}

impl Row {
    pub fn new(cells: Vec<Dom>) -> Self {
        Self { href: None, cells }
    }

    pub fn linked(href: String, cells: Vec<Dom>) -> Self {
        Self {
            href: Some(href),
            cells,
        }
    }
}

/// Which column a table is sorted by, and which way.
pub struct Sort {
    /// Owned rather than borrowed, so a `select` elsewhere on the page can
    /// drive exactly this state instead of mirroring it.
    pub key: Mutable<String>,
    pub descending: Mutable<bool>,
}

impl Sort {
    pub fn new(key: &str) -> Self {
        Self {
            key: Mutable::new(key.to_string()),
            descending: Mutable::new(true),
        }
    }

    /// Use a column: the same one flips direction, a new one starts
    /// descending, because the interesting end of a figure is usually the top.
    pub fn use_column(&self, key: &str) {
        if self.key.get_cloned() == key {
            self.descending.set(!self.descending.get());
        } else {
            self.key.set(key.to_string());
            self.descending.set(true);
        }
    }

    /// Order two values by the current direction.
    pub fn order(&self, a: f64, b: f64) -> std::cmp::Ordering {
        let ordering = a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal);
        if self.descending.get() {
            ordering.reverse()
        } else {
            ordering
        }
    }
}

/// A table with no sorting.
pub fn table(columns: Vec<Column>, rows: Vec<Row>) -> Dom {
    render(columns, rows, None)
}

/// A table whose headers sort it. The caller owns the sort state and the
/// sorted rows; using a header only changes that state.
pub fn sortable_table(columns: Vec<Column>, rows: Vec<Row>, sort: &'static Sort) -> Dom {
    render(columns, rows, Some(sort))
}

fn render(columns: Vec<Column>, rows: Vec<Row>, sort: Option<&'static Sort>) -> Dom {
    let labels: Vec<String> = columns.iter().map(|column| column.label.clone()).collect();
    let numeric: Vec<bool> = columns.iter().map(|column| column.numeric).collect();
    let wide: Vec<bool> = columns.iter().map(|column| column.wide).collect();

    html!("div", {
        .class("table-wrap")
        .child(html!("table", {
            .class(["table", "table--responsive"])
            .apply_if(rows.iter().any(|row| row.href.is_some()), |dom| {
                dom.class("table--rows-link")
            })
            .child(html!("thead", {
                .child(html!("tr", {
                    .children(columns.into_iter().map(|column| header(column, sort)))
                }))
            }))
            .child(html!("tbody", {
                .children(rows.into_iter().map(|row| {
                    let href = row.href.clone();
                    let target = row.href.clone();
                    html!("tr", {
                        .apply(|dom| match href {
                            // Also a capture-phase listener, so a click on a
                            // control inside the row would otherwise navigate
                            // instead of doing what the control does.
                            Some(href) => dom.event(move |event: events::Click| {
                                if !crate::ui::clicked_control(&event) {
                                    routing::go_to_url(&href);
                                }
                            }),
                            None => dom,
                        })
                        .children(row.cells.into_iter().enumerate().map(|(index, cell)| {
                            // The whole row is clickable, but a click is not
                            // the only way people move: the first cell carries
                            // the real link, so the row is reachable by
                            // keyboard and can be opened in a new window.
                            let cell = match (index, target.clone()) {
                                (0, Some(href)) => link!(href, {
                                    .class("table__link")
                                    .child(cell)
                                }),
                                _ => cell,
                            };
                            html!("td", {
                                .apply_if(numeric.get(index).copied().unwrap_or(false), |dom| {
                                    dom.class("num")
                                })
                                .apply_if(wide.get(index).copied().unwrap_or(false), |dom| {
                                    dom.attr("data-span", "full")
                                })
                                .attr("data-label", labels.get(index).map_or("", String::as_str))
                                .child(cell)
                            })
                        }))
                    })
                }))
            }))
        }))
    })
}

fn header(column: Column, sort: Option<&'static Sort>) -> Dom {
    let Some((sort, key)) = sort.zip(column.sort_key) else {
        return html!("th", {
            .attr("scope", "col")
            .apply_if(column.numeric, |dom| dom.class("num"))
            .text(&column.label)
        });
    };

    let active = sort.key.get_cloned() == key;
    let descending = sort.descending.get();

    html!("th", {
        .attr("scope", "col")
        .apply_if(column.numeric, |dom| dom.class("num"))
        .apply_if(active, |dom| {
            dom.attr("aria-sort", if descending { "descending" } else { "ascending" })
        })
        .child(html!("button", {
            .class("table__sort")
            .attr("type", "button")
            .apply_if(active, |dom| {
                dom.attr("aria-sort", if descending { "descending" } else { "ascending" })
            })
            .text(&column.label)
            .apply_if(active, |dom| {
                dom.child(if descending {
                    icon::chevron_down()
                } else {
                    icon::chevron_up()
                })
            })
            .event(move |_: events::Click| sort.use_column(key))
        }))
    })
}
