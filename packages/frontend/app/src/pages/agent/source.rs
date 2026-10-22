//! `/agents/:id/source`: the code the Reviewer read, file by file, with the
//! hash the chain names it by and the archive to download. A sealed agent's
//! page says so plainly and offers nothing to open.
//!
//! Every file is in the page, so it reads with scripts off: the file list
//! links to each file, and the stylesheet shows the one the address names
//! (or the first). In the app the list picks the file without a navigation.

use std::rc::Rc;

use futures_signals::signal::{Mutable, SignalExt};
use shared::{SourceFile, Strategy};
use ui::icons::Icon;

use super::parts::{self, card};
use crate::atoms::button::{Button, Kind, Size};
use crate::atoms::icon;
use crate::format;
use crate::store::Agent;
use crate::view::{El, View, el};

pub fn body(agent: &Rc<Agent>) -> El {
    // An agent never changes its source, so the page is drawn once.
    let strategy = agent.strategy.lock_ref();
    el("div")
        .class("ag-srcpage")
        .child(if strategy.bundle.source.is_published() {
            published(&strategy)
        } else {
            sealed(&strategy)
        })
}

/// The id a file's section carries, which its link and the address name.
pub fn file_id(path: &str) -> String {
    let safe: String = path
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/') {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("file-{safe}")
}

/// A file's size as people say it: 860 bytes, 4.8 KB.
pub fn size_words(bytes: u64) -> String {
    if bytes < 1_000 {
        format!("{bytes} bytes")
    } else if bytes < 1_000_000 {
        format!("{} KB", format::number(bytes as f64 / 1_000.0, 1))
    } else {
        format!("{} MB", format::number(bytes as f64 / 1_000_000.0, 1))
    }
}

fn lines_words(lines: usize) -> String {
    if lines == 1 {
        "1 line".to_string()
    } else {
        format!("{} lines", format::number(lines as f64, 0))
    }
}

fn line_count(file: &SourceFile) -> usize {
    file.body.lines().count().max(1)
}

/// The file shown first: the project's README when it has one.
pub fn first_file(files: &[SourceFile]) -> usize {
    files
        .iter()
        .position(|file| file.path.eq_ignore_ascii_case("README.md"))
        .unwrap_or(0)
}

fn hash_row(strategy: &Strategy) -> El {
    el("div")
        .class("ag-src-hash")
        .child(el("p").class("sh-label").text("Source hash"))
        .child(parts::chip(&strategy.bundle.source_hash, "source hash"))
}

fn published(strategy: &Strategy) -> View {
    let files = &strategy.bundle.source_files;
    let archive = parts::store_url(&strategy.bundle.source_hash, "source.tar");
    let total: u64 = files.iter().map(|file| u64::from(file.bytes)).sum();
    let lines: usize = files.iter().map(line_count).sum();
    let summary = if files.is_empty() {
        "The files aren't in this page's record. The archive below is the whole source, as the Reviewer read it.".to_string()
    } else {
        format!(
            "{} files · {} · {}. Exactly what the Author submitted, and what the Reviewer read before it was built.",
            format::number(files.len() as f64, 0),
            lines_words(lines),
            size_words(total)
        )
    };
    let download: View = match archive {
        Some(archive) => Button::new(Kind::Secondary, "Download source.tar")
            .size(Size::Small)
            .icon_before(Icon::Code)
            .href(archive)
            .build()
            .attr("download", "source.tar")
            .into(),
        None => View::Empty,
    };
    let head = card("ag-src-title", "The source the Reviewer read", Some(Icon::Code))
        .class("ag-src-head")
        .child(el("p").class("ag-body").text(summary))
        .child(
            el("div")
                .class("ag-src-head__row")
                .child(hash_row(strategy))
                .child(
                    el("div")
                        .class("ag-src-head__actions")
                        .child(download)
                        .child(parts::more_in_app(
                            format!("/agents/{}/verify", strategy.slug),
                            "Check it against the chain",
                        )),
                ),
        )
        .child(el("p").class("ag-note").text(
            "The archive's SHA-256 is the source hash. The Verify tab shows how to check it, and how the chain ties it to the build that trades.",
        ));
    if files.is_empty() {
        return head.into();
    }
    View::Many(vec![head.into(), browser(files).into()])
}

