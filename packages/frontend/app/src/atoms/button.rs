//! Button: someone starts an action, and the label says what happens.
//! Primary, secondary, quiet (the ghost button), danger, and the two hero
//! buttons for the navy band; small (36), medium (44) and large (52); an
//! optional icon before or after; a loading state that keeps the label and
//! the width. A link styled as a button is an `<a>`, never a button in a link.

use futures_signals::signal::Mutable;
use ui::icons::Icon;

use super::icon::icon;
use crate::view::{El, View, el, svg};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// One per view: the brand button.
    Primary,
    Secondary,
    /// No border or ground until hovered (the design's "ghost").
    Quiet,
    /// Retire, freeze and kill confirmations, in a dialog that says what happens.
    Danger,
    /// On navy: white.
    Hero,
    /// On navy: outlined.
    Hero2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Small,
    Medium,
    Large,
}

impl Kind {
    fn class(self) -> &'static str {
        match self {
            Kind::Primary => "sh-btn-primary",
            Kind::Secondary => "sh-btn-secondary",
            Kind::Quiet => "sh-btn-ghost",
            Kind::Danger => "sh-btn-danger",
            Kind::Hero => "sh-btn-hero",
            Kind::Hero2 => "sh-btn-hero-2",
        }
    }
}

impl Size {
    fn class(self) -> Option<&'static str> {
        match self {
            Size::Small => Some("sh-btn-sm"),
            Size::Medium => None,
            Size::Large => Some("sh-btn-lg"),
        }
    }

    fn icon_size(self) -> u32 {
        match self {
            Size::Small => 16,
            Size::Medium | Size::Large => 18,
        }
    }
}

/// A button or a link that looks like one.
pub struct Button {
    kind: Kind,
    size: Size,
    label: View,
    before: Option<Icon>,
    after: Option<Icon>,
    href: Option<String>,
    block: bool,
    disabled: bool,
    busy: Option<Mutable<bool>>,
    aria_label: Option<String>,
}

impl Button {
    pub fn new(kind: Kind, label: impl Into<View>) -> Self {
        Self {
            kind,
            size: Size::Medium,
            label: label.into(),
            before: None,
            after: None,
            href: None,
            block: false,
            disabled: false,
            busy: None,
            aria_label: None,
        }
    }

    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    pub fn icon_before(mut self, icon: Icon) -> Self {
        self.before = Some(icon);
        self
    }

    pub fn icon_after(mut self, icon: Icon) -> Self {
        self.after = Some(icon);
        self
    }

    /// Make it a link: it goes somewhere rather than doing something.
    pub fn href(mut self, href: impl Into<String>) -> Self {
        self.href = Some(href.into());
        self
    }

    /// Full width, as in the Exit panel.
    pub fn block(mut self) -> Self {
        self.block = true;
        self
    }

    /// Disabled. Say why somewhere near it.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Loading while this is true: `aria-busy`, a tracing hex, clicks ignored.
    pub fn busy(mut self, busy: &Mutable<bool>) -> Self {
        self.busy = Some(busy.clone());
        self
    }

    pub fn aria_label(mut self, label: impl Into<String>) -> Self {
        self.aria_label = Some(label.into());
        self
    }

    pub fn build(self) -> El {
        let mut button = match &self.href {
            Some(href) => el("a").attr("href", href.clone()),
            None => el("button").attr("type", "button"),
        };
        button = button.class("sh-btn").class(self.kind.class());
        if let Some(size) = self.size.class() {
            button = button.class(size);
        }
        button = button
            .class_if(self.block, "sh-btn-block")
            .attr_if(self.disabled && self.href.is_none(), "disabled", "")
            .attr_if(
                self.disabled && self.href.is_some(),
                "aria-disabled",
                "true",
            )
            .attr_opt("aria-label", self.aria_label);
        if let Some(busy) = &self.busy {
            button = button
                .attr_from("aria-busy", busy, |busy| busy.then(|| "true".to_string()))
                .child_from(busy, |busy| busy.then(|| hex().into()));
        }
        if let Some(before) = self.before {
            button = button.child(icon(before, self.size.icon_size()));
        }
        button = button.child(self.label);
        if let Some(after) = self.after {
            button = button.child(icon(after, self.size.icon_size()));
        }
        button
    }
}

/// The 16px hex with a light tracing it: the loading mark.
pub fn hex() -> El {
    const HEX: &str = "M12 3l7.8 4.5v9L12 21l-7.8-4.5v-9z";
    svg("svg")
        .class("sh-btn__hex")
        .attr("width", "16")
        .attr("height", "16")
        .attr("viewBox", "0 0 24 24")
        .attr("fill", "none")
        .attr("aria-hidden", "true")
        .child(
            svg("path")
                .attr("d", HEX)
                .attr("stroke", "currentColor")
                .attr("stroke-width", "2")
                .attr("stroke-linejoin", "round")
                .attr("opacity", "0.3"),
        )
        .child(
            svg("path")
                .class("sh-btn__hex-trace")
                .attr("d", HEX)
                .attr("stroke", "currentColor")
                .attr("stroke-width", "2.5")
                .attr("stroke-linecap", "round")
                .attr("stroke-linejoin", "round")
                .attr("pathLength", "1"),
        )
}

/// An icon-only button: only for icons everyone understands (close, menu,
/// copy). `label` says the action and the object: "Close dialog".
pub fn icon_button(glyph: Icon, label: impl Into<String>) -> El {
    el("button")
        .attr("type", "button")
        .class("sh-iconbtn")
        .attr("aria-label", label.into())
        .child(icon(glyph, 20))
}

/// What a click on a button does: run `f`.
pub fn on_click(button: El, mut f: impl FnMut() + 'static) -> El {
    button.hook(move |dom| dom.event(move |_: dominator::events::Click| f()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_that_looks_like_a_button_is_a_link() {
        let view: View = Button::new(Kind::Secondary, "Browse agents")
            .href("/agents")
            .build()
            .into();
        let html = view.html();
        assert!(html.starts_with("<a class=\"sh-btn sh-btn-secondary\" href=\"/agents\">"));
        assert!(!html.contains("<button"));
    }

    #[test]
    fn a_disabled_button_uses_the_disabled_attribute() {
        let view: View = Button::new(Kind::Primary, "Deposit")
            .size(Size::Large)
            .disabled(true)
            .build()
            .into();
        let html = view.html();
        assert!(html.contains("class=\"sh-btn sh-btn-primary sh-btn-lg\""));
        assert!(html.contains(" disabled=\"\""));
    }

    #[test]
    fn a_busy_button_keeps_its_label_and_says_it_is_busy() {
        let busy = Mutable::new(true);
        let view: View = Button::new(Kind::Primary, "Deposit")
            .busy(&busy)
            .build()
            .into();
        let html = view.html();
        assert!(html.contains("aria-busy=\"true\""));
        assert!(html.contains("sh-btn__hex"));
        assert!(html.contains("Deposit</button>"));
    }
}
