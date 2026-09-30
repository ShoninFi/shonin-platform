// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {AuthorProfiles} from "../src/AuthorProfiles.sol";
import {DevAttestationVerifier} from "../src/DevAttestationVerifier.sol";
import {LocalUsd} from "../src/LocalUsd.sol";
import {LocalVenue} from "../src/LocalVenue.sol";
import {ReviewRegistry} from "../src/ReviewRegistry.sol";
import {StrategyRegistry} from "../src/StrategyRegistry.sol";
import {Vault} from "../src/Vault.sol";
import {VaultFactory} from "../src/VaultFactory.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {VaultConfig} from "../src/interfaces/IVaultFactory.sol";
import {Test} from "forge-std/Test.sol";

/// The whole platform, deployed the way the development chain deploys it, so
/// every test starts from the arrangement the running system actually has.
contract Fixture is Test {
    uint256 internal constant ATTESTOR_KEY = 0xA11E5;
    uint256 internal constant BUILDER_KEY = 0xB1D;
    uint256 internal constant REVIEWER_KEY = 0x8E7;
    uint256 internal constant EXECUTOR_KEY = 0xE8EC;

    bytes32 internal constant EXECUTOR_IMAGE = keccak256("executor image");
    bytes32 internal constant BUILDER_IMAGE = keccak256("builder image");
    bytes32 internal constant REVIEWER_IMAGE = keccak256("reviewer image");
    bytes32 internal constant PROJECT = keccak256("project");

    bytes32 internal constant SPOT = "BTC";
    bytes32 internal constant PERP = "BTC-PERP";
    uint256 internal constant PRICE = 60_000e6;
    /// Ten per cent: the venue lets a perp run at ten times its margin.
    uint256 internal constant MARGIN_BPS = 1000;

    uint64 internal constant TIMELOCK = 2 days;
    uint64 internal constant BLESSING_TTL = 72 hours;
    uint256 internal constant REVIEW_FEE = 1e6;
    uint256 internal constant MINIMUM_TANK = 40e6;

    address internal admin = makeAddr("admin");
    address internal platform = makeAddr("platform");
    address internal author = makeAddr("author");
    address internal alice = makeAddr("alice");
    address internal bob = makeAddr("bob");
    address internal operator = makeAddr("operator");

    address internal attestor = vm.addr(ATTESTOR_KEY);
    address internal builder = vm.addr(BUILDER_KEY);
    address internal reviewer = vm.addr(REVIEWER_KEY);
    address internal executor = vm.addr(EXECUTOR_KEY);

    LocalUsd internal usd;
    LocalVenue internal venue;
    DevAttestationVerifier internal verifier;
    AttestationRegistry internal attestation;
    StrategyRegistry internal strategies;
    ReviewRegistry internal reviews;
    AuthorProfiles internal profiles;
    VaultFactory internal factory;

    function setUp() public virtual {
        // A date far enough in that a timelock and a blessing have room behind
        // them; a chain that starts at zero makes both meaningless.
        vm.warp(365 days);

        usd = new LocalUsd();
        venue = new LocalVenue(usd, operator, MARGIN_BPS);
        verifier = new DevAttestationVerifier(attestor);
        attestation = new AttestationRegistry(admin, verifier, TIMELOCK, BLESSING_TTL);
        strategies = new StrategyRegistry(attestation);
        reviews = new ReviewRegistry(attestation, strategies, usd, platform, REVIEW_FEE);
        profiles = new AuthorProfiles();
        factory = new VaultFactory(
            usd,
            venue,
            attestation,
            strategies,
            reviews,
            admin,
            platform,
            VaultConfig({
                streamingBps: 100,
                authorBps: 1000,
                platformBps: 500,
                transactionBps: 10,
                exitBandBps: 100,
                defaultAutofillBps: 10_000,
                freezeAfter: 7 days,
                tankTargetDays: 30,
                minimumTank: MINIMUM_TANK
            })
        );

        vm.startPrank(operator);
        venue.listMarket(SPOT, false, PRICE);
        venue.listMarket(PERP, true, PRICE);
        vm.stopPrank();

        vm.startPrank(admin);
        attestation.proposeImage(EXECUTOR_IMAGE, ImageKind.Executor);
        attestation.proposeImage(BUILDER_IMAGE, ImageKind.Builder);
        attestation.proposeImage(REVIEWER_IMAGE, ImageKind.Reviewer);
        attestation.proposeProject(PROJECT);
        vm.stopPrank();
        skip(TIMELOCK);

        blessServiceKey(BUILDER_IMAGE, builder);
        blessServiceKey(REVIEWER_IMAGE, reviewer);
    }

    // Helpers ---------------------------------------------------------------

    function give(address who, uint256 amount) internal {
        usd.mint(who, amount);
    }

    function setPrice(bytes32 market, uint256 price, uint256 spreadBps) internal {
        uint256 half = (price * spreadBps) / 20_000;
        vm.prank(operator);
        venue.setPrices(market, price, price, price - half, price + half);
    }

    function journalFor(
        bytes32 image,
        address signer,
        bytes32 vaultsRoot
    ) internal pure returns (bytes memory) {
        return abi.encode(
            AttestationRegistry.Journal({
                imageDigest: image,
                projectId: PROJECT,
                signer: signer,
                vaultsRoot: vaultsRoot
            })
        );
    }

    function sealOf(bytes memory journal) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) =
            vm.sign(ATTESTOR_KEY, MessageHashUtilsLite.toEthSignedMessageHash(keccak256(journal)));
        return abi.encodePacked(r, s, v);
    }

    /// A builder or a reviewer key: it serves every strategy and no vault.
    function blessServiceKey(bytes32 image, address signer) internal {
        bytes memory journal = journalFor(image, signer, bytes32(0));
        attestation.bless(sealOf(journal), journal, address(0), bytes32(0), new bytes32[](0));
    }

    /// An executor key: it may act for one vault running one bundle.
    function blessExecutorKey(address signer, address vault, bytes32 strategyHash) internal {
        bytes32 leaf = keccak256(bytes.concat(keccak256(abi.encode(vault, strategyHash))));
        bytes memory journal = journalFor(EXECUTOR_IMAGE, signer, leaf);
        attestation.bless(sealOf(journal), journal, vault, strategyHash, new bytes32[](0));
    }

    /// Register a version the way the build service does, then open its vault.
    function registerAndOpen(Vault vault, bytes32 strategyId, uint32 version, bytes32 bundleHash) internal {
        bytes32 digest = strategies.registrationDigest(
            strategyId, version, author, keccak256("source"), bundleHash, keccak256("manifest")
        );
        (uint8 v, bytes32 r, bytes32 s) =
            vm.sign(BUILDER_KEY, MessageHashUtilsLite.toEthSignedMessageHash(digest));
        strategies.register(
            strategyId,
            version,
            author,
            keccak256("source"),
            bundleHash,
            keccak256("manifest"),
            bytes32(0),
            0,
            abi.encodePacked(r, s, v)
        );
        vault.open();
    }

    /// Create a vault the way an author does, and open it.
    function openVault(bytes32 strategyId, uint32 version) internal returns (Vault vault, bytes32 bundle) {
        give(author, MINIMUM_TANK);
        vm.startPrank(author);
        usd.approve(address(factory), MINIMUM_TANK);
        vault = Vault(factory.create(strategyId, version, keccak256("source"), MINIMUM_TANK, 2e6));
        vm.stopPrank();
        bundle = keccak256(abi.encode("bundle", strategyId, version));
        registerAndOpen(vault, strategyId, version, bundle);
    }

    function depositAs(Vault vault, address who, uint256 amount) internal returns (uint256 shares) {
        give(who, amount);
        vm.startPrank(who);
        usd.approve(address(vault), amount);
        shares = vault.deposit(amount, who);
        vm.stopPrank();
    }
}

/// forge-std brings its own copy of this; keeping one here means the fixture
/// does not depend on which of them is in scope.
library MessageHashUtilsLite {
    function toEthSignedMessageHash(bytes32 hash) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", hash));
    }
}
