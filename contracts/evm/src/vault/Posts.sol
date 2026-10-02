// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {IAttestationRegistry, ImageKind} from "../interfaces/IAttestationRegistry.sol";
import {IStation, Send} from "../interfaces/IStation.sol";
import {ClaimState, IVault, StationPost, Valuation, ValuationStatus} from "../interfaces/IVault.sol";
import {Valuations} from "./Valuations.sol";
import {Layout, Post, VaultStorage} from "./VaultStorage.sol";
import {ECDSA} from "openzeppelin/utils/cryptography/ECDSA.sol";
import {MessageHashUtils} from "openzeppelin/utils/cryptography/MessageHashUtils.sol";
import {Math} from "openzeppelin/utils/math/Math.sol";

/// The stations on other chains: their signed posts, which complete a
/// valuation, the valuation's close once the post window has passed, and
/// the exit costs their unwinders report.
library Posts {
    using VaultStorage for Layout;
    using Math for uint256;

    uint256 private constant WAD = VaultStorage.WAD;
    /// A trillion trillion dollars in the asset's six decimals.
    uint256 private constant MAX_POST = 1e30;

    /// One station's word on what it held at the valuation's time. Posts may
    /// disagree: the highest mark prices deposits and the lowest exit prices
    /// withdrawals, so neither side is priced in its own favour.
    function postStation(
        uint64 time,
        uint256 stationChain,
        StationPost calldata post,
        bytes calldata signature
    ) external {
        Layout storage s = VaultStorage.layout();
        uint256 index = s.valuations.length;
        if (index == 0) revert IVault.NotAllowed();
        index -= 1;
        Valuation storage v = s.valuations[index];
        if (v.time != time || v.status != ValuationStatus.Open) revert IVault.NotAllowed();
        if (block.timestamp > time + s.timers.postDeadline) revert IVault.OutsideWindow();
        if (!_isOtherChain(s, stationChain)) revert IVault.NotAllowed();

        bytes32 digest = MessageHashUtils.toEthSignedMessageHash(postDigest(time, stationChain, post));
        (address signer, ECDSA.RecoverError err,) = ECDSA.tryRecover(digest, signature);
        if (err != ECDSA.RecoverError.NoError || !_blessed(s, signer, false)) revert IVault.NotBlessed();

        (uint256 markValue, uint256 exitValue, bool late, uint256 inTransit) =
            _counted(s, time, stationChain, post);
        // No figure a valuation can't add up and price: a post beyond any
        // dollar amount that could exist would leave the valuation open for
        // good, with every way out waiting behind it.
        if (markValue > MAX_POST || exitValue > MAX_POST) revert IVault.NotAllowed();
        if (post.outsideBand || late) s.wide[index] = true;
        uint256 extra;
        if (exitValue > markValue) (extra, exitValue) = (exitValue - markValue, markValue);
        Post storage p = s.posts[index][stationChain];
        if (inTransit != 0) s.moving[index] = true;
        if (!p.posted) {
            p.posted = true;
            p.mark = markValue;
            p.exit = exitValue;
            p.top = markValue + extra;
            v.postsMissing -= 1;
        } else {
            // Deposits pay the highest figure (`top`); fees and payday are
            // measured on the lowest mark, withdrawals on the lowest exit.
            if (markValue + extra > p.top) p.top = markValue + extra;
            if (markValue < p.mark) p.mark = markValue;
            if (exitValue < p.exit) p.exit = exitValue;
        }
        emit IVault.StationPosted(index, stationChain, signer, markValue, exitValue);
    }

    /// Once the post deadline has passed: price the valuation if every
    /// station posted, or void it so its requests roll on to the next.
    function closeValuation(uint64 time) external {
        Layout storage s = VaultStorage.layout();
        uint256 index = s.valuations.length;
        if (index == 0) revert IVault.NotAllowed();
        index -= 1;
        Valuation storage v = s.valuations[index];
        if (v.time != time || v.status != ValuationStatus.Open) revert IVault.NotAllowed();
        if (block.timestamp <= time + s.timers.postDeadline) revert IVault.NotYet();
        s.closedAt = uint64(block.timestamp);
        if (v.postsMissing != 0 || s.wide[index]) {
            v.status = ValuationStatus.Void;
            emit IVault.ValuationVoided(index, time);
            return;
        }
        uint256 keep = WAD - s.openUnits;
        uint256[] storage chains = s.otherChains;
        for (uint256 i = 0; i < chains.length; i++) {
            Post storage p = s.posts[index][chains[i]];
            v.gross += p.mark.mulDiv(keep, WAD);
            s.depositExtra[index] += (p.top - p.mark).mulDiv(keep, WAD);
            v.grossExit += p.exit.mulDiv(keep, WAD);
        }
        Valuations.price(s, index);
    }

    function postDigest(
        uint64 time,
        uint256 stationChain,
        StationPost calldata post
    ) public view returns (bytes32) {
        return keccak256(
            abi.encode(
                block.chainid, address(this), "shonin-post", time, stationChain, keccak256(abi.encode(post))
            )
        );
    }

    /// A post's value with the money in transit to or from its station: at
    /// par while it is younger than the transit timeout, at zero after.
    /// Sends from the home station count until the post says they landed.
    function _counted(
        Layout storage s,
        uint64 time,
        uint256 stationChain,
        StationPost calldata post
    ) private view returns (uint256 markValue, uint256 exitValue, bool late, uint256 inTransit) {
        uint256 timeout = s.timers.transitTimeout;
        // Everything on its way counts at par. Money still on its way past
        // the timeout has no price anyone can trust: counting it at zero
        // would sell shares cheap to whoever sees it is about to land, so
        // such a valuation prices nothing. Dust in transit in all (a tenth
        // of the minimum deposit or less, what anyone could set moving) is
        // neither waited for nor voids.
        uint256 moving;
        uint256 overdue;
        for (uint256 i = 0; i < post.transit.length; i++) {
            if (post.transit[i].sentAt + timeout < time) overdue += post.transit[i].amount;
            moving += post.transit[i].amount;
        }
        // Each home send the post names as still on its way, once: only one
        // made before the home station was valued (a later one is still in
        // its value), and only one to this chain.
        IStation home = IStation(s.homeStation);
        uint64[] calldata inFlight = post.inFlightFromHome;
        for (uint256 i = 0; i < inFlight.length; i++) {
            uint256 index = inFlight[i];
            if (index >= s.sendsAtOpen || (i != 0 && index <= inFlight[i - 1])) revert IVault.NotAllowed();
            Send memory sent = home.sendAt(index);
            if (sent.toChain != stationChain) revert IVault.NotAllowed();
            if (sent.sentAt + timeout < time) overdue += sent.amount;
            moving += sent.amount;
        }
        uint256 dust = s.minimumDeposit / 10;
        late = overdue > dust;
        if (moving > dust) inTransit = moving;
        markValue = post.mark + moving;
        exitValue = post.exit + moving;
    }

    function _isOtherChain(Layout storage s, uint256 chainId) private view returns (bool) {
        uint256[] storage chains = s.otherChains;
        for (uint256 i = 0; i < chains.length; i++) {
            if (chains[i] == chainId) return true;
        }
        return false;
    }

    /// The cost of bringing a claim's fraction home from another chain, as
    /// its unwinder reports it: the claim's running total, signed, never more
    /// than the deployment's limit, and only ever raised.
    function reportExitCost(
        uint256 claimId,
        uint256 cost,
        address payTo,
        bytes calldata signature
    ) external {
        Layout storage s = VaultStorage.layout();
        ClaimState state = s.claims[claimId - 1].state;
        if (state != ClaimState.Pending && state != ClaimState.Waiting && state != ClaimState.Queued) {
            revert IVault.NotAllowed();
        }
        // Exit costs pay for bringing money home from another chain.
        if (
            s.otherChains.length == 0 || cost > s.maxExitCost || cost <= s.exitCost[claimId]
                || payTo == address(0)
        ) {
            revert IVault.NotAllowed();
        }
        bytes32 digest = MessageHashUtils.toEthSignedMessageHash(exitCostDigest(claimId, cost, payTo));
        (address signer, ECDSA.RecoverError err,) = ECDSA.tryRecover(digest, signature);
        if (err != ECDSA.RecoverError.NoError || !_blessed(s, signer, true)) revert IVault.NotBlessed();
        s.exitCost[claimId] = cost;
        s.exitCostTo[claimId] = payTo;
        emit IVault.ExitCostReported(claimId, cost, payTo);
    }

    function exitCostDigest(uint256 claimId, uint256 cost, address payTo) public view returns (bytes32) {
        return keccak256(abi.encode(block.chainid, address(this), "shonin-exit-cost", claimId, cost, payTo));
    }

    /// A key blessed for this vault and its bundle on this chain: an
    /// unwinder, or (unless only an unwinder will do) an executor.
    function _blessed(Layout storage s, address signer, bool unwinderOnly) private view returns (bool) {
        IAttestationRegistry registry = IAttestationRegistry(s.attestation);
        if (!registry.isBlessedFor(signer, block.chainid, address(this), s.bundleHash)) return false;
        ImageKind kind = registry.keyInfo(signer).kind;
        return kind == ImageKind.Unwinder || (!unwinderOnly && kind == ImageKind.Executor);
    }
}
