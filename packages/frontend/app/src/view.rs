//! One description of a piece of page, two ways to draw it.
//!
//! Every atom, component and page is written once, as a [`View`]: elements,
//! classes, attributes, text and children, some of them bound to the store's
//! signals. The app turns a view into a live Dominator tree
//! ([`View::into_dom`]); the build-time pre-render writes the same view as
//! static HTML ([`View::html`]), taking each bound value as it is right now.
//! So the page a reader sees with JavaScript off is the page the app draws,
//! node for node, and the two can't drift apart.
//!
//! Event handlers and anything else that only makes sense in a browser are
//! [`El::hook`]s: the HTML writer skips them.

use std::borrow::Cow;
use std::fmt::Write;

use dominator::{Dom, DomBuilder};
use futures_signals::signal::{Mutable, Signal, SignalExt};
use futures_signals::signal_vec::{MutableVec, SignalVecExt};
use web_sys::Element;

type Hook = Box<dyn FnOnce(DomBuilder<Element>) -> DomBuilder<Element>>;

/// A piece of page.
pub enum View {
    El(El),
    Text(Cow<'static, str>),
    /// Several views side by side, with no element of their own.
    Many(Vec<View>),
    Empty,
}

/// A child that is either fixed or follows a signal.
enum Child {
    Fixed(View),
    /// Drawn now as `html`; in the app, `apply` binds it to its signal at
    /// this position among its siblings.
    Bound {
        html: Vec<View>,
        apply: Hook,
    },
}

/// An element under construction.
pub struct El {
    tag: &'static str,
    svg: bool,
    classes: Vec<Cow<'static, str>>,
    attrs: Vec<(&'static str, Cow<'static, str>)>,
    /// Bound classes and attributes as they are now, for the HTML writer only;
    /// the app binds them through `hooks`.
    now_classes: Vec<Cow<'static, str>>,
    now_attrs: Vec<(&'static str, String)>,
    children: Vec<Child>,
    hooks: Vec<Hook>,
}

/// An HTML element.
pub fn el(tag: &'static str) -> El {
    El::new(tag, false)
}

/// An SVG element.
pub fn svg(tag: &'static str) -> El {
    El::new(tag, true)
}

/// Text, escaped wherever it is written.
pub fn text(value: impl Into<Cow<'static, str>>) -> View {
    View::Text(value.into())
}

impl El {
    fn new(tag: &'static str, svg: bool) -> Self {
        Self {
            tag,
            svg,
            classes: Vec::new(),
            attrs: Vec::new(),
            now_classes: Vec::new(),
            now_attrs: Vec::new(),
            children: Vec::new(),
            hooks: Vec::new(),
        }
    }

    /// Add a class, or several separated by spaces.
    pub fn class(mut self, name: impl Into<Cow<'static, str>>) -> Self {
        let name = name.into();
        if name.contains(char::is_whitespace) {
            self.classes.extend(
                name.split_whitespace()
                    .map(|part| Cow::Owned(part.to_string())),
            );
        } else {
            self.classes.push(name);
        }
        self
    }

    pub fn class_if(self, test: bool, name: impl Into<Cow<'static, str>>) -> Self {
        if test { self.class(name) } else { self }
    }

    pub fn attr(mut self, name: &'static str, value: impl Into<Cow<'static, str>>) -> Self {
        self.attrs.push((name, value.into()));
        self
    }

    pub fn attr_opt(self, name: &'static str, value: Option<impl Into<Cow<'static, str>>>) -> Self {
        match value {
            Some(value) => self.attr(name, value),
            None => self,
        }
    }

    pub fn attr_if(self, test: bool, name: &'static str, value: &'static str) -> Self {
        if test { self.attr(name, value) } else { self }
    }

    pub fn child(mut self, child: impl Into<View>) -> Self {
        self.children.push(Child::Fixed(child.into()));
        self
    }

    pub fn children<V: Into<View>>(mut self, children: impl IntoIterator<Item = V>) -> Self {
        self.children
            .extend(children.into_iter().map(|child| Child::Fixed(child.into())));
        self
    }

    pub fn text(self, value: impl Into<Cow<'static, str>>) -> Self {
        self.child(text(value))
    }

    /// Text that follows a signal: `now` is what it says at this moment.
    pub fn text_signal(
        mut self,
        now: impl Into<Cow<'static, str>>,
        signal: impl Signal<Item = String> + 'static,
    ) -> Self {
        self.children.push(Child::Bound {
            html: vec![text(now)],
            apply: Box::new(move |dom| dom.text_signal(signal)),
        });
        self
    }

