//! What publishing keeps. Nothing here is ever edited after it is recorded,
//! and nothing is ever closed but a draft, which is only a registration on
//! its way in.

use anchor_lang::prelude::*;
use shonin_common::digest;

/// The settings: one per deployment.
pub const CONFIG_SEED: &[u8] = b"config";
/// One paid submission, by its strategy id.
pub const SUBMISSION_SEED: &[u8] = b"submission";
/// A registration being assembled, by strategy id and whoever assembles it.
pub const DRAFT_SEED: &[u8] = b"draft";
/// One registered strategy, by its id.
pub const STRATEGY_SEED: &[u8] = b"strategy";
/// One strategy's review, or its refusal, by its id.
pub const REVIEW_SEED: &[u8] = b"review";

/// The most chains a strategy may name.
pub const MAX_CHAINS: usize = 8;
/// The most drivers one chain may name.
pub const MAX_DRIVERS: usize = 8;
/// The most bridges a strategy may name.
pub const MAX_BRIDGES: usize = 16;
/// The most restricted codes a strategy may carry: the most a vault reads.
pub const MAX_RESTRICTED: usize = 64;

/// The six areas a review rates, in order: security, financial risk,
/// reliability, reproducibility, outside influence, accurate description.
pub const AREAS: usize = 6;
/// The ratings: 1 looks fine, 2 some concerns, 3 serious concerns, 4
/// couldn't tell. Zero is no rating, which no area may have.
pub const LOWEST_RATING: u8 = 1;
pub const HIGHEST_RATING: u8 = 4;

#[account]
#[derive(InitSpace)]
pub struct Config {
    /// The dollar the fee is paid in, and its token program.
    pub mint: Pubkey,
    pub token_program: Pubkey,
    pub decimals: u8,
    /// Who the fee is paid to: its associated token account for the dollar.
    pub platform: Pubkey,
    pub fee: u64,
    /// How many ids have been assigned. Ids start at 1.
    pub count: u64,
    pub bump: u8,
}

/// One paid submission.
#[account]
#[derive(InitSpace)]
pub struct Submission {
    pub strategy_id: u64,
    /// The wallet that paid. The build service starts the job only if it is
    /// also the wallet the upload named.
    pub author: Pubkey,
    /// The reference the build service gave the uploaded archive. Only a
    /// reference: nothing here vouches for what it points at.
    pub upload: [u8; 32],
    pub paid_at: u64,
    pub bump: u8,
}

/// One chain a strategy trades on, by its name in `shonin.toml` ("solana",
/// "base", ...), with the drivers it trades through there.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct ChainNeed {
    pub chain: [u8; 32],
    pub drivers: Vec<[u8; 32]>,
}

/// What a registration says, before and after it is registered.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Lists {
    /// Every chain the strategy trades on, which its vault's stations are
    /// created from. Nobody chooses them but this record.
    pub chains: Vec<ChainNeed>,
    pub bridges: Vec<[u8; 32]>,
    /// Who its vault refuses deposits from: every driver's list on every
    /// chain, plus the author's additions, sorted and without repeats.
    pub restricted: Vec<[u8; 32]>,
}

impl Lists {
    /// The bytes these lists take, Borsh-encoded.
    pub fn space(&self) -> usize {
        let chains: usize = self
            .chains
            .iter()
            .map(|need| 32 + 4 + 32 * need.drivers.len())
            .sum();
        4 + chains + 4 + 32 * self.bridges.len() + 4 + 32 * self.restricted.len()
    }

    /// Whether the lists are within what a strategy may name.
    pub fn within_bounds(&self) -> bool {
        self.chains.len() <= MAX_CHAINS
            && self
                .chains
                .iter()
                .all(|need| need.drivers.len() <= MAX_DRIVERS)
            && self.bridges.len() <= MAX_BRIDGES
            && self.restricted.len() <= MAX_RESTRICTED
    }

    /// Whether the restricted list is sorted, without repeats.
    pub fn is_sorted(&self) -> bool {
        self.restricted.windows(2).all(|pair| pair[0] < pair[1])
    }
}

/// A registration being assembled in pieces, since a strategy's lists can be
/// longer than one transaction holds. It decides nothing: `register` checks
/// everything it says, and anyone may assemble one, each in their own draft.
#[account]
pub struct Draft {
    pub strategy_id: u64,
    /// Who assembled it, and is refunded its rent when it is used or
    /// discarded.
    pub stager: Pubkey,
    pub author: Pubkey,
    pub source_hash: [u8; 32],
    pub bundle_hash: [u8; 32],
    pub manifest_hash: [u8; 32],
    pub lists: Lists,
    pub bump: u8,
}

impl Draft {
    /// The account size for a draft holding `lists`.
    pub fn space(lists: &Lists) -> usize {
        8 + 8 + 32 + 32 + 32 * 3 + lists.space() + 1
    }
}

/// One registered strategy. A changed strategy is a new strategy with a new
/// id; nothing here is ever edited.
#[account]
pub struct Strategy {
    pub strategy_id: u64,
    pub author: Pubkey,
    /// The source the builder and the reviewer read.
    pub source_hash: [u8; 32],
    /// The bundle it produced: the component and its models.
    pub bundle_hash: [u8; 32],
    pub manifest_hash: [u8; 32],
    pub registered_at: u64,
    pub lists: Lists,
    pub bump: u8,
}

