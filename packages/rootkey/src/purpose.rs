//! What a derived key is for.

use std::fmt;
use std::str::FromStr;

use alloy::primitives::{Address, B256};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Error;

/// The platform secret the reviewer reaches its model with.
pub const REVIEWER_MODEL: &str = "reviewer-model";
/// A secret of no value, sealed to prove sealing and release end to end.
pub const DUMMY: &str = "dummy";

/// The longest name a venue or a platform secret may have.
pub const MAX_NAME: usize = 64;

/// What a key is derived for, and so who may be given it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Purpose {
    /// The root itself, passed only between holders. It has no key pair.
    Root,
    /// The builder's key, which sealed source is sealed to.
    Builder,
    /// One bundle's key, by its hash: its runner's secrets are sealed to it.
    Bundle(B256),
    /// A venue's credentials for one vault, by the vault's chain and address.
    Venue {
        vault_chain: u64,
        vault: Address,
        venue: String,
    },
    /// One of the platform's own secrets, by name ([`REVIEWER_MODEL`],
    /// [`DUMMY`]).
    Platform(String),
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_.".contains(&b))
}

impl Purpose {
    /// A venue's purpose, refusing a name that isn't lowercase letters,
    /// digits, `-`, `_` and `.` (1 to 64 of them).
    pub fn venue(vault_chain: u64, vault: Address, venue: &str) -> Result<Purpose, Error> {
        if !valid_name(venue) {
            return Err(Error::Malformed("venue name"));
        }
        Ok(Purpose::Venue {
            vault_chain,
            vault,
            venue: venue.into(),
        })
    }

    /// A platform secret's purpose, under the same rule for its name.
    pub fn platform(name: &str) -> Result<Purpose, Error> {
        if !valid_name(name) {
            return Err(Error::Malformed("platform secret name"));
        }
        Ok(Purpose::Platform(name.into()))
    }

    /// The canonical bytes every label, digest and nonce uses: a tag byte,
    /// then the fields, each name preceded by its length in one byte.
    ///
    /// | purpose | bytes |
    /// | --- | --- |
    /// | root | `00` |
    /// | builder | `01` |
    /// | bundle | `02` ‖ bundle hash (32) |
    /// | venue | `03` ‖ vault chain (8, big-endian) ‖ vault (20) ‖ len (1) ‖ venue |
    /// | platform | `04` ‖ len (1) ‖ name |
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Purpose::Root => vec![0],
            Purpose::Builder => vec![1],
            Purpose::Bundle(hash) => {
                let mut out = vec![2];
                out.extend_from_slice(hash.as_slice());
                out
            }
            Purpose::Venue {
                vault_chain,
                vault,
                venue,
            } => {
                let mut out = vec![3];
                out.extend_from_slice(&vault_chain.to_be_bytes());
                out.extend_from_slice(vault.as_slice());
                out.push(venue.len() as u8);
                out.extend_from_slice(venue.as_bytes());
                out
            }
            Purpose::Platform(name) => {
                let mut out = vec![4, name.len() as u8];
                out.extend_from_slice(name.as_bytes());
                out
            }
        }
    }

    /// Whether the purpose's names are well formed: a purpose built by hand
    /// rather than through [`Purpose::venue`], [`Purpose::platform`] or
    /// parsing may not be.
    pub fn is_well_formed(&self) -> bool {
        match self {
            Purpose::Venue { venue, .. } => valid_name(venue),
            Purpose::Platform(name) => valid_name(name),
            _ => true,
        }
    }
}

/// The string form: `root`, `builder`, `bundle:0x<64 hex>`,
/// `venue:<vault chain id>:0x<vault>:<venue>`, `platform:<name>`.
impl fmt::Display for Purpose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Purpose::Root => write!(f, "root"),
            Purpose::Builder => write!(f, "builder"),
            Purpose::Bundle(hash) => write!(f, "bundle:{hash:#x}"),
            Purpose::Venue {
                vault_chain,
                vault,
                venue,
            } => write!(f, "venue:{vault_chain}:{vault:#x}:{venue}"),
            Purpose::Platform(name) => write!(f, "platform:{name}"),
        }
    }
}

impl FromStr for Purpose {
    type Err = Error;

    fn from_str(text: &str) -> Result<Purpose, Error> {
        let malformed = Error::Malformed("purpose");
        match text {
            "root" => return Ok(Purpose::Root),
            "builder" => return Ok(Purpose::Builder),
            _ => {}
        }
        let (kind, rest) = text.split_once(':').ok_or(Error::Malformed("purpose"))?;
        match (kind, rest) {
            ("bundle", hash) => {
                let hex = hash.strip_prefix("0x").ok_or(Error::Malformed("purpose"))?;
                if hex.len() != 64 {
                    return Err(malformed);
                }
                hash.parse().map(Purpose::Bundle).map_err(|_| malformed)
            }
            ("venue", rest) => {
                let mut parts = rest.splitn(3, ':');
                let (Some(chain), Some(vault), Some(venue)) =
                    (parts.next(), parts.next(), parts.next())
                else {
                    return Err(malformed);
                };
                let vault_chain = chain.parse().map_err(|_| Error::Malformed("purpose"))?;
                let hex = vault
                    .strip_prefix("0x")
                    .ok_or(Error::Malformed("purpose"))?;
                if hex.len() != 40 {
                    return Err(malformed);
                }
                let vault = vault.parse().map_err(|_| Error::Malformed("purpose"))?;
                Purpose::venue(vault_chain, vault, venue)
            }
            ("platform", name) => Purpose::platform(name),
            _ => Err(malformed),
        }
    }
}

impl Serialize for Purpose {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Purpose {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Purpose, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}
