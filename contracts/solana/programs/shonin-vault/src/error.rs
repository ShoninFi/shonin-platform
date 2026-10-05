use anchor_lang::prelude::*;

#[error_code]
pub enum VaultError {
    // The vault's own, as the EVM vault names them --------------------------
    #[msg("Not in this state")]
    WrongState,
    #[msg("Not allowed")]
    NotAllowed,
    #[msg("Too small")]
    TooSmall,
    #[msg("The depositor has no statement for the vault's restricted list as it is")]
    NoStatement,
    #[msg("The key is not blessed for this vault as it must be")]
    NotBlessed,
    #[msg("Outside the slot's window")]
    OutsideWindow,
    #[msg("A priced valuation still has requests to settle")]
    Unsettled,
    #[msg("This slot was already valued")]
    AlreadyValued,
    #[msg("Not yet")]
    NotYet,
    #[msg("Nothing to collect")]
    Nothing,
    #[msg("Below the idle floor")]
    BelowFloor,
    #[msg("The usage total is older than the last, or than allowed")]
    Stale,
    // The factory's ----------------------------------------------------------
    #[msg("Only the key that deployed the program may set it up")]
    NotTheDeployer,
    #[msg("Only the admin key")]
    NotAdmin,
    #[msg("Only the strategy's author may activate it")]
    NotTheAuthor,
    #[msg("The strategy is not registered and reviewed")]
    NotReady,
    #[msg("The strategy already has a vault, here or from a factory this one replaces")]
    VaultExists,
    #[msg("The tank is below the factory's minimum")]
    TankTooSmall,
    #[msg("A term is outside its floor or ceiling")]
    OutOfRange,
    #[msg("The terms changed since the author read them")]
    TermsChanged,
    #[msg("A chain is named twice")]
    ChainTwice,
    #[msg("A vault that trades on other chains can't be activated on Solana")]
    OtherChainsNotYet,
    #[msg("Not the dollar this factory uses, or not six decimals")]
    WrongMint,
    // What Solana adds ------------------------------------------------------------
    #[msg("An account is not the one its seeds or records name")]
    WrongAccount,
    #[msg("The settlement's accounts don't start where the queue is, or run out")]
    WrongCursor,
    #[msg("The station did not answer as the station program")]
    StationSilent,
    #[msg("The restricted list is full")]
    ListFull,
    #[msg("The usage total's signature is not in the transaction")]
    NoSignature,
    #[msg("Arithmetic overflow")]
    Overflow,
    #[msg("More requests than one settlement takes")]
    TooMany,
}
