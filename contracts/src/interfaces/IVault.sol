// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// What a vault is doing right now.
enum VaultState {
    None,
    /// The build service is compiling the submitted source.
    Building,
    /// Taking deposits, trading.
    Open,
    /// A claim went unserved for too long: no new deposits, and the trading
    /// approvals are gone, so only unwinding is left.
    Frozen,
    /// Stopped by the admin key; positions unwound, everyone paid out.
    Killed,
    /// Nothing left to do.
    Closed
}

/// A withdrawal the idle balance could not pay.
enum ClaimState {
    None,
    /// Booked. The leaver's shares are pending, and still outstanding, so they
    /// bear the close pro rata with everyone who stayed.
    Pending,
    /// A settlement was tried and the market was outside the exit band.
    Waiting,
    /// Closed, valued again, burned and paid.
    Paid
}

struct Claim {
    address owner;
    /// Pending shares, held by the vault until the claim settles.
    uint256 shares;
    /// The fraction of every position closed to serve it, to 18 decimals.
    uint256 fractionClosed;
    uint64 bookedAt;
    ClaimState state;
    /// What it paid, once the cash was home.
    uint256 settledFor;
}

/// The schedule a vault is created with. It never changes under a running
/// vault.
struct Fees {
    /// Charged on value, per year, to the platform.
    uint16 streamingBps;
    /// Charged on gains above the high-water mark, to the author.
    uint16 authorBps;
    /// Charged on the same gains, to the platform.
    uint16 platformBps;
    /// Charged on every deposit and withdrawal, and kept in the pool.
    uint16 transactionBps;
}

/// The author's balance for the strategy's running costs. Never depositors'
/// money: it is excluded from what shares are worth.
struct Tank {
    uint256 balance;
    /// What auto-fill tops it back up to.
    uint256 target;
    /// Unpaid charges, taken from future author fees before the author.
    uint256 debt;
    /// The most a day's usage may charge: the declared allowances at the
    /// published prices.
    uint256 dailyCap;
    /// The share of each author fee that refills the tank.
    uint16 autofillBps;
    /// The last day a usage statement was accepted for.
    uint64 lastChargedDay;
    /// What the last accepted statement charged.
    uint256 lastCharge;
}

/// Pooled capital behind one strategy version.
///
/// Its shares are an ordinary ERC-20, so nothing about them is described
/// here: a wallet, an aggregator or another contract holds and moves them
/// knowing only the standard.
///
/// The contract computes its own value from the chain, in the same
/// transaction, and lets anyone leave at any time: at once from the idle
/// balance, or as a claim it settles itself by closing the leaver's fraction.
/// Shares follow ERC-4626 accounting, but `redeem` may book a claim instead of
/// paying, so it is not a compliant ERC-4626 vault and integrators should
/// expect that.
///
/// `moveCapital` names accounts by id: `bytes32("idle")` is the balance on
/// this contract, `bytes32("venue")` the vault's account on the venue. They
/// are the only two that exist, which is why nothing can leave.
interface IVault {
    event Deposited(address indexed receiver, uint256 assets, uint256 shares);
    event Withdrawn(address indexed receiver, uint256 shares, uint256 assets);
    event ClaimBooked(
        uint256 indexed claimId, address indexed receiver, uint256 shares, uint256 fractionClosed
    );
    event ClaimWaiting(uint256 indexed claimId, string reason);
    event ClaimSettled(uint256 indexed claimId, uint256 assets);
    event Frozen(uint256 indexed claimId);
    event Crystallised(
        uint256 streamingShares, uint256 authorShares, uint256 platformShares, uint256 highWaterMark
    );
    event TankFunded(address indexed by, uint256 amount);
    event TankCharged(bytes32 indexed kind, uint256 amount, uint256 debt);
    event CapitalMoved(bytes32 indexed from, bytes32 indexed to, uint256 amount);
    event KeyApproved(bytes32 indexed venue, address indexed key, uint64 validUntil);
    event KeyRevoked(bytes32 indexed venue, address indexed key);
    event Opened(bytes32 bundleHash);
    event BuildFailed(bytes32 reasonHash);
    event Killed(address by);
    event Closed();

    // Depositors ------------------------------------------------------------

    /// Valued at mark, in this transaction, from the chain.
    function deposit(uint256 assets, address receiver) external returns (uint256 shares);
    /// Pays now from the idle balance at exit-side marks, or marks the shares
    /// pending and books a claim. `claimId` is zero when it paid.
    function redeem(uint256 shares, address receiver) external returns (uint256 assets, uint256 claimId);
    /// Anyone: close and sell the leaver's fraction, value again, burn the
    /// pending shares and pay.
    function settle(uint256 claimId) external;
    /// Anyone, once a claim has gone unserved for the freeze period.
    function freeze() external;

    // Value -----------------------------------------------------------------

    /// At mark, less the tank.
    function totalValue() external view returns (uint256);
    /// At exit-side marks, less the tank.
    function exitValue() external view returns (uint256);
    /// The asset each share is worth at mark, to the asset's decimals.
    function sharePrice() external view returns (uint256);
    /// The balance on this contract, which pays a withdrawal at once.
    function idle() external view returns (uint256);
    function highWaterMark() external view returns (uint256);

    // The strategy's own key ------------------------------------------------

    /// A key blessed for this vault: move capital between its own accounts.
    function moveCapital(bytes32 from, bytes32 to, uint256 amount) external;

    // The tank --------------------------------------------------------------

    function fundTank(uint256 assets) external;
    /// A day's usage, signed by a key blessed for this vault, capped by the
    /// contract at the declared allowances.
    function submitUsage(uint64 day, uint256 amount, bytes calldata signature) external;
    /// Author only.
    function setAutofill(uint16 bps) external;
    /// What a usage statement is signed over.
    function usageDigest(uint64 day, uint256 amount) external view returns (bytes32);

    // Fees ------------------------------------------------------------------

    /// Anyone, daily: the streaming and profit fees, minted as new shares.
    function crystallise() external;

    // Lifecycle -------------------------------------------------------------

    /// Anyone, once the registry holds this version: the vault opens.
    function open() external;
    /// A blessed builder key reports that the build failed. The vault closes
    /// and the author reclaims the tank.
    function failBuild(bytes32 reasonHash) external;
    /// The admin key stops the vault: every position closed, all cash home,
    /// every withdrawal paid from the idle balance afterwards.
    function kill() external;

    // Venue approvals -------------------------------------------------------

    /// Anyone; the contract obeys only what the attestation registry justifies.
    function approveKey(bytes32 venue, address key) external;
    function revokeKey(bytes32 venue, address key) external;

    // What the page shows ---------------------------------------------------

    function state() external view returns (VaultState);
    /// The bundle this vault's strategy is, as the registry holds it.
    function bundleHash() external view returns (bytes32);
    function strategyId() external view returns (bytes32);
    function version() external view returns (uint32);
    function author() external view returns (address);
    function sourceHash() external view returns (bytes32);
    function createdAt() external view returns (uint64);
    function lastCrystallised() external view returns (uint64);
    function fees() external view returns (Fees memory);
    function tank() external view returns (Tank memory);
    function exitBandBps() external view returns (uint16);
    function freezeAfter() external view returns (uint32);
    function claimCount() external view returns (uint256);
    function claimAt(uint256 claimId) external view returns (Claim memory);
    /// Shares marked pending across every outstanding claim.
    function pendingShares() external view returns (uint256);
}
