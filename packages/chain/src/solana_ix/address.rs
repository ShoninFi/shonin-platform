//! Every address the platform's instructions name that is derived rather
//! than chosen: the programs' own accounts (seeds as each program writes
//! them) and dollar accounts (associated token accounts).

use super::{Pubkey, ids};

fn derive(seeds: &[&[u8]], program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(seeds, program).0
}

// Dollar accounts ----------------------------------------------------------------

/// `owner`'s associated token account for `mint` under `token_program`.
pub fn associated_token(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    derive(
        &[owner.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ids::ASSOCIATED_TOKEN,
    )
}

/// `owner`'s dollar account: its associated token account for the dollar
/// `mint`, under the classic token program, the one a vault's dollar is
/// under. The vault pays a receiver here, and checks it by this address.
pub fn dollars(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    associated_token(owner, mint, &ids::TOKEN)
}

// The vault program ---------------------------------------------------------------

/// The factory's settings.
pub fn factory() -> Pubkey {
    factory_of(&ids::VAULT)
}

/// The factory's settings under another deployment of the vault program.
pub fn factory_of(program: &Pubkey) -> Pubkey {
    derive(&[b"factory"], program)
}

/// Where strategy `strategy_id`'s vault is, or will be.
pub fn vault(strategy_id: u64) -> Pubkey {
    vault_of(&ids::VAULT, strategy_id)
}

/// The same under another deployment of the vault program.
pub fn vault_of(program: &Pubkey, strategy_id: u64) -> Pubkey {
    derive(&[b"vault", &strategy_id.to_le_bytes()], program)
}

/// `owner`'s holding in `vault`: their shares, units and statement.
pub fn holder(vault: &Pubkey, owner: &Pubkey) -> Pubkey {
    derive(&[b"holder", vault.as_ref(), owner.as_ref()], &ids::VAULT)
}

/// The request `maker` made at `nonce`. The maker is the key that signed for
/// it, or for one the program made, the claim or the vault it was made for.
pub fn request(vault: &Pubkey, maker: &Pubkey, nonce: u64) -> Pubkey {
    derive(
        &[
            b"request",
            vault.as_ref(),
            maker.as_ref(),
            &nonce.to_le_bytes(),
        ],
        &ids::VAULT,
    )
}

/// Claim `id`: its id is the id of the withdrawal it was booked from.
pub fn claim(vault: &Pubkey, id: u64) -> Pubkey {
    derive(&[b"claim", vault.as_ref(), &id.to_le_bytes()], &ids::VAULT)
}

/// The vault's restricted list.
pub fn restricted(vault: &Pubkey) -> Pubkey {
    derive(&[b"restricted", vault.as_ref()], &ids::VAULT)
}

/// The vault program's event authority, which signs its calls of itself.
pub fn event_authority() -> Pubkey {
    derive(&[b"__event_authority"], &ids::VAULT)
}

// The station program ---------------------------------------------------------------

/// Where `vault`'s station is, or will be.
pub fn station(vault: &Pubkey) -> Pubkey {
    derive(&[b"station", vault.as_ref()], &ids::STATION)
}

pub fn station_config() -> Pubkey {
    derive(&[b"config"], &ids::STATION)
}

/// The station's signing authority at `venue`.
pub fn venue_authority(station: &Pubkey, venue: &Pubkey) -> Pubkey {
    derive(&[b"venue", station.as_ref(), venue.as_ref()], &ids::STATION)
}

// Publishing ----------------------------------------------------------------------------

pub fn publishing_config() -> Pubkey {
    derive(&[b"config"], &ids::PUBLISHING)
}

/// The submission paying for strategy `id`.
pub fn submission(id: u64) -> Pubkey {
    derive(&[b"submission", &id.to_le_bytes()], &ids::PUBLISHING)
}

/// Strategy `id`'s record.
pub fn strategy(id: u64) -> Pubkey {
    derive(&[b"strategy", &id.to_le_bytes()], &ids::PUBLISHING)
}

/// Strategy `id`'s review.
pub fn review(id: u64) -> Pubkey {
    derive(&[b"review", &id.to_le_bytes()], &ids::PUBLISHING)
}

// The registry ---------------------------------------------------------------------------

pub fn registry_config() -> Pubkey {
    derive(&[b"config"], &ids::REGISTRY)
}

/// A key's record.
pub fn key(signer: &Pubkey) -> Pubkey {
    derive(&[b"key", signer.as_ref()], &ids::REGISTRY)
}

/// An image's record.
pub fn image(digest: &[u8; 32]) -> Pubkey {
    derive(&[b"image", digest], &ids::REGISTRY)
}

/// A project's record.
pub fn project(id: &[u8; 32]) -> Pubkey {
    derive(&[b"project", id], &ids::REGISTRY)
}

/// `signer`'s blessing for `vault` on the cluster `cluster` stands for (see
/// [`crate::solana_ix::read::cluster_value`]), running the bundle
/// `strategy_hash`.
pub fn blessing(
    signer: &Pubkey,
    cluster: &[u8; 32],
    vault: &Pubkey,
    strategy_hash: &[u8; 32],
) -> Pubkey {
    derive(
        &[
            b"blessing",
            signer.as_ref(),
            cluster,
            vault.as_ref(),
            strategy_hash,
        ],
        &ids::REGISTRY,
    )
}

/// The registry's approval slot for the driver named `name`.
pub fn driver_approval(name: &[u8; 32]) -> Pubkey {
    derive(&[b"approval", b"driver", name], &ids::REGISTRY)
}

/// The revocation record of `implementation` of the driver named `name`.
pub fn driver_revoked(name: &[u8; 32], implementation: &Pubkey) -> Pubkey {
    derive(
        &[b"revoked", b"driver", name, implementation.as_ref()],
        &ids::REGISTRY,
    )
}

// Prices and programs -----------------------------------------------------------------------

/// The price list.
pub fn prices() -> Pubkey {
    derive(&[b"prices"], &ids::PRICES)
}

/// A program's program-data account under the upgradeable loader.
pub fn program_data(program: &Pubkey) -> Pubkey {
    derive(&[program.as_ref()], &ids::UPGRADEABLE_LOADER)
}

// The stand-in venue ------------------------------------------------------------------------

/// The stand-in venue's settings, at the venue deployed at `venue`.
pub fn venue_state(venue: &Pubkey) -> Pubkey {
    derive(&[b"venue"], venue)
}

/// The stand-in venue's dollar vault.
pub fn venue_vault(venue: &Pubkey) -> Pubkey {
    derive(&[b"vault"], venue)
}

/// `master`'s margin account at the stand-in venue.
pub fn venue_account(venue: &Pubkey, master: &Pubkey) -> Pubkey {
    derive(&[b"account", master.as_ref()], venue)
}

/// The stand-in venue's market `id`.
pub fn venue_market(venue: &Pubkey, id: &[u8; 32]) -> Pubkey {
    derive(&[b"market", id], venue)
}
