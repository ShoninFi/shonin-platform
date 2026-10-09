//! What the registry keeps, and the rules that read it.
//!
//! Every record is a program-derived address of this program, one per thing
//! it is about, and none is ever closed: what was proposed, revoked or blessed
//! stays on record, as it does in the EVM contract's storage.

use anchor_lang::prelude::*;

/// The registry's settings: one per deployment.
pub const CONFIG_SEED: &[u8] = b"config";
/// One image, by its digest.
pub const IMAGE_SEED: &[u8] = b"image";
/// One operator project, by its id.
pub const PROJECT_SEED: &[u8] = b"project";
/// One attestation checker ever proposed, by its state account.
pub const VERIFIER_SEED: &[u8] = b"verifier";
/// One key's latest blessing and what it is, by the key.
pub const KEY_SEED: &[u8] = b"key";
/// One key's blessing for one vault and bundle on one vault chain.
pub const BLESSING_SEED: &[u8] = b"blessing";
/// One driver or bridge name's approvals.
pub const APPROVAL_SEED: &[u8] = b"approval";
/// Whether one implementation of a driver or bridge name stands revoked.
pub const REVOKED_SEED: &[u8] = b"revoked";

/// The most attestation checkers allowed or waiting at once. A blessing may
/// have to ask each in turn, and every one it asks adds its accounts to the
/// blessing's transaction, which has to fit on the wire with the journal, the
/// seal and the Merkle proof; a rotation needs two. A retired checker leaves
/// the list.
pub const MAX_VERIFIERS: usize = 4;

/// What a measured image is for. An image is allowlisted as one kind, and a
/// key blessed from it can only do that kind's job.
#[derive(
    AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq, InitSpace,
)]
pub enum ImageKind {
    #[default]
    None,
    Executor,
    Unwinder,
    Builder,
    Reviewer,
}

/// What an instance attests to. The proof is over these bytes, Borsh-encoded:
/// the image digest (32), the project id (32), the instance's key (32), the
/// root of its leaves (32) and when it attested (u64, little-endian): 136
/// bytes.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Journal {
    pub image_digest: [u8; 32],
    pub project_id: [u8; 32],
    /// The instance's own ed25519 key, generated inside it: it names the
    /// instance, and is blessed for nothing unless a leaf names it.
    pub signer: Pubkey,
    /// The root of the (vault chain, vault, bundle, chain, key) leaves of the
    /// keys this instance holds. A builder or a reviewer commits to the leaf
    /// with no vault and its own key, for each chain it serves.
    pub vaults_root: [u8; 32],
    /// When the instance attested. A blessing lasts a TTL from here.
    pub issued_at: u64,
}

/// One allowed attestation checker: a checker program and the state account
/// it checks against (the trust root, a key, a guest id). The state account
/// names it, so one program can serve several checkers.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq, InitSpace)]
pub struct VerifierEntry {
    pub program: Pubkey,
    pub state: Pubkey,
    /// When its timelock runs out.
    pub ready_at: u64,
}

#[account]
#[derive(InitSpace)]
pub struct Config {
    /// The one key that governs the allowlist. Fixed for good.
    pub admin: Pubkey,
    /// What stands for this cluster in leaves and digests:
    /// `keccak256("solana:" + cluster name)`. Fixed for good.
    pub cluster: [u8; 32],
    /// The shortest the timelock can ever be on this deployment, fixed when
    /// it is made: two days where depositors' money is at stake, so nothing
    /// the admin proposes can apply before they have had time to leave, and
    /// zero on a development chain.
    pub min_timelock: u64,
    /// How long a blessing lasts from the attestation. Fixed for good.
    pub blessing_ttl: u64,
    /// The stored timelock, with a lowering waiting to apply. Read the one in
    /// force with [`Config::timelock`].
    pub stored_timelock: u64,
    pub pending_timelock_value: u64,
    pub pending_timelock_at: u64,
    /// The checkers proposed and not retired, oldest first.
    #[max_len(MAX_VERIFIERS)]
    pub verifiers: Vec<VerifierEntry>,
    pub bump: u8,
}

