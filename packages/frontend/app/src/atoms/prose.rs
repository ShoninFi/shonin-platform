//! Prose written by someone else (the Reviewer's summary and findings), with
//! its `code` spans drawn as inline code. The text is split on backticks and
//! every piece goes into the page as a text node, inside a `<code>` element
//! for the spans; nothing is ever parsed as HTML. A backtick with no partner
//! stays a literal backtick.

use crate::view::{View, el, text};

/// The pieces of `value`: plain text, and `code` spans as `<code>` elements.
pub fn prose(value: &str) -> Vec<View> {
    let parts: Vec<&str> = value.split('`').collect();
    // An even number of parts means an odd number of backticks: the last one
    // has no partner, so it and what follows it are plain text.
    let paired = if parts.len().is_multiple_of(2) {
        parts.len() - 1
    } else {
        parts.len()
    };
    let mut out = Vec::new();
    let mut plain = String::new();
    for (index, part) in parts[..paired].iter().enumerate() {
        if index % 2 == 0 {
            plain.push_str(part);
        } else if part.is_empty() {
            // "``" holds no code: keep the two backticks as written.
            plain.push_str("``");
        } else {
            if !plain.is_empty() {
                out.push(text(std::mem::take(&mut plain)));
            }
            out.push(el("code").class("sh-code").text(part.to_string()).into());
        }
    }
    if paired < parts.len() {
        plain.push('`');
        plain.push_str(parts[paired]);
    }
    if !plain.is_empty() {
        out.push(text(plain));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html(value: &str) -> String {
        View::Many(prose(value)).html()
    }

    #[test]
    fn backtick_spans_become_inline_code_and_the_rest_stays_text() {
        assert_eq!(
            html("`Mind.peak` starts at `Decimal::from(0)` in `decide.rs`."),
            "<code class=\"sh-code\">Mind.peak</code> starts at <code class=\"sh-code\">Decimal::from(0)</code> in <code class=\"sh-code\">decide.rs</code>."
        );
    }

    #[test]
    fn markup_inside_a_code_span_is_escaped_text_never_html() {
        let out = html("Calls `<script>alert(1)</script>` and <b>bold</b>.");
        assert_eq!(
            out,
            "Calls <code class=\"sh-code\">&lt;script&gt;alert(1)&lt;/script&gt;</code> and &lt;b&gt;bold&lt;/b&gt;."
        );
        assert!(!out.contains("<script"));
        assert!(!out.contains("<b>"));
    }

    #[test]
    fn an_unpaired_backtick_stays_a_literal_backtick() {
        assert_eq!(
            html("one `two` three ` four"),
            "one <code class=\"sh-code\">two</code> three ` four"
        );
        assert_eq!(html("plain text"), "plain text");
        assert_eq!(html("empty `` pair"), "empty `` pair");
    }
}
