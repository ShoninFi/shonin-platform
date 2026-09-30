//! The pages, as HTML strings.
//!
//! The shell (header, footer) follows the design; the copy is placeholder,
//! taken from the public docs' own wording until the real copy lands.

/// Where the links go.
pub struct Links {
    pub app: String,
    pub docs: String,
}

fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn el(tag: &str, attrs: &str, inner: &str) -> String {
    if attrs.is_empty() {
        format!("<{tag}>{inner}</{tag}>")
    } else {
        format!("<{tag} {attrs}>{inner}</{tag}>")
    }
}

fn link(href: &str, text: &str) -> String {
    format!("<a href=\"{}\">{}</a>", esc(href), esc(text))
}

fn link_class(href: &str, class: &str, text: &str) -> String {
    format!(
        "<a class=\"{}\" href=\"{}\">{}</a>",
        esc(class),
        esc(href),
        esc(text)
    )
}

fn para(text: &str) -> String {
    el("p", "", &esc(text))
}

fn list(items: &[&str]) -> String {
    el(
        "ul",
        "",
        &items
            .iter()
            .map(|item| el("li", "", &esc(item)))
            .collect::<String>(),
    )
}

/// A section of a page: an anchor, an eyebrow, a heading and its body.
fn section(id: &str, eyebrow: &str, heading: &str, body: &str) -> String {
    let id_attr = if id.is_empty() {
        "class=\"section\"".to_string()
    } else {
        format!("class=\"section\" id=\"{}\"", esc(id))
    };
    el(
        "section",
        &id_attr,
        &format!(
            "{}{}{}",
            el("p", "class=\"eyebrow\"", &esc(eyebrow)),
            el("h2", "", &esc(heading)),
            body
        ),
    )
}

fn page(title: &str, description: &str, main: &str, links: &Links) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n\
         <meta charset=\"utf-8\" />\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n\
         <title>{title}</title>\n\
         <meta name=\"description\" content=\"{description}\" />\n\
         <!-- A scaffold: not for indexing until it takes shonin.fi. -->\n\
         <meta name=\"robots\" content=\"noindex\" />\n\
         <meta name=\"theme-color\" content=\"#f6f7fb\" />\n\
         <link rel=\"stylesheet\" href=\"/styles/tokens.css\" />\n\
         <link rel=\"stylesheet\" href=\"/styles/marketing.css\" />\n\
         </head>\n<body>\n{header}\n<main>\n{main}\n</main>\n{footer}\n</body>\n</html>\n",
        title = esc(title),
        description = esc(description),
        header = header(links),
        footer = footer(links),
    )
}

fn header(links: &Links) -> String {
    let nav = [
        link("/#how", "How it works"),
        link("/#traders", "For Traders"),
        link("/authors/", "For Authors"),
        link("/#fees", "Fees"),
        format!(
            "<a href=\"{}/\" rel=\"noopener\">Docs ↗</a>",
            esc(&links.docs)
        ),
        link_class(&links.app, "button button--primary", "Open the app →"),
    ]
    .join("");
    el(
        "header",
        "class=\"site-header\"",
        &format!(
            "{}{}",
            el("a", "class=\"wordmark\" href=\"/\"", "Shonin"),
            el("nav", "aria-label=\"Main\"", &nav)
        ),
    )
}

fn footer(links: &Links) -> String {
    let app = |path: &str, text: &str| link(&format!("{}{path}", links.app), text);
    let columns = [
        (
            "The app",
            vec![
                app("/agents", "Agents"),
                app("/portfolio", "Portfolio"),
                app("/publish", "Publish an agent"),
                app("/status", "Status"),
            ],
        ),
        (
            "Learn",
            vec![
                link("/#how", "How it works"),
                link("/authors/", "For Authors"),
                link("/#fees", "Fees"),
                format!("<a href=\"{}/\">Docs</a>", esc(&links.docs)),
            ],
        ),
    ];
    let columns: String = columns
        .into_iter()
        .map(|(title, items)| {
            el(
                "div",
                "",
                &format!(
                    "{}{}",
                    el("h3", "", &esc(title)),
                    el(
                        "ul",
                        "",
                        &items
                            .into_iter()
                            .map(|item| el("li", "", &item))
                            .collect::<String>()
                    )
                ),
            )
        })
        .collect();
    el(
        "footer",
        "class=\"site-footer\"",
        &format!(
            "{}{}",
            el(
                "div",
                "class=\"footer-brand\"",
                &format!(
                    "{}{}",
                    el("p", "class=\"wordmark\"", "Shonin"),
                    para("Trading agents you can check.")
                )
            ),
            el("div", "class=\"footer-columns\"", &columns)
        ),
    )
}