impl Config {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        admin: Pubkey,
        cluster: [u8; 32],
        timelock: u64,
        min_timelock: u64,
        blessing_ttl: u64,
        first_verifier: VerifierEntry,
        bump: u8,
    ) -> Self {
        Self {
            admin,
            cluster,
            min_timelock,
            blessing_ttl,
            stored_timelock: timelock,
            pending_timelock_value: 0,
            pending_timelock_at: 0,
            verifiers: vec![first_verifier],
            bump,
        }
    }

    /// The timelock in force at `now`.
    pub fn timelock(&self, now: u64) -> u64 {
        if self.pending_timelock_at != 0 && now >= self.pending_timelock_at {
            return self.pending_timelock_value;
        }
        self.stored_timelock
    }

    /// A lowering waiting to apply, and when it does; zeros if none.
    pub fn pending_timelock(&self, now: u64) -> (u64, u64) {
        if self.pending_timelock_at == 0 || now >= self.pending_timelock_at {
            return (0, 0);
        }
        (self.pending_timelock_value, self.pending_timelock_at)
    }

    /// Sets the timelock in force now to `value`, at once.
    pub(crate) fn set_timelock_now(&mut self, value: u64) {
        self.stored_timelock = value;
        self.pending_timelock_value = 0;
        self.pending_timelock_at = 0;
    }

    /// Keeps `current` in force until `effective_at`, then `value`.
    pub(crate) fn set_timelock_later(&mut self, current: u64, value: u64, effective_at: u64) {
        self.stored_timelock = current;
        self.pending_timelock_value = value;
        self.pending_timelock_at = effective_at;
    }

    /// When something proposed at `now` becomes ready.
    pub fn ready_at(&self, now: u64) -> Option<u64> {
        now.checked_add(self.timelock(now))
    }

    /// Whether checker `a` was proposed after checker `b`, both still on the
    /// list (one retired is never newer, and nothing is newer than it).
    pub fn is_newer(&self, a: &Pubkey, b: &Pubkey) -> bool {
        let place = |state: &Pubkey| {
            self.verifiers
                .iter()
                .position(|entry| &entry.state == state)
        };
        match (place(a), place(b)) {
            (Some(a), Some(b)) => a > b,
            _ => false,
        }
    }

    /// Whether the checker named by its state account is allowed at `now`.
    pub fn is_verifier_allowed(&self, state: &Pubkey, now: u64) -> bool {
        self.verifiers
            .iter()
            .any(|entry| &entry.state == state && now >= entry.ready_at)
    }
}

/// An image on the allowlist.
#[account]
#[derive(InitSpace)]
pub struct ImageRecord {
    pub kind: ImageKind,
    pub proposed: bool,
    /// When the timelock ran out and it became blessable.
    pub allowed_at: u64,
    pub revoked: bool,
    pub bump: u8,
}

impl ImageRecord {
    pub fn is_allowed(&self, now: u64) -> bool {
        self.proposed && !self.revoked && now >= self.allowed_at
    }
}

/// Whether an image is allowed; one never proposed is not.
pub fn is_image_allowed(image: Option<&ImageRecord>, now: u64) -> bool {
    image.is_some_and(|image| image.is_allowed(now))
}

/// An operator project on the allowlist.
#[account]
#[derive(InitSpace)]
pub struct ProjectRecord {
    pub proposed: bool,
    pub ready_at: u64,
    pub revoked: bool,
    pub bump: u8,
}

impl ProjectRecord {
    pub fn is_allowed(&self, now: u64) -> bool {
        self.proposed && !self.revoked && now >= self.ready_at
    }
}

/// Whether a project is allowed; one never proposed is not.
pub fn is_project_allowed(project: Option<&ProjectRecord>, now: u64) -> bool {
    project.is_some_and(|project| project.is_allowed(now))
}

/// That a checker was once proposed, so it can never be proposed again: a
/// retired checker stays retired, and nothing it blessed comes back without a
/// fresh seal.
#[account]
#[derive(InitSpace)]
pub struct VerifierRecord {
    pub program: Pubkey,
    pub state: Pubkey,
    pub proposed: bool,
    pub bump: u8,
}

/// What the registry knows about one blessed key: its image and project, and
/// its latest blessing. A key generated inside an instance that serves
/// several vaults is blessed for each of them separately; a [`Blessing`]
/// answers for one.
#[account]
#[derive(InitSpace)]
pub struct KeyInfo {
    /// The image the key was generated inside.
    pub image_digest: [u8; 32],
    /// The operator project the instance runs in.
    pub project_id: [u8; 32],
    pub kind: ImageKind,
    /// The bundle the key may run, for an executor or an unwinder key; zero
    /// for a builder or a reviewer, which serve every strategy.
    pub strategy_hash: [u8; 32],
    /// The chain the vault it serves lives on; zero for a builder or a
    /// reviewer.
    pub vault_chain: [u8; 32],
    /// The one vault it may act for; zero for a builder or a reviewer.
    pub vault: [u8; 32],
    /// What the proof was checked against.
    pub trust_root: [u8; 32],
    /// The checker that accepted it, by its state account. Retiring the
    /// checker retires the key.
    pub verifier: Pubkey,
    /// When its last blessing expires: a TTL after the attestation it came
    /// from was issued. Attest again to renew.
    pub blessed_until: u64,
    pub bump: u8,
}

