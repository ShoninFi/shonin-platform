//! The footer: the navy band at the end of every page, on both sites. Brand,
//! three columns of links, then the legal and social row, then the risk line,
//! always. Discord and X stay hidden until their addresses are known.

use ui::icons::Icon;

use crate::atoms::icon;
use crate::atoms::logo::{Logo, image};
use crate::route::{DOCS, GITHUB, PRIVACY, Route, SITE, TERMS};
use crate::view::{El, View, el};
use crate::web::app_link;

fn docs(path: &str, label: &'static str) -> El {
    el("a").attr("href", format!("{DOCS}{path}")).text(label)
}

fn column(label: &'static str, links: Vec<El>) -> El {
    el("nav")
        .class("sh-footer__col")
        .attr("aria-label", label)
        .child(el("p").class("sh-label").text(label))
        .children(links)
}

pub fn footer(route: &Route) -> View {
    let on_status = *route == Route::Status;
    el("footer")
        .class("sh-hero")
        .class("sh-footer")
        .child(
            el("div")
                .class("sh-container")
                .class("sh-footer__inner")
                .child(
                    el("div")
                        .class("sh-footer__cols")
                        .child(
                            el("div")
                                .class("sh-footer__brand")
                                .child(
                                    el("a")
                                        .class("sh-logo")
                                        .class("sh-logo--tagline")
                                        .attr("href", SITE)
                                        .attr("aria-label", "Shonin home")
                                        .child(image(Logo::TaglineReversed)),
                                )
                                .child(el("p").text("Attested agents. Explicit constraints.")),
                        )
                        .child(column(
                            "Product",
                            vec![
                                app_link("/agents").text("Agents"),
                                app_link("/portfolio").text("Portfolio"),
                                app_link("/publish").text("Publish an agent"),
                            ],
                        ))
                        .child(column(
                            "Learn",
                            vec![
                                docs("basics/trading-agents/", "How Shonin works"),
                                docs("verify/", "Verification"),
                                docs("fees/", "Fees"),
                                docs("tank/", "The tank"),
                                docs("reference/glossary/", "Glossary"),
                            ],
                        ))
                        .child(column(
                            "Shonin",
                            vec![
                                app_link("/status")
                                    .attr_if(on_status, "aria-current", "page")
                                    .text("Status"),
                                el("a")
                                    .attr("href", DOCS)
                                    .text("Docs")
                                    .child(icon(Icon::External, 14)),
                            ],
                        )),
                )
                .child(bottom()),
        )
        .into()
}

/// The legal and social row and the risk line: the end of every footer,
/// and of the 404.
pub fn bottom() -> El {
    el("div")
        .class("sh-footer__bottom")
        .child(
            el("div")
                .class("sh-footer__legal-row")
                .child(
                    el("nav")
                        .class("sh-footer__legal")
                        .attr("aria-label", "Legal")
                        .child(el("span").text("© 2026 Shonin"))
                        .child(el("a").attr("href", TERMS).text("Terms of Use"))
                        .child(el("a").attr("href", PRIVACY).text("Privacy Policy")),
                )
                .child(
                    el("ul")
                        .class("sh-footer__social")
                        .attr("aria-label", "Shonin elsewhere")
                        .child(
                            el("li").child(
                                el("a")
                                    .attr("href", GITHUB)
                                    .attr("aria-label", "Shonin on GitHub")
                                    .attr("data-logo", "github")
                                    .text("GH"),
                            ),
                        ),
                ),
        )
        // The longer line names the build checklist, which has no public
        // page yet; until it does, every footer carries the phone's line.
        .child(
            el("p")
                .class("sh-footer__risk")
                .text("Agents can lose money. A review isn’t a prediction, and a track record isn’t a promise."),
        )
}
