// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

// Written by the attestation-fixture binary of packages/attestation/host
// (`--token`): a real RISC Zero Groth16 proof of the attestation guest over a
// genuine Confidential Space token, a PKI token signed under Google's Confidential
// Space Root CA, checked against that root alone. The guest ran in 887401
// cycles; the Groth16 proof took 185 s. Regenerate it whenever the
// guest changes.
library RealPkiTokenFixture {
    bytes32 internal constant IMAGE_ID = 0x8380b1a222ddbf918b25839f79b6e4aa9b7bab73e307cf21f8938ea1dacd6cef;
    /// The hash of the root certificate the token's chain ends in:
    /// Google's Confidential Space Root CA.
    bytes32 internal constant ROOT_HASH = 0x148b293821bb0c6a317f413c8ba475814091cb22d49b9e3c94198db8e8f86c39;
    bytes internal constant SEAL =
        hex"73c457ba21f39c4854ebe10ca504d027826ec2394b9905d1ac9005375c0db18e2aca8da2056392cd1602e51cf9ad6ae80c2acb1a2dbf019ea3fd9c6551bfb5d0d3cf9a821cb3552c13ac5949b25264135deb05749b0ce58c2cf19b99a6c12a24d6e6fb022a85cdc1731e5e170151c2362f67e0ce1d2569b148f4179039a085f990dcfda72e1085133cad31fa5d4b10439fa7e84dd129abec1012902a9f07ba4daaa1cad2124f2b74ef97392e0574fd98c4da4e404960231696ef8ac83297b22a5a4a1bcf2d7df01052a572b77573935e2570347f0379918f6390dc3b37f7eddc3c93901a006cb117db5a6216d916e0f8746c1132c864c8758613786804557b103b49a1e70000000000000000000000000000000000000000000000000000000000000000148b293821bb0c6a317f413c8ba475814091cb22d49b9e3c94198db8e8f86c39";
    bytes internal constant JOURNAL =
        hex"b6192f4f8125235df12b18c764b6ecd068ccbe223cfd5dff24c6934bd3704342b585fbc00011b8d92eaf5863288849faeb95b4ecfb1b28dab38fc94fbd817e1f000000000000000000000000e890cd248bed3547dc084cfb22c62da62f410539f159e7a0989764a9343b846389c9369977d2b2848446a31d8a60ae34f1fee164000000000000000000000000000000000000000000000000000000006ac56094";
    bytes32 internal constant IMAGE_DIGEST =
        0xb6192f4f8125235df12b18c764b6ecd068ccbe223cfd5dff24c6934bd3704342;
    bytes32 internal constant PROJECT_ID = 0xb585fbc00011b8d92eaf5863288849faeb95b4ecfb1b28dab38fc94fbd817e1f;
    address internal constant SIGNER = 0xE890cd248BED3547dC084cfb22C62DA62F410539;
    bytes32 internal constant VAULTS_ROOT = 0xf159e7a0989764a9343b846389c9369977d2b2848446a31d8a60ae34f1fee164;
    uint64 internal constant ISSUED_AT = 1_791_320_212;
    uint256 internal constant VAULT_CHAIN = 31_337;
    address internal constant VAULT = 0x1111111111111111111111111111111111111111;
    bytes32 internal constant STRATEGY_HASH =
        0xd3b76bf6803fde2a360c1efa4b1c8e2e3b79cb80509ade1c0bb70d72c155456e;

    /// Google's Confidential Space keys as published when the checker's key
    /// set was pinned, by hash.
    function googleKeys() internal pure returns (bytes32[] memory keys) {
        keys = new bytes32[](2);
        keys[0] = 0x8c87b83d047d492d8a05ebcc84333ff23a5c03d8ebfbd0c5bd88aeca78bbf8eb;
        keys[1] = 0x8bcab72feb3cd8f7e7981f51c45e6bd383c3ad446e5e2895683ef5662ca022d5;
    }
}