/// One key's blessing for one vault and bundle on one vault chain: until
/// when, and through which checker.
#[account]
#[derive(InitSpace)]
pub struct Blessing {
    pub signer: Pubkey,
    pub vault_chain: [u8; 32],
    pub vault: [u8; 32],
    pub strategy_hash: [u8; 32],
    pub until: u64,
    pub verifier: Pubkey,
    pub bump: u8,
}

/// Whether a key is blessed as `kind` at `now`: its latest blessing hasn't
/// run out, the checker that gave it is still allowed, and so are its image
/// and its project. Revoking any of those reaches every key it covers at once.
pub fn is_blessed_as(
    config: &Config,
    key: &KeyInfo,
    image: Option<&ImageRecord>,
    project: Option<&ProjectRecord>,
    kind: ImageKind,
    now: u64,
) -> bool {
    key.kind == kind
        && key.blessed_until >= now
        && config.is_verifier_allowed(&key.verifier, now)
        && is_image_allowed(image, now)
        && is_project_allowed(project, now)
}

/// Whether a key is blessed for the vault and bundle its `blessing` is for,
/// at `now`, on the same terms as [`is_blessed_as`]. `image` and `project` are
/// the records of the key's image and project.
pub fn is_blessed_for(
    config: &Config,
    blessing: &Blessing,
    image: Option<&ImageRecord>,
    project: Option<&ProjectRecord>,
    now: u64,
) -> bool {
    blessing.vault != [0; 32]
        && blessing.until >= now
        && config.is_verifier_allowed(&blessing.verifier, now)
        && is_image_allowed(image, now)
        && is_project_allowed(project, now)
}

/// Which list a name is on. A name on one is never on the other.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq, InitSpace)]
pub enum ApprovalKind {
    Driver,
    Bridge,
}

impl ApprovalKind {
    /// Its part of a record's address.
    pub fn seed(self) -> &'static [u8] {
        match self {
            ApprovalKind::Driver => b"driver",
            ApprovalKind::Bridge => b"bridge",
        }
    }
}

/// One approved driver or bridge name: the implementation in force, and a
/// newer one waiting out its timelock.
#[account]
#[derive(InitSpace)]
pub struct ApprovalSlot {
    pub kind: ApprovalKind,
    pub name: [u8; 32],
    pub current: Pubkey,
    pub next: Pubkey,
    pub next_ready_at: u64,
    pub bump: u8,
}

impl ApprovalSlot {
    /// Whether the waiting proposal has waited out its timelock.
    pub fn next_is_ready(&self, now: u64) -> bool {
        self.next != Pubkey::default() && now >= self.next_ready_at
    }

    /// The implementation a station created now would pin, before
    /// revocations: the latest approval.
    pub fn effective(&self, now: u64) -> Pubkey {
        if self.next_is_ready(now) {
            self.next
        } else {
            self.current
        }
    }
}

/// Whether one implementation of a name stands revoked. Per name and
/// implementation, so a fix proposed after a revocation counts again once its
/// own timelock passes.
#[account]
#[derive(InitSpace)]
pub struct RevokedRecord {
    pub kind: ApprovalKind,
    pub name: [u8; 32],
    pub implementation: Pubkey,
    pub revoked: bool,
    pub bump: u8,
}

/// Whether `implementation` stands revoked. One proposed again and past its
/// timelock is approved again, even before anything settles it.
pub fn is_revoked(
    slot: Option<&ApprovalSlot>,
    record: Option<&RevokedRecord>,
    implementation: &Pubkey,
    now: u64,
) -> bool {
    if let Some(slot) = slot {
        if &slot.next == implementation && slot.next_is_ready(now) {
            return false;
        }
    }
    record.is_some_and(|record| record.revoked)
}

/// What a station created now pins: the latest approval, unless it was
/// revoked; the default key while none is approved. `record` is the
/// revocation record of the latest approval, if it has one.
pub fn trading(slot: Option<&ApprovalSlot>, record: Option<&RevokedRecord>, now: u64) -> Pubkey {
    let Some(slot) = slot else {
        return Pubkey::default();
    };
    let effective = slot.effective(now);
    if is_revoked(Some(slot), record, &effective, now) {
        return Pubkey::default();
    }
    effective
}
