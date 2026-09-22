//! Types shared across the Shonin platform: the frontend, the backend and the
//! executor all speak in these.

pub mod model;

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
