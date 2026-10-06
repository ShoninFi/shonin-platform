// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IAttestationRegistry, ImageKind} from "../../interfaces/IAttestationRegistry.sol";
import {IStation, IStationFactory, StationSpec} from "../../interfaces/IStation.sol";

/// One position, in the venue's own units.
struct LighterPosition {
    uint48 account;
    uint16 market;
    /// Signed: positive is long. In the market's base units (its size
    /// decimals).
    int64 size;
}

/// One market's prices, in the venue's price units (its price decimals).
struct LighterPrice {
    uint16 market;
    /// The exchange's mark, which values positions.
    uint32 mark;
    /// The exchange's index: its own price, which the band is measured
    /// against.
    uint32 index;
    /// The book's best bid and ask: where a close would fill.
    uint32 bid;
    uint32 ask;
}

/// Shares one of the station's accounts holds in a public pool.
struct LighterShares {
    uint48 account;
    uint48 pool;
    uint64 shares;
    /// What they are worth, in dollar units.
    uint256 value;
}

/// What an attested machine read from Lighter's API about every account a
/// station owns, at `asOf`: the time of the chain block its on-chain figures
/// were read at.
struct LighterReport {
    uint64 asOf;
    /// Every account index the station owns: its master and the
    /// sub-accounts.
    uint48[] accounts;
    /// What the accounts are worth together at the mark, in dollar units,
    /// pool shares apart.
    uint256 equity;
    /// The station's pending balance at the exchange contract (withdrawals
    /// carried out, not yet paid out) as read on-chain beside the API, at
    /// `asOf`.
    uint256 pending;
    /// The dollars the exchange had taken out of the accounts for
    /// withdrawals that were not yet in `pending` at `asOf`: carried out,
    /// and not yet credited on-chain. A withdrawal is in exactly one of
    /// `equity`, `inFlight` and `pending` at the post's moment, or paid out
    /// (in `arrived`).
    uint256 inFlight;
    /// The station's own counters, read on-chain at the same block as
    /// `pending`: every dollar that had reached it outside its own calls (a
    /// pending withdrawal paid out among them) and every dollar it had put
    /// into the exchange. What has changed in them since is what a payout or
    /// a deposit since the reading moved, so it is counted once.
    uint256 arrived;
    uint256 sent;
    /// The dollars the accounts could withdraw now, per account, in the
    /// order of `accounts`.
    uint256[] available;
    LighterPosition[] positions;
    LighterPrice[] prices;
    LighterShares[] pools;
}

/// Where the attested machines put what they read about the Lighter accounts
/// stations own, and where an executor's key says which Lighter key it
/// holds.
///
/// Nothing on-chain tells what a Lighter account is worth: its state lives
/// in the exchange. So a key blessed for a station's vault, as its executor
/// or its unwinder, reads the station's accounts from the exchange's API
/// inside its attested image and posts them here; the Lighter driver values
/// the station, checks the band and proves a close from the newest post. A
/// post is only as good as the image that made it, which is the attestation
/// it was blessed under.
///
/// A Lighter key is not an Ethereum key, so an executor's key names its
/// Lighter public key here, itself, and the station registers that key at
/// the exchange when it approves the executor.
///
/// It is immutable, has no owner, and holds no money.
contract LighterBoard {
    /// A Lighter public key: five field elements of eight bytes.
    uint256 public constant PUB_KEY_BYTES = 40;
    /// What a post may hold at most, so reading it stays cheap.
    uint256 public constant MAX_ITEMS = 64;

    mapping(address => bytes) private _pubKey;
    mapping(address => bytes) private _report;
    mapping(address => uint64) public reportedAt;

    event PubKeySet(address indexed key, bytes pubKey);
    event Reported(address indexed station, address indexed by, uint64 asOf);

    error NotBlessed();
    error BadKey();
    error Stale();
    error TooBig();

    /// The caller's Lighter public key.
    function setPubKey(bytes calldata pubKey) external {
        if (pubKey.length != PUB_KEY_BYTES) revert BadKey();
        _pubKey[msg.sender] = pubKey;
        emit PubKeySet(msg.sender, pubKey);
    }

    function pubKeyOf(address key) external view returns (bytes memory) {
        return _pubKey[key];
    }

    /// Post what the caller read about `station`'s accounts. Only a key
    /// blessed for the station's vault as its executor or unwinder may, and
    /// only a reading newer than the last and not from the future.
    function post(address station, LighterReport calldata report) external {
        StationSpec memory spec = IStation(station).spec();
        IAttestationRegistry registry =
            IAttestationRegistry(IStationFactory(_factoryOf(station)).attestation());
        if (!registry.isBlessedFor(msg.sender, spec.vaultChain, spec.vault, spec.bundleHash)) {
            revert NotBlessed();
        }
        ImageKind kind = registry.keyInfo(msg.sender).kind;
        if (kind != ImageKind.Executor && kind != ImageKind.Unwinder) revert NotBlessed();
        if (report.asOf > block.timestamp || report.asOf <= reportedAt[station]) revert Stale();
        if (
            report.accounts.length > MAX_ITEMS || report.positions.length > MAX_ITEMS
                || report.prices.length > MAX_ITEMS || report.pools.length > MAX_ITEMS
                || report.available.length != report.accounts.length
        ) revert TooBig();
        _report[station] = abi.encode(report);
        reportedAt[station] = report.asOf;
        emit Reported(station, msg.sender, report.asOf);
    }

    /// The newest post about `station`, and whether there is one.
    function reportOf(address station) external view returns (LighterReport memory report, bool exists) {
        bytes memory raw = _report[station];
        if (raw.length == 0) return (report, false);
        return (abi.decode(raw, (LighterReport)), true);
    }

    function _factoryOf(address station) private view returns (address) {
        return StationFactoryReader(station).factory();
    }
}

interface StationFactoryReader {
    function factory() external view returns (address);
}
