use anchor_lang::prelude::*;

#[error_code]
pub enum PublishingError {
    #[msg("Only the key that deployed the program may set it up")]
    NotTheDeployer,
    #[msg("The registration is not signed by a blessed builder")]
    NotTheBuilder,
    #[msg("The source was not cleared by a blessed reviewer")]
    NotCleared,
    #[msg("The strategy is already registered")]
    AlreadyRegistered,
    #[msg("The id was not paid for by this author")]
    NotSubmittedByTheAuthor,
    #[msg("The restricted list is not sorted without repeats")]
    NotSorted,
    #[msg("A registration names a source and a bundle")]
    NothingBuilt,
    #[msg("More chains, drivers, bridges or restricted codes than a strategy may name")]
    TooMany,
    #[msg("Only a blessed reviewer key may post a review")]
    NotTheReviewer,
    #[msg("The strategy is not registered")]
    NotRegistered,
    #[msg("The strategy was never submitted")]
    NotSubmitted,
    #[msg("The review is not of the registered bundle and source")]
    WrongBundle,
    #[msg("The strategy already has its review")]
    AlreadyReviewed,
    #[msg("Every area carries a rating")]
    BadRating,
    #[msg("A refusal names the source it read")]
    NoSource,
}
