// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

import {Station} from "./Station.sol";
import {IAttestationRegistry, ImageKind} from "./interfaces/IAttestationRegistry.sol";
import {IStationFactory, StationSpec} from "./interfaces/IStation.sol";
import {Clones} from "openzeppelin/proxy/Clones.sol";
import {ECDSA} from "openzeppelin/utils/cryptography/ECDSA.sol";
import {MessageHashUtils} from "openzeppelin/utils/cryptography/MessageHashUtils.sol";

/// Creates a vault's stations, at one address on every EVM chain.
///
/// The factory and the station implementation are deployed through the
/// standard CREATE2 deployer with the same creation code everywhere, so they
/// sit at the same addresses on every chain, and so does each vault's station:
/// a clone salted by the vault's chain and address. A bridge send to "my own
/// address on the other chain" then needs no table anybody could get wrong.
///
/// What differs between chains -- the registry, the dollar, the vault factory
/// -- is set once, after deployment, by the one key named in the creation
/// code. Nobody else can initialise it, so nobody can front-run it.
contract StationFactory is IStationFactory {
    uint16 public constant MAX_EXIT_BAND_BPS = 1000;

    address public immutable initializer;

    address public attestation;
    address public asset;
    address public vaultFactory;
    address public implementation;

    error NotInitializer();
    error AlreadyInitialized();
    error NotInitialized();
    error NotVaultFactory();
    error NotBlessed();
    error WrongChain();
    error OutOfRange();

    constructor(address initializer_) {
        initializer = initializer_;
    }

    function initialize(
        address attestation_,
        address asset_,
        address vaultFactory_,
        address implementation_
    ) external {
        if (msg.sender != initializer) revert NotInitializer();
        if (implementation != address(0)) revert AlreadyInitialized();
        // Zero would leave it uninitialised and open to a second call, or
        // brick every station it creates.
        if (implementation_ == address(0) || attestation_ == address(0) || asset_ == address(0)) {
            revert NotInitialized();
        }
        attestation = attestation_;
        asset = asset_;
        vaultFactory = vaultFactory_;
        implementation = implementation_;
    }

    function createHome(
        address vault,
        bytes32 bundleHash,
        bytes32[] calldata drivers,
        bytes32[] calldata bridges,
        uint256[] calldata chains,
        uint16 exitBandBps
    ) external returns (address) {
        if (msg.sender != vaultFactory) revert NotVaultFactory();
        return _create(
            StationSpec({
                vaultChain: block.chainid,
                vault: vault,
                bundleHash: bundleHash,
                drivers: drivers,
                bridges: bridges,
                chains: chains,
                exitBandBps: exitBandBps
            })
        );
    }

    function create(StationSpec calldata spec, bytes calldata builderSignature) external returns (address) {
        if (spec.vaultChain == block.chainid) revert WrongChain();
        IAttestationRegistry registry = IAttestationRegistry(attestation);
        // The executor of this vault, attested on this chain, asks for it...
        if (
            !registry.isBlessedFor(msg.sender, spec.vaultChain, spec.vault, spec.bundleHash)
                || registry.keyInfo(msg.sender).kind != ImageKind.Executor
        ) revert NotBlessed();
        // ...and the builder that built the bundle said what this chain's
        // station holds.
        bytes32 digest = MessageHashUtils.toEthSignedMessageHash(
            stationDigest(
                spec.vaultChain, spec.bundleHash, spec.drivers, spec.bridges, spec.chains, spec.exitBandBps
            )
        );
        (address builder, ECDSA.RecoverError err,) = ECDSA.tryRecover(digest, builderSignature);
        if (err != ECDSA.RecoverError.NoError || !registry.isBlessedAs(builder, ImageKind.Builder)) {
            revert NotBlessed();
        }
        // The station pins this chain's approved drivers and bridges when it
        // is created, and refuses a name with none.
        return _create(spec);
    }

    function stationOf(uint256 vaultChain, address vault) public view returns (address) {
        return Clones.predictDeterministicAddress(implementation, _salt(vaultChain, vault));
    }

    function exists(uint256 vaultChain, address vault) public view returns (bool) {
        return stationOf(vaultChain, vault).code.length != 0;
    }

    function stationDigest(
        uint256 vaultChain,
        bytes32 bundleHash,
        bytes32[] calldata drivers,
        bytes32[] calldata bridges,
        uint256[] calldata chains,
        uint16 exitBandBps
    ) public view returns (bytes32) {
        return keccak256(
            abi.encode(
                block.chainid,
                address(this),
                "shonin-station",
                vaultChain,
                bundleHash,
                keccak256(abi.encode(drivers)),
                keccak256(abi.encode(bridges)),
                keccak256(abi.encode(chains)),
                exitBandBps
            )
        );
    }

    /// Creating is idempotent: asking again returns the station that exists.
    function _create(StationSpec memory spec) private returns (address station) {
        if (implementation == address(0)) revert NotInitialized();
        if (spec.exitBandBps > MAX_EXIT_BAND_BPS) revert OutOfRange();
        if (exists(spec.vaultChain, spec.vault)) return stationOf(spec.vaultChain, spec.vault);
        station = Clones.cloneDeterministic(implementation, _salt(spec.vaultChain, spec.vault));
        Station(station).initialize(spec);
        emit StationCreated(spec.vaultChain, spec.vault, station);
    }

    function _salt(uint256 vaultChain, address vault) private pure returns (bytes32) {
        return keccak256(abi.encode(vaultChain, vault));
    }
}