/// The file list and the files.
fn browser(files: &[SourceFile]) -> El {
    let first = first_file(files);
    // The pre-render marks no file as picked: without scripts the address
    // picks it, which the list can't follow.
    let selected = Mutable::new(if cfg!(target_arch = "wasm32") {
        first
    } else {
        usize::MAX
    });
    let list = el("nav")
        .class("ag-src__nav")
        .attr("aria-label", "Files")
        .child(
            el("p")
                .class("ag-src__count")
                .text(format!("{} files", format::number(files.len() as f64, 0))),
        )
        .child(el("ul").class("ag-src__list").children({
            // Files under their folder, each folder named once.
            let mut items: Vec<View> = Vec::new();
            let mut last_folder: Option<String> = None;
            for (index, file) in files.iter().enumerate() {
                let (folder, name) = match file.path.rsplit_once('/') {
                    Some((folder, name)) => (Some(format!("{folder}/")), name.to_string()),
                    None => (None, file.path.clone()),
                };
                if folder.is_some() && folder != last_folder {
                    items.push(
                        el("li")
                            .class("ag-src__folder")
                            .attr("aria-hidden", "true")
                            .text(folder.clone().unwrap_or_default())
                            .into(),
                    );
                }
                last_folder = folder.clone();
                items.push(file_link(index, file, folder, name, &selected).into());
            }
            items
        }));
    let viewer = el("div")
        .class("ag-src__files")
        .attr("id", "ag-src-files")
        .children(
            files
                .iter()
                .enumerate()
                .map(|(index, file)| file_view(file, index, index == first, &selected)),
        );
    el("div").class("ag-src").child(list).child(viewer).hook({
        let selected = selected.clone();
        let ids: Vec<String> = files.iter().map(|file| file_id(&file.path)).collect();
        move |dom| {
            // In the app the list drives the choice; the stylesheet's
            // address-driven rule is for the page without scripts.
            let named = hash_now().and_then(|hash| ids.iter().position(|id| *id == hash));
            if let Some(index) = named {
                selected.set_neq(index);
            }
            let dom = dom.class("ag-src--live");
            if named.is_some() {
                // A link to one file opens on it.
                dom.after_inserted(|_| {
                    gloo_timers::callback::Timeout::new(0, reveal_viewer).forget();
                })
            } else {
                dom
            }
        }
    })
}

/// One file in the list: its name (its folder said to screen readers, and
/// drawn once above it) and its size.
fn file_link(
    index: usize,
    file: &SourceFile,
    folder: Option<String>,
    name: String,
    selected: &Mutable<usize>,
) -> El {
    let id = file_id(&file.path);
    let pick = selected.clone();
    let target = id.clone();
    el("li").child(
        el("a")
            .class("ag-src__link")
            .class_if(folder.is_some(), "ag-src__link--nested")
            .attr("href", format!("#{id}"))
            .attr_from("aria-current", selected, move |current| {
                (*current == index).then(|| "true".to_string())
            })
            .child(
                el("span")
                    .class("ag-src__path")
                    .child(View::from(
                        folder.map(|folder| el("span").class("sh-vh").text(folder).into()),
                    ))
                    .child(el("span").class("ag-src__name").text(name)),
            )
            .child(
                el("span")
                    .class("ag-src__size")
                    .text(size_words(u64::from(file.bytes))),
            )
            .hook(move |dom| {
                dom.event_with_options(
                    &dominator::EventOptions::preventable(),
                    move |event: dominator::events::Click| {
                        if event.ctrl_key() || event.shift_key() || event.alt_key() {
                            return;
                        }
                        event.prevent_default();
                        pick.set_neq(index);
                        parts::replace_hash(&target);
                        reveal_viewer();
                    },
                )
            }),
    )
}

