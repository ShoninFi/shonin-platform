//! A snapshot the tests render against.
//!
//! It is not made up: it is what a running platform actually answered,
//! trimmed so the file stays readable. So a test that renders it is
//! rendering the shapes the indexer really produces, and a change that
//! breaks one will break the other.
//!
//! Regenerate it with the platform running:
//!
//! ```text
//! curl -s http://localhost:19081/api/snapshot > packages/frontend/app/tests/snapshot.json
//! ```

use shared::Snapshot;

/// The captured snapshot, as JSON.
pub const SNAPSHOT: &str = include_str!("../tests/snapshot.json");

/// Read it.
pub fn snapshot() -> Snapshot {
    serde_json::from_str(SNAPSHOT).expect("the captured snapshot still parses")
}

/// Make it what the app is showing, so a test renders real pages.
pub fn install() {
    if crate::data::current().is_none() {
        crate::data::set(snapshot());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::Standing;

    /// The site has to render every state a vault can be in. The fixture is
    /// what proves it does, so it has to contain them.
    #[test]
    fn the_fixture_covers_what_the_site_has_to_show() {
        let snapshot = snapshot();
        let standings: Vec<Standing> = snapshot
            .strategies
            .iter()
            .map(|strategy| strategy.standing)
            .collect();

        for wanted in [Standing::New, Standing::Killed] {
            assert!(
                standings.contains(&wanted),
                "nothing in the fixture is {wanted:?}"
            );
        }
        assert!(
            snapshot
                .strategies
                .iter()
                .any(|strategy| strategy.review.is_some()),
            "no agent has a review"
        );
        assert!(
            snapshot
                .jobs
                .iter()
                .any(|job| job.state == shared::JobState::Ready),
            "no agent is ready and waiting to be activated"
        );
        assert!(
            snapshot
                .strategies
                .iter()
                .any(|strategy| !strategy.bundle.source.is_published()),
            "no sealed-source agent"
        );
        assert!(
            snapshot
                .jobs
                .iter()
                .any(|job| matches!(job.state, shared::JobState::Failed { .. })),
            "no failed job in the queue"
        );
        assert!(!snapshot.authors.is_empty(), "nobody published anything");
        assert!(!snapshot.feed.is_empty(), "nothing has happened");
        assert!(
            snapshot
                .strategies
                .iter()
                .any(|strategy| strategy.record.is_some()),
            "no agent has a record"
        );
    }
}
