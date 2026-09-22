//! Types shared across the Shonin platform: the web app, the services and the
//! executor all speak in these.
//!
//! Beside the domain model it holds the maths behind a record, the shape of
//! everything the API says, and what an author declares in a manifest. All of
//! it is plain data and plain arithmetic, so the browser and the services
//! compute the same figures from the same inputs.

pub mod api;
pub mod manifest;
pub mod model;
pub mod record;

pub use api::{
    Addresses, ApiError, Benchmark, ChainInfo, Config, DevMode, HealthPost, HeldClaim, Holding,
    Portfolio, Snapshot, Submission, SubmissionAccepted, SubmissionSource, Totals,
};
pub use manifest::{Allowances as DeclaredAllowances, Manifest, Prices};

pub use model::activity::{EventKind, Kill, Side, Trade, VaultEvent};
pub use model::attestation::{Approval, Attestation, BlessedKey, Image, KeyRole};
pub use model::bundle::{Allowances, Bundle, LogLevel, LogLine, ModelFile, Source, SourceFile};
pub use model::common::{Audit, AuditStatus, Chain, Performance, Point};
pub use model::health::{
    Entry as HealthEntry, Health, Level as HealthLevel, Signal as HealthSignal,
};
pub use model::registry::{Author, FeedEvent, FeedKind, Job, JobState};
pub use model::review::{Finding, FindingCategory, Review, ScoreBand, Severity};
pub use model::strategy::{Record, Standing, Strategy};
pub use model::vault::{Claim, ClaimState, Fees, InTransit, Tank, Vault, VaultState};
pub use model::venue::{Custody, ExitClass, ExitPath, Leg, Valuation, VenueStatus};
