//! The authors publishing on the platform.

use shared::Author;

use super::days_ago;

pub fn all() -> Vec<Author> {
    vec![Author {
        handle: "shonin".to_string(),
        joined: days_ago(260),
        blurb: "Reference strategies, published through the same queue, the same review and \
                the same vault contracts as everyone else's."
            .to_string(),
        links: vec![("Verification guide".to_string(), "/verify".to_string())],
    }]
}
