use anchor_lang::prelude::*;

#[error_code]
pub enum RegistryError {
    #[msg("Only the admin may do this")]
    NotAdmin,
    #[msg("Already proposed")]
    AlreadyProposed,
    #[msg("No allowed checker accepted the proof")]
    ProofRejected,
    #[msg("The image is not allowed")]
    ImageNotAllowed,
    #[msg("The operator project is not allowed")]
    ProjectNotAllowed,
    #[msg("A vault key names a vault chain, a vault and a bundle; a service key names none")]
    WrongKindForVault,
    #[msg("The instance did not attest to this vault, bundle and chain")]
    NotAmongTheVaults,
    #[msg("No implementation")]
    NoImplementation,
    #[msg("The attestation is not current")]
    Stale,
    #[msg("Longer than a year")]
    TooLong,
    #[msg("Shorter than this deployment's floor")]
    TooShort,
    #[msg("An image must name its kind")]
    NoKind,
    #[msg("The key belongs to another image or project")]
    NotThisInstance,
    #[msg("The journal names no key")]
    NoSigner,
    #[msg("Only the key that deployed the program may set it up")]
    NotTheDeployer,
    #[msg("This build serves another cluster")]
    WrongCluster,
    #[msg("The program can still be changed by its upgrade authority")]
    NotFinal,
    #[msg("The checker's state account is not its program's")]
    NotAVerifier,
    #[msg("As many checkers as the registry holds are already allowed or waiting")]
    TooManyVerifiers,
    #[msg("The accounts for a checker are not the ones it was proposed with")]
    WrongVerifierAccounts,
    #[msg("The revocation record of the proposal being settled is missing")]
    MissingRevocationRecord,
    #[msg("An account is not the registry record it stands for")]
    AccountSubstituted,
    #[msg("Arithmetic overflow")]
    Overflow,
}