pub fn home(links: &Links) -> String {
    let hero = el(
        "section",
        "class=\"hero\" id=\"top\"",
        &format!(
            "{}{}{}{}",
            el("h1", "", "Trading agents you can check."),
            para(
                "Smart contracts are public and auditable. Trading agents should be too. On \
                 Shonin, what an agent does, what risks it takes and whether anyone could walk \
                 off with the money are clear and verifiable, backed by secure enclaves and the \
                 chain."
            ),
            el(
                "p",
                "class=\"actions\"",
                &format!(
                    "{}{}",
                    link_class(
                        &format!("{}/agents", links.app),
                        "button button--primary",
                        "Browse agents"
                    ),
                    link_class("#how", "button", "How it works")
                )
            ),
            list(&[
                "What does it do?",
                "What risks does its strategy take?",
                "Can somebody swap the code out from under my feet?",
                "Could anyone walk off with my money?",
                "Can I get out whenever I want?",
            ])
        ),
    );

    let how = section(
        "how",
        "Under the hood",
        "Attested agents. Explicit constraints.",
        &format!(
            "{}{}",
            para(
                "An agent runs on an attested machine: a computer that can prove exactly what \
                 code it is running. The vault's trading key is made inside it and never leaves, \
                 and it can place and cancel orders and nothing else."
            ),
            el(
                "div",
                "class=\"cards\"",
                &[
                    (
                        "Reviewed",
                        "A model reads the source and rates it before anything is built.",
                    ),
                    ("Built once", "The build is signed, and it is what runs."),
                    (
                        "Trade-only keys",
                        "The key can trade. It can't withdraw, and neither can anyone else.",
                    ),
                    (
                        "Smart contracts",
                        "Money only moves between a vault and its own stations.",
                    ),
                ]
                .iter()
                .map(|(title, text)| {
                    el(
                        "article",
                        "class=\"card\"",
                        &format!("{}{}", el("h3", "", &esc(title)), para(text)),
                    )
                })
                .collect::<String>()
            )
        ),
    );

    let traders = section(
        "traders",
        "For Traders",
        "Back a strategy you can actually check.",
        &format!(
            "{}{}",
            para(
                "Put money into an agent's vault and get shares back. Your shares rise and fall \
                 with the vault, and you can cash them in whenever you like."
            ),
            list(&[
                "The review: six areas rated, and a summary of what the code does.",
                "The source, when its Author shares it, and proof it's the code that's trading.",
                "The share price over time, read straight from the vault's contract.",
            ])
        ),
    );

    let authors = section(
        "authors",
        "For Authors",
        "Turn a good strategy into income.",
        &format!(
            "{}{}",
            para(
                "Write a trading agent and publish it. If Depositors back it and it makes money, \
                 you earn a share of the profit, without ever holding their money. You don't \
                 need anyone's permission to publish."
            ),
            el(
                "p",
                "",
                &link_class("/authors/", "button", "Writing an agent →")
            )
        ),
    );

    let publishing = section(
        "publishing",
        "Publishing",
        "Submit it for $1. Activate it once it's built.",
        &para(
            "Publishing takes two steps in the web app: submit your agent, which covers its \
             review and its build, and once it has been reviewed and built, activate it.",
        ),
    );

    let fees = section(
        "fees",
        "Fees",
        "Paid on new profit, and nothing when there's none.",
        &el(
            "table",
            "",
            &format!(
                "{}{}",
                el(
                    "thead",
                    "",
                    "<tr><th>Fee</th><th>How much</th><th>Who gets it</th></tr>"
                ),
                el(
                    "tbody",
                    "",
                    "<tr><td>Profit fee</td><td>15% of new profit</td><td>10% to the Author, 5% to Shonin</td></tr>\
                     <tr><td>Holding fee</td><td>1% a year</td><td>Shonin</td></tr>\
                     <tr><td>Deposit and withdrawal fee</td><td>0.1% each way</td><td>The vault: it raises every share's value</td></tr>"
                )
            ),
        ),
    );

    let questions = section(
        "questions",
        "Questions",
        "Can I get out whenever I want?",
        &para(
            "Yes. You can cash in your shares whenever you like; nobody can withdraw a \
             Depositor's money but the Depositor.",
        ),
    );

    let eligibility = section(
        "eligibility",
        "Who can deposit",
        "Not everyone: each vault follows its venues' rules.",
        &para(
            "No vault takes deposits from US persons or sanctioned persons, and each adds \
             whoever its venues exclude. When you deposit, your wallet signs a statement that \
             you aren't one of them, and the vault's contract won't take a deposit without it. \
             Getting out is never restricted.",
        ),
    );

    let risks = section(
        "risks",
        "Risks",
        "Trade-only doesn't mean risk-free.",
        &para(
            "A key that can trade can also lose money. What helps you judge that risk is the \
             Reviewer's report, the agent's public source and its track record, and what \
             protects you is that you can leave at any time.",
        ),
    );

    let closing = el(
        "section",
        "class=\"closing\"",
        &format!(
            "{}{}",
            el("h2", "", "See what's running."),
            el(
                "p",
                "class=\"actions\"",
                &link_class(&links.app, "button button--primary", "Open the app →")
            )
        ),
    );

    page(
        "Shonin: trading agents you can check",
        "Verifiable trading agents with pooled capital: reviewed, attested and held to their limits by smart contracts.",
        &[
            hero,
            how,
            traders,
            authors,
            publishing,
            fees,
            questions,
            eligibility,
            risks,
            closing,
        ]
        .join("\n"),
        links,
    )
}

