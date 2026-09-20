//! The app's icons, as inline SVG so they need no network and follow the
//! text colour. All on a 24×24 grid, drawn with strokes.

use dominator::{Dom, svg};

fn stroke(class: &str, paths: &[&str]) -> Dom {
    svg!("svg", {
        .class(class)
        .attr("viewBox", "0 0 24 24")
        .attr("fill", "none")
        .attr("stroke", "currentColor")
        .attr("stroke-width", "1.6")
        .attr("stroke-linecap", "round")
        .attr("stroke-linejoin", "round")
        .attr("aria-hidden", "true")
        .children(paths.iter().map(|d| svg!("path", { .attr("d", d) })))
    })
}

pub fn sun() -> Dom {
    const RAYS: [&str; 8] = [
        "M12 2v2",
        "M12 20v2",
        "M2 12h2",
        "M20 12h2",
        "M4.9 4.9l1.4 1.4",
        "M17.7 17.7l1.4 1.4",
        "M19.1 4.9l-1.4 1.4",
        "M6.3 17.7l-1.4 1.4",
    ];
    let rays = RAYS.iter().map(|d| svg!("path", { .attr("d", d) }));

    svg!("svg", {
        .class("btn__icon")
        .attr("viewBox", "0 0 24 24")
        .attr("fill", "none")
        .attr("stroke", "currentColor")
        .attr("stroke-width", "1.6")
        .attr("stroke-linecap", "round")
        .attr("aria-hidden", "true")
        .child(svg!("circle", {
            .attr("cx", "12")
            .attr("cy", "12")
            .attr("r", "4")
        }))
        .children(rays)
    })
}

pub fn moon() -> Dom {
    stroke(
        "btn__icon",
        &["M20 14.5A8.5 8.5 0 1 1 9.5 4a6.6 6.6 0 0 0 10.5 10.5Z"],
    )
}

pub fn check() -> Dom {
    stroke("notice__icon", &["M4 12.5l5 5L20 6.5"])
}

pub fn warning() -> Dom {
    stroke(
        "notice__icon",
        &["M12 4.5 2.8 20h18.4L12 4.5Z", "M12 10v4.5", "M12 17.2v.1"],
    )
}

pub fn error() -> Dom {
    stroke(
        "notice__icon",
        &[
            "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18Z",
            "M12 7.5v5.5",
            "M12 16.2v.1",
        ],
    )
}

pub fn info() -> Dom {
    stroke(
        "notice__icon",
        &[
            "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18Z",
            "M12 11v5.5",
            "M12 7.8v.1",
        ],
    )
}

pub fn copy() -> Dom {
    stroke("copy__icon", &["M9 9h10v10H9z", "M5 15V5h10"])
}

pub fn external() -> Dom {
    stroke("btn__icon", &["M14 5h5v5", "M19 5l-8 8", "M18 14v5H5V6h5"])
}

pub fn chevron_down() -> Dom {
    stroke("table__caret", &["M5 9l7 7 7-7"])
}

pub fn chevron_up() -> Dom {
    stroke("table__caret", &["M5 15l7-7 7 7"])
}

pub fn arrow_right() -> Dom {
    stroke("btn__icon", &["M4 12h15", "M13 6l6 6-6 6"])
}