impl Strategy {
    /// The account size for a strategy holding `lists`.
    pub fn space(lists: &Lists) -> usize {
        8 + 8 + 32 + 32 * 3 + 8 + lists.space() + 1
    }
}

/// What a reviewer posts: the report card it signed, which says how the code
/// reads, area by area, and never what it might earn.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReviewInput {
    /// The bundle built from the source; zero in a refusal, where nothing
    /// was built.
    pub bundle_hash: [u8; 32],
    pub source_hash: [u8; 32],
    /// The pinned model, and the prompt and output schema it was given.
    pub model_id_hash: [u8; 32],
    pub prompt_hash: [u8; 32],
    /// The full report, which the page links.
    pub report_hash: [u8; 32],
    /// One rating per area, in order.
    pub ratings: [u8; AREAS],
    pub finding_count: u16,
}

/// One strategy's review, or its refusal. The image and the time are the
/// registry's own knowledge of the key that posted it and of the clock,
/// never the reviewer's claim.
#[account]
#[derive(InitSpace)]
pub struct Review {
    pub strategy_id: u64,
    pub bundle_hash: [u8; 32],
    pub source_hash: [u8; 32],
    pub reviewer_image: [u8; 32],
    pub model_id_hash: [u8; 32],
    pub prompt_hash: [u8; 32],
    pub report_hash: [u8; 32],
    pub ratings: [u8; AREAS],
    pub finding_count: u16,
    pub reviewed_at: u64,
    pub refused: bool,
    pub bump: u8,
}

impl Review {
    /// Whether it is a review a vault can follow: posted, and not a refusal.
    pub fn is_review(&self) -> bool {
        self.reviewed_at != 0 && !self.refused
    }
}

/// What a builder key signs over: everything it registers, bound to this
/// cluster, this program, the strategy's record and its purpose.
#[derive(AnchorSerialize)]
struct RegistrationMessage {
    purpose: [u8; 32],
    cluster: [u8; 32],
    program: Pubkey,
    strategy: Pubkey,
    strategy_id: u64,
    author: Pubkey,
    source_hash: [u8; 32],
    bundle_hash: [u8; 32],
    manifest_hash: [u8; 32],
    chains_hash: [u8; 32],
    bridges_hash: [u8; 32],
    restricted_hash: [u8; 32],
}

/// What a reviewer key signs to let source be built and registered.
#[derive(AnchorSerialize)]
struct ClearanceMessage {
    purpose: [u8; 32],
    cluster: [u8; 32],
    program: Pubkey,
    strategy: Pubkey,
    strategy_id: u64,
    source_hash: [u8; 32],
}

/// The address of strategy `strategy_id`'s record.
pub fn strategy_address(strategy_id: u64) -> Pubkey {
    Pubkey::find_program_address(&[STRATEGY_SEED, &strategy_id.to_le_bytes()], &crate::ID).0
}

/// What a builder key signs over: `keccak256` of the Borsh encoding of the
/// purpose (`keccak256("shonin-strategy-registration")`), the cluster value,
/// this program, the strategy's record, its id, the author, the three hashes,
/// and `keccak256` of each list's Borsh encoding.
#[allow(clippy::too_many_arguments)]
pub fn registration_digest(
    cluster: &[u8; 32],
    strategy_id: u64,
    author: &Pubkey,
    source_hash: &[u8; 32],
    bundle_hash: &[u8; 32],
    manifest_hash: &[u8; 32],
    lists: &Lists,
) -> [u8; 32] {
    let hash = |bytes: Vec<u8>| digest::keccak(&[&bytes]);
    let message = RegistrationMessage {
        purpose: digest::purpose("shonin-strategy-registration"),
        cluster: *cluster,
        program: crate::ID,
        strategy: strategy_address(strategy_id),
        strategy_id,
        author: *author,
        source_hash: *source_hash,
        bundle_hash: *bundle_hash,
        manifest_hash: *manifest_hash,
        chains_hash: hash(borsh::to_vec(&lists.chains).unwrap_or_default()),
        bridges_hash: hash(borsh::to_vec(&lists.bridges).unwrap_or_default()),
        restricted_hash: hash(borsh::to_vec(&lists.restricted).unwrap_or_default()),
    };
    digest::keccak(&[&borsh::to_vec(&message).unwrap_or_default()])
}

/// What a reviewer key signs to clear source for building: `keccak256` of the
/// Borsh encoding of the purpose (`keccak256("shonin-review-clearance")`), the
/// cluster value, this program, the strategy's record, its id and the source
/// hash.
pub fn clearance_digest(cluster: &[u8; 32], strategy_id: u64, source_hash: &[u8; 32]) -> [u8; 32] {
    let message = ClearanceMessage {
        purpose: digest::purpose("shonin-review-clearance"),
        cluster: *cluster,
        program: crate::ID,
        strategy: strategy_address(strategy_id),
        strategy_id,
        source_hash: *source_hash,
    };
    digest::keccak(&[&borsh::to_vec(&message).unwrap_or_default()])
}
