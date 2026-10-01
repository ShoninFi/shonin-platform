//! Types shared across the Shonin platform: the web app, the services and the
//! executor all speak in these.
//!
//! Beside the domain model it holds the maths behind a record, the shape of
//! everything the API says, and what an author declares in a manifest. All of
//! it is plain data and plain arithmetic, so the browser and the services
//! compute the same figures from the same inputs.

pub mod api;
pub mod archive;
pub mod base64;
pub mod catalogue;
pub mod manifest;
pub mod model;
pub mod record;

pub use api::{
    Addresses, ApiError, ChainInfo, Config, DevMode, HealthPost, HeldClaim, Holding, Portfolio,
    ReviewAnswer, ReviewRequest, Snapshot, Terms, Totals, Upload, UploadAccepted,
};
pub use catalogue::Catalogue;
pub use manifest::{Allowances, Canonical, ChainNeed, Manifest, Prices};

pub use model::activity::{EventKind, Kill, Side, Trade, VaultEvent};
pub use model::attestation::{Approval, Attestation, BlessedKey, Image, KeyRole};
pub use model::bundle::{Bundle, ModelFile, Source, SourceFile};
pub use model::common::{Chain, Performance, Point};
pub use model::health::{
    Entry as HealthEntry, Health, Level as HealthLevel, Signal as HealthSignal,
};
pub use model::registry::{Author, Failure, FeedEvent, FeedKind, Job, JobState};
pub use model::review::{Area, Finding, Rating, Report, Review, Severity};
pub use model::strategy::{Record, Standing, Strategy};
pub use model::vault::{Claim, ClaimState, Fees, InTransit, Tank, Vault, VaultState};
pub use model::venue::{Custody, ExitClass, ExitPath, Leg, Valuation, VenueStatus};
