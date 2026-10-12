//! Icon: one of the 24px line icons, `aria-hidden`, beside a word.

use ui::icons::Icon;

use crate::view::{El, svg};

/// An icon at `size` pixels (16, 18, 20 or 24; 14 inside chips).
pub fn icon(icon: Icon, size: u32) -> El {
    let mut el = svg("svg")
        .class("sh-icon")
        .attr("width", size.to_string())
        .attr("height", size.to_string())
        .attr("viewBox", "0 0 24 24")
        .attr("fill", "none")
        .attr("stroke", "currentColor")
        .attr("stroke-width", "1.75")
        .attr("stroke-linecap", "round")
        .attr("stroke-linejoin", "round")
        .attr("aria-hidden", "true")
        .attr("focusable", "false");
    for shape in icon.shapes() {
        let mut part = svg(shape.tag);
        for (name, value) in shape.attrs {
            part = part.attr(name, *value);
        }
        el = el.child(part);
    }
    el
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::View;

    #[test]
    fn an_icon_draws_as_the_design_system_draws_it() {
        let view: View = icon(Icon::Check, 20).into();
        assert_eq!(view.html(), Icon::Check.svg(20, ""));
    }
}
