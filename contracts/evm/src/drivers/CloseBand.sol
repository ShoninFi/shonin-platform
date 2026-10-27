// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

/// The band `IDriver.close` is given, and how a stopped station's full unwind
/// marks its own.
///
/// A claim's close is given the station's exit band, at most 10% (so never
/// near `STOPPED`). A stopped station's full unwind is given `STOPPED` beside
/// its band as it stands: the exit band for the first `WIDEN_AFTER` after the
/// stop, then doubling each `WIDEN_AFTER` until it is full (`BPS`); from then
/// on the low bits count on past `BPS`, one for each `WIDEN_AFTER` since it
/// became full, so a driver can go on widening what it accepts on the same
/// schedule. A band of `BPS` or more without the mark is read as a stopped
/// unwind's full band too.
library CloseBand {
    /// The mark of a stopped station's full unwind.
    uint16 internal constant STOPPED = 0x8000;
    uint256 private constant LOW = 0x7FFF;
    uint256 private constant BPS = 10_000;

    /// The band a stopped station's full unwind is given, marked, `elapsed`
    /// after the stop: `exitBand` for the first `widenAfter`, then doubling
    /// each `widenAfter` until full, then the full band and one more for
    /// each `widenAfter` since it became full.
    function stopped(uint256 exitBand, uint256 elapsed, uint256 widenAfter) internal pure returns (uint16) {
        uint256 periods = elapsed / widenAfter;
        uint256 widened = exitBand;
        uint256 doublings;
        while (doublings < periods && widened < BPS) {
            widened = widened == 0 ? 2 : widened * 2;
            doublings++;
        }
        if (widened < BPS) return uint16(STOPPED | widened);
        uint256 past = periods - doublings;
        if (past > LOW - BPS) past = LOW - BPS;
        return uint16(STOPPED | (BPS + past));
    }

    /// The band itself, never past the full band: what a driver that keeps
    /// no schedule of its own past the full band closes inside.
    function band(uint16 bandBps) internal pure returns (uint16) {
        uint256 low = bandBps & LOW;
        return uint16(low > BPS ? BPS : low);
    }

    /// Whether the close is a stopped station's full unwind.
    function isStopped(uint16 bandBps) internal pure returns (bool) {
        return bandBps & STOPPED != 0 || bandBps & LOW >= BPS;
    }

    /// How many `WIDEN_AFTER`s it is since a stopped unwind's band became
    /// full: nothing before then.
    function pastFull(uint16 bandBps) internal pure returns (uint256) {
        uint256 low = bandBps & LOW;
        return low > BPS ? low - BPS : 0;
    }
}