pub fn authors(links: &Links) -> String {
    let body = [
        el(
            "section",
            "class=\"hero hero--page\"",
            &format!(
                "{}{}",
                el(
                    "h1",
                    "",
                    "Write it once. Earn from it for as long as it works."
                ),
                para(
                    "An ordinary Rust project: one program that watches the markets, decides \
                     what to do, and hands its orders to the attested machine it runs on. You \
                     never hold a key, and your code can only ever trade your vault's own \
                     accounts."
                )
            ),
        ),
        section(
            "",
            "What it costs",
            "A flat $1 to publish.",
            &para(
                "It covers both the review of your code and the build. After that, your agent's \
                 tank pays for the computing and gas it uses, at published prices and never past \
                 the limits you set.",
            ),
        ),
        section(
            "",
            "What you earn",
            "10% of your vault's new profit.",
            &para(
                "Paid weekly, as shares in the vault. New profit means gains above the highest \
                 price its shares have reached, so you're never paid twice for the same gain.",
            ),
        ),
        el(
            "section",
            "class=\"closing\"",
            &format!(
                "{}{}",
                el("h2", "", "Ready to write an agent?"),
                el(
                    "p",
                    "class=\"actions\"",
                    &format!(
                        "{}{}",
                        link_class(
                            &format!("{}/writing/getting-started/", links.docs),
                            "button button--primary",
                            "Getting started ↗"
                        ),
                        link_class(
                            &format!("{}/publish", links.app),
                            "button",
                            "Publish an agent →"
                        )
                    )
                )
            ),
        ),
    ]
    .join("\n");
    page(
        "For Authors · Shonin",
        "Publish a trading agent on Shonin and earn a share of the profit it makes, without ever holding anyone's money.",
        &body,
        links,
    )
}

pub fn not_found(links: &Links) -> String {
    page(
        "Not found · Shonin",
        "There is no page here.",
        &el(
            "section",
            "class=\"hero hero--page\"",
            &format!(
                "{}{}",
                el("h1", "", "There's no page here."),
                el("p", "class=\"actions\"", &link_class("/", "button", "Home"))
            ),
        ),
        links,
    )
}

pub const STYLE: &str = r#"/* The marketing site. Values come from tokens.css; nothing here restates one. */
*, *::before, *::after { box-sizing: border-box; }
html { background: var(--color-paper); color: var(--color-ink); }
body {
  margin: 0;
  font-family: var(--text-body-family);
  font-size: var(--text-body-size);
  line-height: var(--text-body-line);
}
a { color: var(--color-link); }
main, .site-header, .site-footer { max-width: 1120px; margin: 0 auto; padding: 0 var(--space-4); }

