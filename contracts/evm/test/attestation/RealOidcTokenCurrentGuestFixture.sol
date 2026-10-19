// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

// Written by the attestation-fixture binary of packages/attestation/host
// (`--token`): a real RISC Zero Groth16 proof of the attestation guest over a
// genuine Confidential Space token, signed by Google, checked against Google's
// published keys alone. The guest ran in 298225
// cycles; the Groth16 proof took 126 s. Regenerate it whenever the
// guest changes.
library RealOidcTokenCurrentGuestFixture {
    bytes32 internal constant IMAGE_ID = 0x8380b1a222ddbf918b25839f79b6e4aa9b7bab73e307cf21f8938ea1dacd6cef;
    /// The hash of Google's key that signed the token.
    bytes32 internal constant JWK_HASH = 0x8c87b83d047d492d8a05ebcc84333ff23a5c03d8ebfbd0c5bd88aeca78bbf8eb;
    bytes internal constant SEAL =
        hex"73c457ba25e181977e8c883f5b634a679787993ca01c67e98b638b8ed4e8b142f8ae16590f2abf3148a8c3cfa0a15c808513018f711687fa42a25232b4a5bf5650851852041f795b268d641e35fbd8a1e503af5abbb573d858a9534150a13f872a59cdb3228b91bb4c0f3113553524669b31f3537ddf31f5b54cc8b5e139d7cea1361f760348b1d44225a319766775b2d605558829c17c3f158098ff5d670d42c431ebe318face8a4e1bef4655e5f42c3d6e9aa1963103397a5a2a02540cf9f47d861461093bba1bead563fe79a96182cf976be6eb6762f0c1294488075049fc2feb9b0411c9ce1ec5898490f4d18b7037154d01c778ad99b2b203954f417caa5c6dd0b300000000000000000000000000000000000000000000000000000000000000008c87b83d047d492d8a05ebcc84333ff23a5c03d8ebfbd0c5bd88aeca78bbf8eb";
    bytes internal constant JOURNAL =
        hex"b6192f4f8125235df12b18c764b6ecd068ccbe223cfd5dff24c6934bd3704342b585fbc00011b8d92eaf5863288849faeb95b4ecfb1b28dab38fc94fbd817e1f000000000000000000000000e890cd248bed3547dc084cfb22c62da62f410539f159e7a0989764a9343b846389c9369977d2b2848446a31d8a60ae34f1fee164000000000000000000000000000000000000000000000000000000006ac56093";
    bytes32 internal constant IMAGE_DIGEST =
        0xb6192f4f8125235df12b18c764b6ecd068ccbe223cfd5dff24c6934bd3704342;
    bytes32 internal constant PROJECT_ID = 0xb585fbc00011b8d92eaf5863288849faeb95b4ecfb1b28dab38fc94fbd817e1f;
    address internal constant SIGNER = 0xE890cd248BED3547dC084cfb22C62DA62F410539;
    bytes32 internal constant VAULTS_ROOT = 0xf159e7a0989764a9343b846389c9369977d2b2848446a31d8a60ae34f1fee164;
    uint64 internal constant ISSUED_AT = 1_791_320_211;
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
