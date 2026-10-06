// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

// Written by the attestation-fixture binary of packages/attestation/host
// (`--token`): a real RISC Zero Groth16 proof of the attestation guest over a
// genuine Confidential Space token, signed by Google, checked against
// Google's published keys alone. The guest ran in 297132 cycles; the
// Groth16 proof took 262 s. Regenerate it whenever the guest changes.
library RealTokenFixture {
    bytes32 internal constant IMAGE_ID = 0x173f3361de2092dc0eb630570982375f05ef92fbbd19119b6860549f3c7250e0;
    /// The hash of Google's key that signed the token.
    bytes32 internal constant JWK_HASH = 0x8c87b83d047d492d8a05ebcc84333ff23a5c03d8ebfbd0c5bd88aeca78bbf8eb;
    bytes internal constant SEAL =
        hex"73c457ba1ad1484236b08983c05de4ffedc010f0c0bd1854ab07f496dd58ab53c63adc233030edd66ab46e7ccac34c21cd20fbe468097b721a9d2dac51d855d9e124bf99090bd58d9ca4e9ae506a3a66f623b288ca239576d364d32fd9d3bb2b9720ad38260865854709fe84ce2cc5dbc08af75d66d4bd5262cb7d5895e16a30c5e482da1c91056b98b554fed559961d61a288862558acb5472c889e353942b5656115032dc4ee98c4d3e31d26fa644f65f3501b3132599c67deceda571eab17274c44a01b061a8abb7ed304ff683a30d60950b7288d55dd3556295d82b71d7fa54c12610d2fb12741f1f0cf397400563f5822b2f3fee9a3593aee08372e13722503ab0700000000000000000000000000000000000000000000000000000000000000008c87b83d047d492d8a05ebcc84333ff23a5c03d8ebfbd0c5bd88aeca78bbf8eb";
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
