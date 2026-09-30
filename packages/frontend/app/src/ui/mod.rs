//! The app's views: the shell it renders inside, and the pieces every page
//! is built from.

use dominator::events;

pub mod chart;
pub mod form;
pub mod icon;
pub mod overlay;
pub mod primitives;
pub mod shell;
pub mod table;
pub mod tabs;

/// Whether a click landed on the element carrying the listener itself, rather
/// than on something inside it.
///
/// Listeners are attached in the capture phase, so a click anywhere inside an
/// element reaches that element's handler first. Anything that acts on its own
/// background — a modal's backdrop — has to check this, or it swallows every
/// click meant for its contents.
pub fn clicked_self(event: &events::Click, class: &str) -> bool {
    event
        .dyn_target::<web_sys::Element>()
        .is_some_and(|target| target.class_list().contains(class))
}

/// Whether a click landed on a control — a button, a link, a field — inside
/// the element carrying the listener. A row that navigates when clicked has to
/// check this, so the controls it contains still work.
pub fn clicked_control(event: &events::Click) -> bool {
    let mut node = event.dyn_target::<web_sys::Element>();
    while let Some(element) = node {
        if matches!(
            element.tag_name().to_lowercase().as_str(),
            "button" | "a" | "input" | "select" | "textarea" | "label"
        ) {
            return true;
        }
        if element.class_list().contains("table") {
            break;
        }
        node = element.parent_element();
    }
    false
}