.site-header {
  display: flex; align-items: center; justify-content: space-between; gap: var(--space-4);
  flex-wrap: wrap; padding-top: var(--space-4); padding-bottom: var(--space-4);
}
.site-header nav { display: flex; align-items: center; gap: var(--space-4); flex-wrap: wrap; }
.site-header nav a { color: var(--color-ink); text-decoration: none; }
.wordmark {
  font-family: var(--font-display); font-weight: 800; font-size: 22px;
  color: var(--color-ink); text-decoration: none; margin: 0;
}

.button {
  display: inline-block; padding: 10px 16px; border-radius: var(--radius-m);
  border: var(--border-control) solid var(--color-line-control);
  color: var(--color-ink); text-decoration: none; font-weight: 600;
}
.site-header nav a.button--primary, .button--primary {
  background: var(--color-brand); border-color: var(--color-brand); color: var(--color-on-brand);
}
.button--primary:hover { background: var(--color-brand-hover); }
.actions { display: flex; gap: var(--space-3); flex-wrap: wrap; }

h1, h2, h3 { font-family: var(--font-display); color: var(--color-ink); }
.hero { padding: 72px 0 48px; }
.hero h1 {
  font-size: var(--text-display-xl-size); line-height: var(--text-display-xl-line);
  font-weight: var(--text-display-xl-weight); letter-spacing: var(--text-display-xl-tracking);
  max-width: 14em; margin: 0 0 var(--space-4);
}
.hero--page h1 {
  font-size: var(--text-display-l-size); line-height: var(--text-display-l-line);
  font-weight: var(--text-display-l-weight); letter-spacing: var(--text-display-l-tracking);
}
.hero p { max-width: 40em; color: var(--color-ink-muted); }
.hero ul { color: var(--color-ink-muted); }

.section { padding: 48px 0; border-top: var(--border-hairline) solid var(--color-line); }
.section h2 {
  font-size: var(--text-display-m-size); line-height: var(--text-display-m-line);
  font-weight: var(--text-display-m-weight); letter-spacing: var(--text-display-m-tracking);
  margin: 0 0 var(--space-4);
}
.section p { max-width: 42em; }
.eyebrow { color: var(--color-brand); font-weight: 600; margin: 0 0 var(--space-2); }
.cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: var(--space-4); }
.card {
  background: var(--color-paper-raised); border: var(--border-hairline) solid var(--color-line);
  border-radius: var(--radius-l); padding: var(--space-5);
}
.card h3 { margin: 0 0 var(--space-2); }
table { border-collapse: collapse; width: 100%; max-width: 760px; }
th, td { text-align: left; padding: var(--space-3); border-bottom: var(--border-hairline) solid var(--color-line); }

.closing { padding: 64px 0; border-top: var(--border-hairline) solid var(--color-line); }

.site-footer {
  display: flex; justify-content: space-between; gap: var(--space-6); flex-wrap: wrap;
  padding-top: 40px; padding-bottom: 56px; border-top: var(--border-hairline) solid var(--color-line);
  color: var(--color-ink-muted);
}
.footer-columns { display: flex; gap: 48px; flex-wrap: wrap; }
.site-footer h3 { font-size: 14px; margin: 0 0 var(--space-2); }
.site-footer ul { list-style: none; padding: 0; margin: 0; }
.site-footer li { margin: 4px 0; }

@media (max-width: 640px) {
  .hero h1 { font-size: var(--text-display-l-size); line-height: var(--text-display-l-line); }
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn links() -> Links {
        Links {
            app: "https://app.example".to_string(),
            docs: "https://docs.example".to_string(),
        }
    }

    #[test]
    fn the_home_page_has_the_designs_sections_and_links() {
        let html = home(&links());
        for id in [
            "top",
            "how",
            "traders",
            "authors",
            "publishing",
            "fees",
            "questions",
            "eligibility",
            "risks",
        ] {
            assert!(html.contains(&format!("id=\"{id}\"")), "missing #{id}");
        }
        for path in ["/agents", "/portfolio", "/publish", "/status"] {
            assert!(html.contains(&format!("href=\"https://app.example{path}\"")));
        }
        assert!(html.contains("href=\"https://docs.example/\""));
        assert!(html.contains("Open the app →"));
    }

    #[test]
    fn text_is_escaped() {
        assert_eq!(
            esc("<a href=\"x\">&'"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }
}
