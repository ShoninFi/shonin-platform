use anchor_lang::prelude::*;

#[error_code]
pub enum StationError {
    #[msg("Only the key that deployed the program may set it up")]
    NotTheDeployer,
    #[msg("The vault is not a program-derived address of the vault program")]
    NotAVault,
    #[msg("Not the dollar, or not the vault's own dollar account")]
    WrongDollars,
    #[msg("The exit band is wider than a station allows")]
    BandTooWide,
    #[msg("More drivers than a station holds")]
    TooManyDrivers,
    #[msg("A driver is named twice")]
    DuplicateDriver,
    #[msg("The registry approves no implementation of this driver now, or it is revoked")]
    NotApproved,
    #[msg("The driver program can still be changed")]
    NotFinal,
    #[msg("The driver's allowlist is malformed or names a program a station never calls")]
    BadAllowlist,
    #[msg("The station declares no such driver")]
    NotDeclared,
    #[msg("The key is not blessed as this vault's executor")]
    NotBlessed,
    #[msg("Not allowed")]
    NotAllowed,
    #[msg("The key's blessing still stands and its driver is not revoked")]
    StillBlessed,
    #[msg("The key is not the one approved")]
    NotTheKey,
    #[msg("The station has stopped")]
    AlreadyStopped,
    #[msg("The vault has not halted, or its grace has not passed")]
    NotHalted,
    #[msg("The instruction is for a program the driver's allowlist doesn't name")]
    WrongProgram,
    #[msg("The instruction is not on the driver's allowlist")]
    NotOnTheAllowlist,
    #[msg("An account that must be the station's own is not")]
    ForeignAccount,
    #[msg("The station's address or dollar account, or its authority at the venue, stands where the allowlist doesn't put it")]
    StationOutOfPlace,
    #[msg("The call would spend dollars the station hasn't accounted for")]
    Unaccounted,
    #[msg("A close couldn't bring its worth home")]
    ShortSweep,
    #[msg("A driver prepared an instruction the station can't make")]
    BadCall,
    #[msg("The driver gave no answer")]
    DriverSilent,
    #[msg("The driver's answer doesn't decode")]
    BadAnswer,
    #[msg("The accounts for the drivers are not the ones the station pinned")]
    WrongDrivers,
    #[msg("The vault has a priced valuation to settle first")]
    Unsettled,
    #[msg("More than the station has accounted for")]
    NotEnoughIdle,
    #[msg("The registry records of the key in place are needed")]
    MissingRecords,
    #[msg("Arithmetic overflow")]
    Overflow,
    #[msg("Two drivers name the same venue")]
    SharedVenue,
    #[msg("A call left a token account of the station's open to someone else")]
    LeftOpen,
    #[msg("The call names another venue, a driver program the station pins, or a program no venue call may name")]
    ReachesPinned,
    #[msg("Not the station's authority at the driver's venue, or not its dollar account")]
    WrongAuthority,
    #[msg("Only an instruction that trades may be sent dollars")]
    FundedExit,
    #[msg("A venue call would be handed the station's authority at the venue writable")]
    AuthorityWritable,
    #[msg("A venue call left the station's authority at the venue other than a system account with no data")]
    AuthorityChanged,
}
