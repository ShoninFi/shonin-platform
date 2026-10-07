//! Why something was refused. No variant carries a secret, and no message
//! says one: every one of them may be logged or sent back to the asker.

use crate::chain_view::ImageKind;
use crate::purpose::Purpose;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The part named is not well formed.
    #[error("the {0} is malformed")]
    Malformed(&'static str),
    /// No image may ever be given this platform secret's key, so it is not
    /// published.
    #[error("no image may ever have the key for {0}, so it is not published")]
    UnknownPlatformSecret(Purpose),
    /// The root is passed whole, between holders; it has no key pair.
    #[error("the root has no key pair of its own")]
    RootHasNoKeyPair,
    /// The token fails one of the attestation guest's checks.
    #[error("the attestation token is refused: {0}")]
    Token(shonin_attestation_core::Error),
    /// The token is signed by a Google key that no checker the canonical
    /// chain allows trusts.
    #[error("the token is signed by a Google key no allowed checker trusts")]
    UntrustedGoogleKey,
    /// The token's nonce does not bind this request's key and purpose.
    #[error("the token's nonce does not bind this request's key and purpose")]
    Nonce,
    /// The token is older than an exchange accepts.
    #[error("the token is older than an exchange accepts")]
    Stale,
    /// The token's image is not allowed on the canonical chain.
    #[error("the token's image is not on the canonical chain's allowlist")]
    ImageNotAllowed,
    /// The token's operator project is not allowed on the canonical chain.
    #[error("the token's project is not allowed on the canonical chain")]
    ProjectNotAllowed,
    /// The image's kind may never have this purpose's key.
    #[error("an image allowed as {kind} may not have the key for {purpose}")]
    NotForKind { kind: ImageKind, purpose: Purpose },
    /// The purpose is released only with a vault key's proof, and none came.
    #[error("the key for {0} is released only with a vault key's proof")]
    ProofMissing(Purpose),
    /// The proof's signature does not recover, or is not over this request.
    #[error("the vault key's proof is not a valid signature over this request")]
    ProofInvalid,
    /// The proof names another vault or bundle than the purpose does.
    #[error("the vault key's proof is for another vault or bundle")]
    ProofMismatch,
    /// The proving key is not blessed, from the asker's image, for the vault.
    #[error("the vault key is not blessed for that vault and bundle from this image")]
    NotBlessed,
    /// The canonical chain, or the vault's chain, could not be read; nothing
    /// was decided.
    #[error("the chain could not be read")]
    ChainUnavailable,
    /// The vault's chain is one whose registry this holder doesn't read, so
    /// its vault keys' blessings can't be checked.
    #[error("this holder does not read the registry on chain {0}, the vault's chain")]
    VaultChainNotRead(u64),
    /// This instance does not hold the root.
    #[error("this instance does not hold the root")]
    NoRoot,
    /// The root is held, but the canonical chain does not record its signer
    /// as the live root key: nothing derived from it is served.
    #[error("the canonical chain does not record this root as the live root key")]
    NotServing,
    /// A published key's signature is not the expected signer's, for this
    /// purpose and chain.
    #[error("the published key is not signed by the root signer for this purpose and chain")]
    BadPublishedKey,
    /// Sealing failed.
    #[error("sealing failed")]
    Seal,
    /// The sealed message does not open with this key and aad.
    #[error("the sealed message does not open with this key and aad")]
    Open,
    /// The operating system's randomness could not be read.
    #[error("the operating system's randomness could not be read")]
    Randomness,
}
