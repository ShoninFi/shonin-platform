// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {ReviewRegistry} from "../src/ReviewRegistry.sol";
import {Review} from "../src/interfaces/IReviewRegistry.sol";
import {Fixture} from "./Fixture.sol";

/// Smaller properties of the registries: a first proposal can be withdrawn,
/// a revocation before a proposal wedges nothing, the reviewer's image and time
/// come from the registry, and the factory's terms are checked.
contract RegistryQuirksTest is Fixture {
    /// `revokeDriver` settles the slot, withdraws any pending proposal, and
    /// then reverts `NoImplementation` if nothing is current, undoing the
    /// withdrawal. So the very first proposal for a name cannot be taken back:
    /// if the admin proposes a wrong implementation, it will become the
    /// trading driver when its timelock passes, and the only ways out are to
    /// propose something else over it (which then becomes the driver) or to
    /// race the revoke after it is ready. The same holds for bridges.
    function test_the_first_driver_proposal_can_be_withdrawn() public {
        address wrong = makeAddr("wrong driver");
        vm.startPrank(admin);
        attestation.proposeDriver("local", wrong);
        attestation.revokeDriver("local");
        vm.stopPrank();
        skip(TIMELOCK);
        assertEq(attestation.tradingDriver("local"), address(0), "the withdrawn proposal went live");
    }

    /// `proposeImage` clears a stale `revoked` flag because it overwrites the
    /// whole struct; `proposeProject` does not touch `_projectRevoked`. So a
    /// project id revoked before it was ever proposed can never be allowed:
    /// the proposal is accepted, waits out its timelock, and is then refused
    /// forever, and `AlreadyProposed` stops a second try.
    function test_a_project_revoked_before_it_was_proposed_can_still_be_proposed() public {
        bytes32 project = keccak256("future operator");
        vm.startPrank(admin);
        attestation.revokeProject(project);
        attestation.proposeProject(project);
        vm.stopPrank();
        skip(TIMELOCK);
        assertTrue(attestation.isProjectAllowed(project), "the project is blocked for good");
        vm.prank(admin);
        vm.expectRevert(AttestationRegistry.AlreadyProposed.selector);
        attestation.proposeProject(project);
    }

    /// The review's `reviewerImage` and `reviewedAt` are whatever the sender
    /// wrote. The sender is a blessed reviewer key, so the registry already
    /// knows which image it is; the record should say that image and this
    /// block, not what the key claims.
    function test_a_review_records_the_image_the_registry_knows_the_reviewer_by() public {
        (uint256 id, bytes32 bundle) = ready();
        // `ready()` already posted a review for `id`; take a fresh strategy.
        id = submitAs(author);
        bundle = keccak256(abi.encode("bundle", id));
        register(id, author, bundle);

        Review memory review = reviewFor(bundle, 1);
        review.reviewerImage = BUILDER_IMAGE;
        review.reviewedAt = uint64(block.timestamp) + 365 days;
        vm.prank(reviewer);
        reviews.submitReview(id, review);

        (Review memory stored,) = reviews.reviewOf(id);
        assertEq(stored.reviewerImage, REVIEWER_IMAGE, "a reviewer can claim to be any image");
        assertEq(stored.reviewedAt, uint64(block.timestamp), "a reviewer can date its review freely");
    }
}
