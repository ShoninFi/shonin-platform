// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Call, ExitPath, IDelayedDriver, IDriver} from "../interfaces/IDriver.sol";
import {IStation, IStationVault, Stage} from "../interfaces/IStation.sol";
import {Flow, StationLayout, StationStorage} from "./StationStorage.sol";

import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {SafeERC20} from "openzeppelin/token/ERC20/utils/SafeERC20.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// A station's closes: the one-transaction close, the stopped station's full
/// unwind, and the staged close through drivers whose venue acts after the
/// transaction. It runs in the station's own context (a linked library), on
/// the station's own storage.
///
/// A staged close always ends within its windows. In a block after it was
/// placed, once its settle delay has passed, it is proved: wholly closed,
/// or, once its window is over, for the part that did close (the smallest
/// share of what was ordered that closed, over every market ordered). It is
/// measured for that part against what the account held when it was
/// placed, at the prices now, so the leaver bears every dollar of slippage
/// of everything their stage sold and nothing else; that is asked for from
/// the venue, and delivered once it has landed, or, a delivery window after
/// the ask, the part of it that has landed is delivered. A measure below
/// nothing is a loss the stage's own sale made, owed by whom it was for.
/// Only a stage nothing at all could be measured for by the end of its
/// window (no reading of the venue after it) lapses with nothing paid.
library StationCloses {
    using StationStorage for StationLayout;
    using SafeERC20 for IERC20;
    using Math for uint256;

    uint256 private constant WAD = 1e18;
    uint256 private constant BPS = 10_000;

    error NotAllowed();
    error NotYet();

    // Closing a fraction ----------------------------------------------------

    /// Close `fraction` of every position. Every driver's close is prepared
    /// before any is made, so a market outside the band on one venue closes
    /// nothing anywhere: a leaver is never half-sold. A delayed driver
    /// holding more than dust stages the close instead.
    function close(uint256 fraction, bool forVault) external returns (uint256 sentHome, bool blocked, bool) {
        StationLayout storage s = StationStorage.layout();
        uint256 count = s.spec.drivers.length;
        IDriver[] memory drivers = new IDriver[](count);
        Call[][] memory closes = new Call[][](count);
        uint256[] memory freed = new uint256[](count);
        uint256[] memory before = new uint256[](count);
        bool[] memory skipped = new bool[](count);
        bool delayed;
        for (uint256 i = 0; i < count; i++) {
            drivers[i] = IDriver(s.pinned(s.spec.drivers[i]));
            // What the fraction is worth at exit prices before the close:
            // exactly what selling it realises, and all that comes home for
            // it. The rest stays with whoever still owns it.
            (uint256 markBefore, uint256 exitBefore) = drivers[i].valueOf(address(this));
            // The band first, for every account: a market outside it closes
            // nothing anywhere, whatever any account reads as.
            bool outside;
            (closes[i], outside) = drivers[i].close(address(this), fraction, s.spec.exitBandBps);
            if (outside) {
                emit IStation.Unwound(fraction, 0, true);
                return (0, true, false);
            }
            // An account worth no more than a few dollar-units is dust: a
            // fraction of it is left where it is rather than let its rounding
            // hold every leaver's close. A full unwind still sells it.
            if (fraction < WAD && markBefore <= count + 1) {
                skipped[i] = true;
                continue;
            }
            freed[i] = exitBefore.mulDiv(fraction, WAD, Math.Rounding.Floor);
            before[i] = exitBefore;
            // A fraction worth more than dust at the mark and nothing at exit
            // prices (an account under water at the bid) is never sold for
            // the leaver: it would bring them nothing and leave the loss with
            // everyone else. Only a full unwind, which everyone shares, may.
            if (
                freed[i] == 0 && markBefore.mulDiv(fraction, WAD) > (count + 1) * (count + 1)
                    && fraction < WAD
            ) {
                revert IStation.ShortSweep();
            }
            if (drivers[i].exitPath() == ExitPath.Delayed) delayed = true;
        }
        if (delayed) {
            // On the home station only the vault stages a close, for whom it
            // asks, and it is delivered through the vault's own door.
            if (s.isHome() && !forVault) revert NotAllowed();
            return (0, true, _stage(s, fraction, drivers, closes, freed, before, skipped));
        }

        uint256 idleBefore = s.balance();
        uint256 brought;
        for (uint256 i = 0; i < count; i++) {
            if (!skipped[i]) brought += _closeOne(s, drivers[i], closes[i], freed[i], before[i]);
        }
        sentHome = brought + idleBefore.mulDiv(fraction, WAD, Math.Rounding.Floor);
        if (sentHome != 0) s.home(sentHome);
        emit IStation.Unwound(fraction, sentHome, false);
        return (sentHome, false, false);
    }

    /// Close and sweep one venue whose closes settle in the transaction, and
    /// what of it counts as the fraction's: never more than its worth.
    function _closeOne(
        StationLayout storage s,
        IDriver driver,
        Call[] memory calls,
        uint256 freed,
        uint256 exitBefore
    ) private returns (uint256) {
        address[] memory targets = driver.targets();
        uint256 held = s.balance();
        s.run(calls, targets);
        s.run(driver.sweep(address(this), freed), targets);
        // Selling the fraction is only a serve if its worth comes home:
        // proceeds left at the venue would still belong to everyone.
        uint256 swept = s.balance() - held;
        // Nothing home for a fraction worth something is never a fill.
        if (swept < freed.mulDiv(BPS - s.spec.exitBandBps, BPS) || (swept == 0 && freed != 0)) {
            revert IStation.ShortSweep();
        }
        // A shortfall is the leaver's own fill, never the stayers' gain:
        // proceeds that went to repay the account's margin debt, so the
        // account is left worth more than the stayers' share, still belong
        // to the leaver, and the close is undone.
        if (swept < freed) {
            (, uint256 exitAfter) = driver.valueOf(address(this));
            if (exitAfter > exitBefore - freed + 1) revert IStation.ShortSweep();
            return swept;
        }
        // Never more than the fraction's worth: anything a venue sends
        // beyond it stays here, everyone's.
        return freed;
    }

    // A stopped station's unwind --------------------------------------------

    /// A stopped station's full unwind, which every holder shares: each
    /// driver closes on its own, inside the band as it has widened since the
    /// stop, so a venue that refuses or a market still outside the band
    /// leaves only its own money behind. Everything idle then goes home. It
    /// takes the place of a staged close: whatever that sold or brought home
    /// is everyone's again.
    function unwindStopped(bytes32[] memory drivers) external returns (uint256 sentHome, bool blocked) {
        StationLayout storage s = StationStorage.layout();
        // Not between a valuation's pricing and its settlement: what this
        // sale brings home must not pay withdrawals priced before it.
        if (s.isHome() && (IStationVault(s.spec.vault).valuing() || IStationVault(s.spec.vault).unsettled()))
        {
            revert NotAllowed();
        }
        if (s.stage.stagedAt != 0) _lapse(s);
        uint16 band = IStation(address(this)).stoppedBand();
        for (uint256 i = 0; i < drivers.length; i++) {
            s.pinned(drivers[i]);
            try IStation(address(this)).closeAll(drivers[i], band) returns (bool outside) {
                if (outside) blocked = true;
            } catch {
                blocked = true;
                emit IStation.DriverFailed(drivers[i]);
            }
        }
        sentHome = s.balance();
        if (sentHome != 0) s.home(sentHome);
        emit IStation.Unwound(WAD, sentHome, blocked);
    }

    /// One driver's part of a stopped station's full unwind.
    function closeAll(bytes32 name, uint16 band) external returns (bool outside) {
        StationLayout storage s = StationStorage.layout();
        IDriver driver = IDriver(s.pinned(name));
        bool delayed = driver.exitPath() == ExitPath.Delayed;
        // Through a venue that acts after the transaction, not yet: no
        // sooner after the last move than the driver spaces the executor's
        // moves, whoever calls, so a priced valuation always fits between
        // two of them.
        if (delayed && block.timestamp < IDelayedDriver(address(driver)).movableAt(address(this))) {
            return true;
        }
        (uint256 markBefore, uint256 exitBefore) = driver.valueOf(address(this));
        Call[] memory calls;
        (calls, outside) = driver.close(address(this), WAD, band);
        if (outside) return true;
        address[] memory targets = driver.targets();
        if (delayed) {
            // The venue acts after this transaction: place what is left to
            // close, and sweep home whatever earlier closes freed. Anyone may
            // call again until nothing is left; while anything was ordered
            // the unwind is not done. Dollars it asks for are on their way
            // for a while: the driver's value isn't trusted meanwhile. An
            // account worth no more than a dollar has nothing real to close
            // or bring home, so an unwind through it moves nothing that
            // could go uncounted, and doesn't count as a move.
            s.run(calls, targets);
            Call[] memory sweep = driver.sweep(address(this), type(uint256).max);
            s.run(sweep, targets);
            Flow storage flow = s.flows[name];
            if ((calls.length != 0 || sweep.length != 0) && markBefore > s.oneDollar()) {
                flow.movedAt = uint64(block.timestamp);
            }
            if (band >= BPS && flow.fullUnwindAt == 0) flow.fullUnwindAt = uint64(block.timestamp);
            return calls.length != 0;
        }
        uint256 held = s.balance();
        s.run(calls, targets);
        s.run(driver.sweep(address(this), exitBefore), targets);
        uint256 swept = s.balance() - held;
        if (swept < exitBefore.mulDiv(BPS - band, BPS)) revert IStation.ShortSweep();
    }

    // Delayed venues --------------------------------------------------------

    /// Stage a close whose band every driver has passed: record what each
    /// delayed account holds, then place its closing orders; close and sweep
    /// any other venue at once, its dollars kept here for the stage. Nothing
    /// goes home in this transaction. One stage at a time and none once
    /// stopped; each driver has already refused (as blocked) a close while
    /// dollars moved to or from its venue may still be landing. Nothing is
    /// recorded until every driver has passed.
    function _stage(
        StationLayout storage s,
        uint256 fraction,
        IDriver[] memory drivers,
        Call[][] memory closes,
        uint256[] memory freed,
        uint256[] memory before,
        bool[] memory skipped
    ) private returns (bool) {
        if (s.stage.stagedAt != 0 || s.stopped) {
            emit IStation.Unwound(fraction, 0, true);
            return false;
        }
        uint64 window;
        uint64 delay;
        uint256 estimate;
        for (uint256 i = 0; i < drivers.length; i++) {
            if (skipped[i] || drivers[i].exitPath() != ExitPath.Delayed) continue;
            bytes32 name = s.spec.drivers[i];
            IDelayedDriver driver = IDelayedDriver(address(drivers[i]));
            window = uint64(Math.max(window, driver.deliveryWindow()));
            delay = uint64(Math.max(delay, driver.settleDelay()));
            s.stagedExposure[name] = driver.exposure(address(this));
            s.stagedLedger[name] = driver.ledger(address(this));
            s.stagedFreed[name] = freed[i];
            estimate += freed[i];
        }
        uint256 owed = s.balance().mulDiv(fraction, WAD, Math.Rounding.Floor);
        for (uint256 i = 0; i < drivers.length; i++) {
            if (skipped[i]) continue;
            if (drivers[i].exitPath() == ExitPath.Delayed) {
                s.run(closes[i], drivers[i].targets());
            } else {
                owed += _closeOne(s, drivers[i], closes[i], freed[i], before[i]);
            }
        }
        s.stage = Stage({
            fraction: fraction,
            owed: owed,
            estimate: estimate,
            fetched: 0,
            cost: 0,
            part: 0,
            arrivedAt: s.arrived,
            stagedAt: uint64(block.timestamp),
            stagedBlock: uint64(block.number),
            sweptAt: 0,
            sweptBlock: 0,
            window: window,
            delay: delay,
            forVault: s.isHome()
        });
        emit IStation.CloseStaged(fraction, owed, estimate);
        emit IStation.Unwound(fraction, 0, true);
        return true;
    }

    /// Move a staged close on (see the library's note).
    function settle() external {
        StationLayout storage s = StationStorage.layout();
        Stage memory current = s.stage;
        if (current.stagedAt == 0) revert NotAllowed();
        uint256 arrived = s.arrived - current.arrivedAt;
        if (current.sweptBlock == 0) {
            if (
                block.number <= current.stagedBlock
                    || block.timestamp < uint256(current.stagedAt) + current.delay
            ) {
                revert NotYet();
            }
            bool over = block.timestamp >= uint256(current.stagedAt) + current.window;
            (uint256 part, bool known) = _closedPart(s, current);
            // Wholly closed proves it at once; anything less waits for the
            // window, and is then measured for the part that closed.
            if ((!known || part < WAD) && !over) revert NotYet();
            if (!known) {
                // Nothing the venue did after the stage can be read: nothing
                // can be measured, and the stage ends with nothing paid.
                _lapse(s);
                return;
            }
            int256 measured = int256(arrived);
            for (uint256 i = 0; i < s.spec.drivers.length; i++) {
                bytes32 name = s.spec.drivers[i];
                if (s.stagedExposure[name].length == 0) continue;
                int256 one = IDelayedDriver(s.pinned(name)).measure(
                    address(this),
                    s.stagedLedger[name],
                    s.stagedExposure[name],
                    current.fraction,
                    part,
                    s.spec.exitBandBps
                );
                measured += one;
                // What to ask this venue for: its own part, as measured.
                s.stagedFreed[name] = one > 0 ? uint256(one) : 0;
            }
            current.part = part;
            if (measured < 0) {
                current.cost = uint256(-measured);
            } else {
                // Never above the closed part's worth at exit prices when it
                // was placed.
                current.fetched = Math.min(uint256(measured), current.estimate.mulDiv(part, WAD));
            }
            s.stage.part = part;
            s.stage.fetched = current.fetched;
            s.stage.cost = current.cost;
            if (arrived < current.fetched) {
                uint256 asked = _sweepStaged(s, current.fetched - arrived);
                s.stage.sweptAt = uint64(block.timestamp);
                s.stage.sweptBlock = uint64(block.number);
                emit IStation.CloseSwept(current.fraction, current.fetched, asked);
                return;
            }
        } else if (arrived < current.fetched && block.timestamp < uint256(current.sweptAt) + current.window) {
            revert NotYet();
        }
        _deliver(s, current, arrived);
    }

    /// The smallest part closed over the staged drivers, and whether every
    /// one of them could tell.
    function _closedPart(
        StationLayout storage s,
        Stage memory current
    ) private view returns (uint256 part, bool known) {
        part = WAD;
        known = true;
        for (uint256 i = 0; i < s.spec.drivers.length; i++) {
            bytes32 name = s.spec.drivers[i];
            if (s.stagedExposure[name].length == 0) continue;
            (uint256 one, bool told) = IDelayedDriver(s.pinned(name)).closedPart(
                address(this), s.stagedExposure[name], current.fraction, current.stagedAt + current.delay
            );
            if (!told) known = false;
            if (one < part) part = one;
        }
    }

    /// Deliver what has landed: all it fetched, or, its delivery window over,
    /// the part of it that has arrived, with that part of what the stage
    /// holds here. The part delivered is `part` x arrived / fetched, rounded
    /// up, so the shares it pays for are never too few. Whatever was asked
    /// for and has not arrived is everyone's from here on.
    function _deliver(StationLayout storage s, Stage memory current, uint256 arrived) private {
        uint256 landed = Math.min(arrived, current.fetched);
        uint256 part = current.fetched == 0
            ? current.part
            : current.part.mulDiv(landed, current.fetched, Math.Rounding.Ceil);
        uint256 amount = Math.min(landed + current.owed.mulDiv(part, WAD), s.balance());
        _clear(s);
        if (current.forVault) {
            s.asset().forceApprove(s.spec.vault, amount);
            IStationVault(s.spec.vault).delivered(amount, part, current.cost, current.owed + current.estimate);
        } else if (amount != 0) {
            s.home(amount);
        }
        emit IStation.CloseSettled(part, current.cost);
        emit IStation.CloseDelivered(current.fraction, amount);
    }

    /// Ask each delayed venue for its measured part of `need`, in order.
    /// What it asks for is the stage's own: the stage voids valuations
    /// until it is delivered, and a delivery waits for it to land or its
    /// window to pass, far longer than a send takes. It is not a move: what
    /// has not landed by the delivery is counted where the venue's reading
    /// has it (a Lighter post names what is carried out and not yet
    /// credited), and nothing waits on a reading made after it.
    function _sweepStaged(StationLayout storage s, uint256 need) private returns (uint256 asked) {
        for (uint256 i = 0; i < s.spec.drivers.length && asked < need; i++) {
            bytes32 name = s.spec.drivers[i];
            uint256 ask = Math.min(s.stagedFreed[name], need - asked);
            if (ask == 0 || s.stagedExposure[name].length == 0) continue;
            IDriver implementation = IDriver(s.pinned(name));
            s.run(implementation.sweep(address(this), ask), implementation.targets());
            asked += ask;
        }
    }

    /// End a stage without a delivery, and tell the vault if it was its.
    function _lapse(StationLayout storage s) private {
        Stage memory current = s.stage;
        _clear(s);
        emit IStation.CloseLapsed(current.fraction);
        if (current.forVault) IStationVault(s.spec.vault).unstaged();
    }

    function _clear(StationLayout storage s) private {
        for (uint256 i = 0; i < s.spec.drivers.length; i++) {
            bytes32 name = s.spec.drivers[i];
            delete s.stagedExposure[name];
            delete s.stagedFreed[name];
            delete s.stagedLedger[name];
        }
        delete s.stage;
    }
}
