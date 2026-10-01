// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {AttestationRegistry} from "../src/AttestationRegistry.sol";
import {AuthorProfiles} from "../src/AuthorProfiles.sol";
import {DevAttestationVerifier} from "../src/DevAttestationVerifier.sol";
import {LocalUsd} from "../src/LocalUsd.sol";
import {LocalVenue} from "../src/LocalVenue.sol";

import {Prices} from "../src/Prices.sol";
import {Publishing} from "../src/Publishing.sol";
import {ReviewRegistry} from "../src/ReviewRegistry.sol";
import {StrategyRegistry} from "../src/StrategyRegistry.sol";
import {Vault} from "../src/Vault.sol";
import {VaultFactory} from "../src/VaultFactory.sol";
import {ImageKind} from "../src/interfaces/IAttestationRegistry.sol";
import {Allowances, UnitPrices} from "../src/interfaces/IPrices.sol";
import {Review} from "../src/interfaces/IReviewRegistry.sol";
import {ChainNeed} from "../src/interfaces/IStrategyRegistry.sol";

import {VaultConfig} from "../src/interfaces/IVaultFactory.sol";
import {Test} from "forge-std/Test.sol";
import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";

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
    uint256 internal constant PUBLISH_FEE = 1e6;
    uint256 internal constant MINIMUM_TANK = 40e6;
    uint64 internal constant PRICE_NOTICE = 30 days;

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
    Publishing internal publishing;
    Prices internal prices;
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
        publishing = new Publishing(IERC20(address(usd)), platform, PUBLISH_FEE);
        prices = new Prices(admin, PRICE_NOTICE, publishedPrices());
        strategies = new StrategyRegistry(attestation, publishing);
        reviews = new ReviewRegistry(attestation, strategies, publishing);
        profiles = new AuthorProfiles();
        factory = new VaultFactory(
            VaultFactory.Deps({
                asset: IERC20(address(usd)),
                venue: venue,
                attestation: attestation,
                strategies: strategies,
                reviews: reviews,
                prices: prices,
                admin: admin,
                platform: platform
            }),
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

    /// What an instance attests to, attested now.
    function journalFor(
        bytes32 image,
        address signer,
        bytes32 vaultsRoot
    ) internal view returns (bytes memory) {
        return abi.encode(
            AttestationRegistry.Journal({
                imageDigest: image,
                projectId: PROJECT,
                signer: signer,
                vaultsRoot: vaultsRoot,
                issuedAt: uint64(block.timestamp)
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
        bytes memory journal = journalFor(image, signer, serviceLeaf());
        attestation.bless(sealOf(journal), journal, 0, address(0), bytes32(0), new bytes32[](0));
    }

    /// The leaf a builder or a reviewer commits to: no vault, on this chain.
    function serviceLeaf() internal view returns (bytes32) {
        return
            keccak256(bytes.concat(keccak256(abi.encode(uint256(0), address(0), bytes32(0), block.chainid))));
    }

    /// The leaf an instance commits to for one vault and one bundle, on this
    /// chain.
    function leafFor(address vault, bytes32 strategyHash) internal view returns (bytes32) {
        return
            keccak256(bytes.concat(keccak256(abi.encode(block.chainid, vault, strategyHash, block.chainid))));
    }

    /// An executor key: it may act for one vault running one bundle.
    function blessExecutorKey(address signer, address vault, bytes32 strategyHash) internal {
        bytes memory journal = journalFor(EXECUTOR_IMAGE, signer, leafFor(vault, strategyHash));
        attestation.bless(sealOf(journal), journal, block.chainid, vault, strategyHash, new bytes32[](0));
    }

    /// The prices the Rust side publishes: $0.15 a core-hour, $0.01 a
    /// GiB-hour, $0.15 a GB, and inference at the processor rate.
    function publishedPrices() internal pure returns (UnitPrices memory) {
        return UnitPrices({
            vcpuHour: 0.15e18,
            gibHour: 0.01e18,
            gbEgress: 0.15e18,
            inferenceSecond: uint128(0.15e18) / 3600
        });
    }

    /// Allowances whose day costs exactly `perDay`: all of it gas, which is
    /// charged at face value.
    function allowancesCosting(uint64 perDay) internal pure returns (Allowances memory a) {
        a.gasPerDay = perDay;
    }

    function signed(uint256 key, bytes32 digest) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, MessageHashUtilsLite.toEthSignedMessageHash(digest));
        return abi.encodePacked(r, s, v);
    }

    /// Pay to publish, as the author.
    function submitAs(address who) internal returns (uint256 id) {
        give(who, PUBLISH_FEE);
        vm.startPrank(who);
        usd.approve(address(publishing), PUBLISH_FEE);
        id = publishing.submit(keccak256(abi.encode("upload", who, publishing.count())));
        vm.stopPrank();
    }

    function oneChain() internal pure returns (ChainNeed[] memory chains) {
        chains = new ChainNeed[](1);
        chains[0].chain = "base";
        chains[0].drivers = new bytes32[](1);
        chains[0].drivers[0] = "local";
    }

    /// Register a strategy the way the build service does: the reviewer's
    /// go-ahead, then the builder's signature over what it built.
    function register(uint256 id, address who, bytes32 bundleHash) internal {
        ChainNeed[] memory chains = oneChain();
        bytes32[] memory none = new bytes32[](0);
        bytes32[] memory restricted = new bytes32[](2);
        restricted[0] = "SANCTIONED";
        restricted[1] = "US";
        bytes32 digest = strategies.registrationDigest(
            id, who, keccak256("source"), bundleHash, keccak256("manifest"), chains, none, restricted
        );
        strategies.register(
            id,
            who,
            keccak256("source"),
            bundleHash,
            keccak256("manifest"),
            chains,
            none,
            restricted,
            signed(BUILDER_KEY, digest),
            signed(REVIEWER_KEY, strategies.clearanceDigest(id, keccak256("source")))
        );
    }

    function reviewFor(bytes32 bundleHash, uint8 rating) internal view returns (Review memory r) {
        r.bundleHash = bundleHash;
        r.sourceHash = keccak256("source");
        r.reviewerImage = REVIEWER_IMAGE;
        r.modelIdHash = keccak256("model");
        r.promptHash = keccak256("prompt");
        r.reportHash = keccak256("report");
        for (uint256 i = 0; i < 6; i++) {
            r.ratings[i] = rating;
        }
        r.findingCount = 2;
        r.reviewedAt = uint64(block.timestamp);
    }

    /// Submitted, reviewed and built: ready for its author to activate.
    function ready() internal returns (uint256 id, bytes32 bundleHash) {
        id = submitAs(author);
        bundleHash = keccak256(abi.encode("bundle", id));
        register(id, author, bundleHash);
        vm.prank(reviewer);
        reviews.submitReview(id, reviewFor(bundleHash, 1));
    }

    /// Publish and activate a vault the way an author does, with a daily
    /// usage cap of 2 dollars.
    function openVault() internal returns (Vault vault, bytes32 bundle) {
        uint256 id;
        (id, bundle) = ready();
        give(author, MINIMUM_TANK);
        vm.startPrank(author);
        usd.approve(address(factory), MINIMUM_TANK);
        vault = Vault(factory.activate(id, allowancesCosting(2e6), MINIMUM_TANK));
        vm.stopPrank();
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
