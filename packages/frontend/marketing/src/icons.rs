//! The design system's 24px line icons, as the SVG bodies its reference
//! components draw, plus the handoff's new ones (arrow, chevrons, close,
//! menu) and the two the marketing boards add (key, swap).
//!
//! Every icon is drawn at 1.75 stroke on a 24px grid, round caps and joins,
//! in `currentColor`, and is always `aria-hidden`: a word beside it, or the
//! control's own label, carries the meaning.

/// The body of the icon called `name`.
pub fn body(name: &str) -> Option<&'static str> {
    Some(match name {
        "check" => r#"<path d="M5 12.5l4.5 4.5L19 7.5"/>"#,
        "concern" => r#"<path d="M12 4.5l8.5 15h-17z"/><path d="M12 10v4"/><path d="M12 17h.01"/>"#,
        "serious" => {
            r#"<path d="M8.6 3.5h6.8l5.1 5.1v6.8l-5.1 5.1H8.6l-5.1-5.1V8.6z"/><path d="M9.2 9.2l5.6 5.6M14.8 9.2l-5.6 5.6"/>"#
        }
        "unknown" => {
            r#"<circle cx="12" cy="12" r="8.5"/><path d="M9.6 9.6a2.5 2.5 0 1 1 3.4 2.3c-.6.3-1 .8-1 1.5v.6"/><path d="M12 16.8h.01"/>"#
        }
        "info" => {
            r#"<circle cx="12" cy="12" r="8.5"/><path d="M12 11v5.5"/><path d="M12 7.8h.01"/>"#
        }
        "seal" => {
            r#"<rect x="3.5" y="3.5" width="17" height="17" rx="4.5"/><path d="M12 3.5v3.2M12 17.3v3.2"/><circle cx="12" cy="12" r="3.2"/>"#
        }
        "lock" => {
            r#"<rect x="5" y="10.5" width="14" height="10" rx="2.5"/><path d="M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5"/><path d="M12 14.5v2"/>"#
        }
        "vault" => {
            r#"<rect x="3.5" y="4.5" width="17" height="15" rx="3"/><circle cx="12" cy="12" r="3.5"/><path d="M12 8.5V7M12 17v-1.5M8.5 12H7M17 12h-1.5"/><path d="M6.5 19.5v1.5M17.5 19.5v1.5"/>"#
        }
        "tank" => {
            r#"<rect x="5.5" y="3.5" width="13" height="17" rx="3"/><path d="M5.5 13h13"/><path d="M9 16.5h6"/><path d="M18.5 8H20v6"/>"#
        }
        "up" => r#"<path d="M7 17L17 7"/><path d="M9 7h8v8"/>"#,
        "copy" => {
            r#"<rect x="8.5" y="8.5" width="11" height="11" rx="2.5"/><path d="M15.5 8.5V6a1.5 1.5 0 0 0-1.5-1.5H6A1.5 1.5 0 0 0 4.5 6v8A1.5 1.5 0 0 0 6 15.5h2.5"/>"#
        }
        "external" => {
            r#"<path d="M13.5 4.5h6v6"/><path d="M19.5 4.5l-8 8"/><path d="M17 13.5v4.5a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 5 18V8.5A1.5 1.5 0 0 1 6.5 7H11"/>"#
        }
        "bridge" => {
            r#"<path d="M3.5 17.5h17"/><path d="M5 17.5c1.5-5 4-7.5 7-7.5s5.5 2.5 7 7.5"/><path d="M8.5 11.9v5.6M12 10v7.5M15.5 11.9v5.6"/>"#
        }
        "clock" => r#"<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5V12l3 2"/>"#,
        "machine" => {
            r#"<rect x="6.5" y="6.5" width="11" height="11" rx="2"/><rect x="9.75" y="9.75" width="4.5" height="4.5" rx="1"/><path d="M9.5 3.5v3M14.5 3.5v3M9.5 17.5v3M14.5 17.5v3M3.5 9.5h3M3.5 14.5h3M17.5 9.5h3M17.5 14.5h3"/>"#
        }
        "review" => {
            r#"<path d="M6.5 3.5h7l4 4v4"/><path d="M13.5 3.5v4h4"/><path d="M6.5 3.5a1.5 1.5 0 0 0-1.5 1.5v14a1.5 1.5 0 0 0 1.5 1.5H10"/><circle cx="15.5" cy="15.5" r="3.5"/><path d="M18 18l2.5 2.5"/>"#
        }
        "code" => {
            r#"<path d="M8.5 7.5L4 12l4.5 4.5"/><path d="M15.5 7.5L20 12l-4.5 4.5"/><path d="M13.2 5.5l-2.4 13"/>"#
        }
        "exit" => {
            r#"<path d="M14 4.5h4a1.5 1.5 0 0 1 1.5 1.5v12a1.5 1.5 0 0 1-1.5 1.5h-4"/><path d="M10 8l-4 4 4 4"/><path d="M6 12h9"/>"#
        }
        "arrow" => r#"<path d="M5 12h14"/><path d="M13 6l6 6-6 6"/>"#,
        "chev-down" => r#"<path d="M6 9l6 6 6-6"/>"#,
        "chev-right" => r#"<path d="M9 6l6 6-6 6"/>"#,
        "chev-left" => r#"<path d="M15 6l-6 6 6 6"/>"#,
        "close" => r#"<path d="M6 6l12 12M18 6L6 18"/>"#,
        "menu" => r#"<path d="M4 7h16M4 12h16M4 17h16"/>"#,
        "key" => r#"<circle cx="8" cy="12" r="3.5"/><path d="M11.5 12H20M17 12v3M20 12v2.5"/>"#,
        "swap" => r#"<path d="M8 5v14M5 16l3 3 3-3"/><path d="M16 19V5M13 8l3-3 3 3"/>"#,
        _ => return None,
    })
}

/// The icon as inline SVG, `size` pixels square.
pub fn svg(name: &str, size: u32, stroke: &str) -> Option<String> {
    let body = body(name)?;
    Some(format!(
        r#"<svg class="ic" width="{size}" height="{size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="{stroke}" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">{body}</svg>"#
    ))
}