    /// Text drawn from a `Mutable`, redrawn only when its value changes.
    pub fn text_from<T: 'static>(
        self,
        value: &Mutable<T>,
        f: impl Fn(&T) -> String + 'static,
    ) -> Self {
        let now = f(&value.lock_ref());
        self.text_signal(now, value.signal_ref(f))
    }

    /// A child that follows a signal: `now` is what it is at this moment.
    pub fn child_signal(
        mut self,
        now: Option<View>,
        signal: impl Signal<Item = Option<View>> + 'static,
    ) -> Self {
        self.children.push(Child::Bound {
            html: now.into_iter().collect(),
            apply: Box::new(move |dom| {
                dom.child_signal(signal.map(|view| view.map(View::into_dom)))
            }),
        });
        self
    }

    /// A child drawn from a `Mutable`.
    pub fn child_from<T: 'static>(
        self,
        value: &Mutable<T>,
        f: impl Fn(&T) -> Option<View> + 'static,
    ) -> Self {
        let now = f(&value.lock_ref());
        self.child_signal(now, value.signal_ref(f))
    }

    /// Children that follow a list: each item drawn once, and only the items
    /// that change are redrawn.
    pub fn children_from<T: Clone + 'static>(
        mut self,
        list: &MutableVec<T>,
        f: impl Fn(&T) -> View + 'static,
    ) -> Self {
        let html = list.lock_ref().iter().map(&f).collect();
        let signal = list
            .signal_vec_cloned()
            .map(move |item| f(&item).into_dom());
        self.children.push(Child::Bound {
            html,
            apply: Box::new(move |dom| dom.children_signal_vec(signal)),
        });
        self
    }

    /// An attribute that follows a signal; `None` removes it.
    pub fn attr_signal(
        mut self,
        name: &'static str,
        now: Option<String>,
        signal: impl Signal<Item = Option<String>> + 'static,
    ) -> Self {
        if let Some(value) = now {
            self.now_attrs.push((name, value));
        }
        self.hooks
            .push(Box::new(move |dom| dom.attr_signal(name, signal)));
        self
    }

    pub fn attr_from<T: 'static>(
        self,
        name: &'static str,
        value: &Mutable<T>,
        f: impl Fn(&T) -> Option<String> + 'static,
    ) -> Self {
        let now = f(&value.lock_ref());
        self.attr_signal(name, now, value.signal_ref(f))
    }

    /// A class that is on while a signal says so.
    pub fn class_signal(
        mut self,
        name: &'static str,
        now: bool,
        signal: impl Signal<Item = bool> + 'static,
    ) -> Self {
        if now {
            self.now_classes.push(name.into());
        }
        self.hooks
            .push(Box::new(move |dom| dom.class_signal(name, signal)));
        self
    }

    pub fn class_from<T: 'static>(
        self,
        name: &'static str,
        value: &Mutable<T>,
        f: impl Fn(&T) -> bool + 'static,
    ) -> Self {
        let now = f(&value.lock_ref());
        self.class_signal(name, now, value.signal_ref(f))
    }

    /// Something only a browser does: an event handler, focus, a timer.
    pub fn hook(
        mut self,
        f: impl FnOnce(DomBuilder<Element>) -> DomBuilder<Element> + 'static,
    ) -> Self {
        self.hooks.push(Box::new(f));
        self
    }

    pub fn into_dom(self) -> Dom {
        let mut dom = if self.svg {
            DomBuilder::<Element>::new_svg(self.tag)
        } else {
            DomBuilder::<Element>::new_html(self.tag)
        };
        for class in &self.classes {
            dom = dom.class(class.as_ref());
        }
        for (name, value) in &self.attrs {
            dom = dom.attr(*name, value);
        }
        for child in self.children {
            dom = match child {
                Child::Fixed(view) => match view {
                    View::Empty => dom,
                    View::Many(views) => dom.children(views.into_iter().map(View::into_dom)),
                    view => dom.child(view.into_dom()),
                },
                Child::Bound { apply, .. } => apply(dom),
            };
        }
        for hook in self.hooks {
            dom = hook(dom);
        }
        dom.into_dom()
    }

    fn write_html(&self, out: &mut String) {
        let _ = write!(out, "<{}", self.tag);
        let classes: Vec<&str> = self
            .classes
            .iter()
            .chain(&self.now_classes)
            .map(|class| class.as_ref())
            .collect();
        if !classes.is_empty() {
            let _ = write!(out, " class=\"{}\"", escape(&classes.join(" ")));
        }
        for (name, value) in &self.attrs {
            write_attr(out, name, value);
        }
        for (name, value) in &self.now_attrs {
            write_attr(out, name, value);
        }
        if is_void(self.tag) {
            out.push('>');
            return;
        }
        if self.svg && self.children.is_empty() {
            out.push_str("/>");
            return;
        }
        out.push('>');
        for child in &self.children {
            match child {
                Child::Fixed(view) => view.write_html(out),
                Child::Bound { html, .. } => html.iter().for_each(|view| view.write_html(out)),
            }
        }
        let _ = write!(out, "</{}>", self.tag);
    }
}

