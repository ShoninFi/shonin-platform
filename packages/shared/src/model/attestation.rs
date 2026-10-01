//! What the "which code is trading this vault?" panel shows: the image, the
//! blessed keys, and the venue approvals that follow from them.

use serde::{Deserialize, Serialize};
use time::{Date, PrimitiveDateTime};

use super::venue::Custody;

/// A measured image on the allowlist.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Image {
    pub digest: String,
    /// When the timelock on it ran out and it became blessable.
    pub allowlisted: Date,
    /// `executor`, `unwinder`, `builder` or `reviewer`.
    pub kind: String,
}

/// One executor key, blessed for this vault and this bundle.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlessedKey {
    pub address: String,
    /// Where the enclave runs.
    pub zone: String,
    pub blessed_until: PrimitiveDateTime,
    /// The transaction that carried the proof.
    pub proof_tx: String,
    pub role: KeyRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyRole {
    Primary,
    Standby,
    Unwinder,
}

impl KeyRole {
    pub fn label(self) -> &'static str {
        match self {
            Self::Primary => "primary executor",
            Self::Standby => "standby executor",
            Self::Unwinder => "unwinder",
        }
    }
}

/// What a venue's own records show the vault has approved.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Approval {
    pub venue: String,
    /// "agent 0xE7…, 0xA2… (trade-only)", "role STRATEGY on Safe 0x88…".
    pub detail: String,
    pub custody: Custody,
}

/// Everything a verifier needs to answer "which code is trading this vault".
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attestation {
    pub image: Image,
    pub keys: Vec<BlessedKey>,
    pub approvals: Vec<Approval>,
    /// The builder image that signed this bundle, and the hash it signed over.
    pub builder: Image,
    /// The trust root the token was checked against.
    pub trust_root: String,
    /// How long a blessing lasts before the executor has to attest again.
    pub blessing_ttl_hours: u32,
}
