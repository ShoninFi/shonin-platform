//! Rating: one of exactly four words, always with its fixed icon:
//! Looks fine, Some concerns, Serious concerns, Couldn't tell.

use shared::Rating;
use ui::icons::Icon;

use super::badge::{Tone, badge};
use crate::model::{ReviewSummary, rating_word};
use crate::view::El;

pub fn rating_icon(rating: Rating) -> Icon {
    match rating {
        Rating::LooksFine => Icon::Check,
        Rating::SomeConcerns => Icon::Concern,
        Rating::SeriousConcerns => Icon::Serious,
        Rating::CouldntTell => Icon::Unknown,
    }
}

pub fn rating_tone(rating: Rating) -> Tone {
    match rating {
        Rating::LooksFine => Tone::Gain,
        Rating::SomeConcerns => Tone::Caution,
        Rating::SeriousConcerns => Tone::Loss,
        Rating::CouldntTell => Tone::Neutral,
    }
}

/// One rating, icon and word.
pub fn rating(rating: Rating) -> El {
    badge(
        rating_tone(rating),
        Some(rating_icon(rating)),
        rating_word(rating),
    )
}

/// A review summed up for a row: the most severe rating present and how many
/// areas have it, or "Looks fine".
pub fn review_chip(summary: &ReviewSummary) -> El {
    match summary.rating() {
        Some(worst) => badge(
            rating_tone(worst),
            Some(rating_icon(worst)),
            summary.words(),
        ),
        None => badge(Tone::Neutral, None, summary.words()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::View;

    #[test]
    fn a_rating_is_always_its_icon_and_its_exact_word() {
        let view: View = rating(Rating::CouldntTell).into();
        let html = view.html();
        assert!(html.starts_with("<span class=\"sh-chip sh-chip-neutral\"><svg"));
        assert!(
            html.ends_with("Couldn&#39;t tell</span>") || html.ends_with("Couldn’t tell</span>")
        );
    }
}