/// The address's `#…`, decoded, without the `#`.
fn hash_now() -> Option<String> {
    let window = crate::web::window()?;
    let location = js_sys::Reflect::get(&window, &"location".into()).ok()?;
    let hash = js_sys::Reflect::get(&location, &"hash".into())
        .ok()?
        .as_string()?;
    crate::route::decode(hash.strip_prefix('#')?)
}

/// After picking a file on a narrow screen the file sits under the list:
/// bring its top into view when it isn't.
fn reveal_viewer() {
    let Some(viewer) =
        crate::web::document().and_then(|document| document.get_element_by_id("ag-src-files"))
    else {
        return;
    };
    let top = viewer.get_bounding_client_rect().top();
    let height = crate::web::window()
        .and_then(|window| window.inner_height().ok())
        .and_then(|height| height.as_f64())
        .unwrap_or(0.0);
    if top < 0.0 || top > height * 0.6 {
        viewer.scroll_into_view();
    }
}

fn file_view(file: &SourceFile, index: usize, first: bool, selected: &Mutable<usize>) -> El {
    let id = file_id(&file.path);
    let lines = line_count(file);
    let gutter: String = (1..=lines)
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let title = format!("{id}-title");
    el("section")
        .class("ag-src__file")
        .class_if(first, "ag-src__file--first")
        .class_signal(
            "ag-src__file--current",
            index == selected.get(),
            selected.signal().map(move |current| current == index),
        )
        .attr("id", id)
        .attr("aria-labelledby", title.clone())
        .child(
            el("div")
                .class("ag-src__bar")
                .child(
                    el("h3")
                        .class("ag-src__title")
                        .attr("id", title)
                        .text(file.path.clone()),
                )
                .child(el("p").class("ag-src__meta").text(format!(
                    "{} · {}",
                    lines_words(lines),
                    size_words(u64::from(file.bytes))
                ))),
        )
        .child(
            el("div")
                .class("ag-src__body")
                .child(
                    el("pre")
                        .class("ag-src__gutter")
                        .attr("aria-hidden", "true")
                        .text(gutter),
                )
                .child(
                    el("pre")
                        .class("ag-src__code")
                        .attr("tabindex", "0")
                        .attr(
                            "aria-label",
                            format!("{}, {}", file.path, lines_words(lines)),
                        )
                        .child(el("code").text(file.body.trim_end_matches('\n').to_string())),
                ),
        )
}

fn sealed(strategy: &Strategy) -> View {
    card("ag-src-title", "Source sealed: only the Reviewer read it", Some(Icon::Lock))
        .class("ag-src-head")
        .class("ag-src-head--sealed")
        .child(el("p").class("ag-body").text(
            "Its Author chose to keep this agent's code private, so there's nothing here to read or download. The attested Reviewer and builder read it. The chain records which source the Reviewer read, and that the build trading this vault came from that same source.",
        ))
        .child(hash_row(strategy))
        .child(
            el("div")
                .class("ag-src-head__actions")
                .child(parts::more_in_app(
                    format!("/agents/{}/review", strategy.slug),
                    "Read what the Reviewer found",
                ))
                .child(parts::more_in_app(
                    format!("/agents/{}/verify", strategy.slug),
                    "How sealed agents are checked",
                )),
        )
        .child(
            el("p")
                .class("ag-sealed-note")
                .child(icon(Icon::Lock, 16))
                .text("What you're relying on is the Reviewer's reading, not your own."),
        )
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_in_bytes_then_kilobytes() {
        assert_eq!(size_words(860), "860 bytes");
        assert_eq!(size_words(41_327), "41.3 KB");
        assert_eq!(size_words(2_400_000), "2.4 MB");
    }

    #[test]
    fn a_file_id_keeps_its_path_readable_and_drops_anything_unsafe() {
        assert_eq!(file_id("src/lib.rs"), "file-src/lib.rs");
        assert_eq!(file_id("a b\"c.rs"), "file-a-b-c.rs");
    }
}