fn write_attr(out: &mut String, name: &str, value: &str) {
    let _ = write!(out, " {name}=\"{}\"", escape(value));
}

fn is_void(tag: &str) -> bool {
    matches!(
        tag,
        "area" | "br" | "col" | "hr" | "img" | "input" | "link" | "meta" | "source" | "wbr"
    )
}

impl View {
    pub fn into_dom(self) -> Dom {
        match self {
            View::El(el) => el.into_dom(),
            View::Text(value) => dominator::text(&value),
            View::Many(views) => {
                // A group with no element of its own still needs one node in
                // the browser; `display: contents` keeps it out of layout.
                let mut dom = DomBuilder::<Element>::new_html("span").class("sh-contents");
                for view in views {
                    dom = dom.child(view.into_dom());
                }
                dom.into_dom()
            }
            View::Empty => dominator::Dom::empty(),
        }
    }

    /// The view as static HTML, with every bound value as it is now.
    pub fn html(&self) -> String {
        let mut out = String::new();
        self.write_html(&mut out);
        out
    }

    fn write_html(&self, out: &mut String) {
        match self {
            View::El(el) => el.write_html(out),
            View::Text(value) => out.push_str(&escape(value)),
            View::Many(views) => views.iter().for_each(|view| view.write_html(out)),
            View::Empty => {}
        }
    }
}

impl From<El> for View {
    fn from(el: El) -> Self {
        View::El(el)
    }
}

impl From<Option<View>> for View {
    fn from(view: Option<View>) -> Self {
        view.unwrap_or(View::Empty)
    }
}

impl From<Vec<View>> for View {
    fn from(views: Vec<View>) -> Self {
        View::Many(views)
    }
}

impl From<&'static str> for View {
    fn from(value: &'static str) -> Self {
        text(value)
    }
}

impl From<String> for View {
    fn from(value: String) -> Self {
        text(value)
    }
}

/// Text made safe for HTML, in text and in a quoted attribute alike.
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_attributes_are_escaped_so_a_name_can_never_become_markup() {
        let view: View = el("a")
            .attr("href", "/x?a=1&b=\"2\"")
            .text("<script>alert(1)</script> & 'friends'")
            .into();
        assert_eq!(
            view.html(),
            "<a href=\"/x?a=1&amp;b=&quot;2&quot;\">&lt;script&gt;alert(1)&lt;/script&gt; &amp; &#39;friends&#39;</a>"
        );
    }

    #[test]
    fn a_bound_value_is_written_as_it_is_now() {
        let value = Mutable::new(3);
        let view: View = el("p")
            .class("x")
            .class_from("on", &value, |n| *n > 2)
            .attr_from("data-n", &value, |n| Some(n.to_string()))
            .text("n is ")
            .text_from(&value, |n| n.to_string())
            .into();
        assert_eq!(view.html(), "<p class=\"x on\" data-n=\"3\">n is 3</p>");
        value.set(1);
        let view: View = el("p")
            .class_from("on", &value, |n| *n > 2)
            .text_from(&value, |n| n.to_string())
            .into();
        assert_eq!(view.html(), "<p>1</p>");
    }

    #[test]
    fn a_bound_list_is_written_in_its_current_order() {
        let list = MutableVec::new_with_values(vec!["b", "a"]);
        let view: View = el("ul")
            .children_from(&list, |item| el("li").text(*item).into())
            .into();
        assert_eq!(view.html(), "<ul><li>b</li><li>a</li></ul>");
    }

    #[test]
    fn void_and_empty_svg_elements_close_themselves() {
        let view: View = el("div")
            .child(el("img").attr("src", "/a.svg").attr("alt", "Shonin"))
            .child(svg("svg").child(svg("path").attr("d", "M0 0")))
            .into();
        assert_eq!(
            view.html(),
            "<div><img src=\"/a.svg\" alt=\"Shonin\"><svg><path d=\"M0 0\"/></svg></div>"
        );
    }
}
