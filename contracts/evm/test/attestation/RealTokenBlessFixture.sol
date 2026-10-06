// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

// Written by the attestation-fixture binary of packages/attestation/host
// (`--token`): a real RISC Zero Groth16 proof of the attestation guest over a
// genuine Confidential Space token, signed by Google, checked against
// Google's published keys alone. The guest ran in 297140 cycles; the
// Groth16 proof took 228 s. Regenerate it whenever the guest changes.
library RealTokenBlessFixture {
    bytes32 internal constant IMAGE_ID = 0x173f3361de2092dc0eb630570982375f05ef92fbbd19119b6860549f3c7250e0;
    /// The hash of Google's key that signed the token.
    bytes32 internal constant JWK_HASH = 0x8c87b83d047d492d8a05ebcc84333ff23a5c03d8ebfbd0c5bd88aeca78bbf8eb;
    bytes internal constant SEAL =
        hex"73c457ba11b4da8f1e0a3fbc53c17e8280f8f38766f846074a23de5c62070df9d359881c234f9cdf7ba828f09f818e7883dd5aded8deba1af1d0879b065b6ce05583102f0556fdbd1c7f582c6fb94dd57ff1135b6583a0e8535e60c1ee9fc8c471dd50da0a66b27a8e8903f40785c7559b6a413f4012d60603f0ffd603b25529ceeea65600fa64b4d476972a68eb20ed86f0220f55eb41c4b3e1faabe696db059045eacc0b070e31fa19dac97a27110c166b3686fdadeb82bb2ed711b3e46ee9dab663b610bfd4b63bfe9c05f3dcfc9b65a2242d97e9568e0779b3496031dd03f9156d8b160fdf8e3f916ae42c7b3c5e51fca12f761a27d3bce952ab5b39458865232d8900000000000000000000000000000000000000000000000000000000000000008c87b83d047d492d8a05ebcc84333ff23a5c03d8ebfbd0c5bd88aeca78bbf8eb";
    bytes internal constant JOURNAL =
        hex"eead1b2f4b6ca31b07ed6a06259dacfae647c3508b9e5f616d48df7fb85f14fcb585fbc00011b8d92eaf5863288849faeb95b4ecfb1b28dab38fc94fbd817e1f00000000000000000000000031d4821d2237b25d9208f9de49e3c7cf535fe7d0ee0e32bf0f72c31a13dfcf6c967d6f67cb031bed6c8783908e476b180a5de090000000000000000000000000000000000000000000000000000000006ac5724b";
    bytes32 internal constant IMAGE_DIGEST =
        0xeead1b2f4b6ca31b07ed6a06259dacfae647c3508b9e5f616d48df7fb85f14fc;
    bytes32 internal constant PROJECT_ID = 0xb585fbc00011b8d92eaf5863288849faeb95b4ecfb1b28dab38fc94fbd817e1f;
    address internal constant SIGNER = 0x31D4821d2237B25d9208F9DE49E3C7CF535fe7d0;
    bytes32 internal constant VAULTS_ROOT = 0xee0e32bf0f72c31a13dfcf6c967d6f67cb031bed6c8783908e476b180a5de090;
    uint64 internal constant ISSUED_AT = 1_791_324_747;
    uint256 internal constant LEAVES = 3;

    /// The `i`th leaf under the root: what `bless` takes besides the seal
    /// and the journal.
    function leaf(uint256 i)
        internal
        pure
        returns (uint256 vaultChain, address vault, bytes32 strategyHash, address key, bytes32[] memory proof)
    {
        if (i == 0) {
            (vaultChain, vault, strategyHash, key) = (
                31_337,
                0x2222222222222222222222222222222222222222,
                0xd3b76bf6803fde2a360c1efa4b1c8e2e3b79cb80509ade1c0bb70d72c155456e,
                0x2E65Ce26d316338bd41096Cad5D377e977E3b034
            );
            proof = new bytes32[](2);
            proof[0] = 0x024c3e4d4d39cd2fdc538daf2e6231438a454c343ec3975bab09b9394ed95282;
            proof[1] = 0x02299b7d11c9a11ef34cad96ca50ed6048577fe82ad30c974391067d88cd2d67;
            return (vaultChain, vault, strategyHash, key, proof);
        }
        if (i == 1) {
            (vaultChain, vault, strategyHash, key) = (
                31_337,
                0x3333333333333333333333333333333333333333,
                0xd3b76bf6803fde2a360c1efa4b1c8e2e3b79cb80509ade1c0bb70d72c155456e,
                0x22aa6A7E9f3024563dd4F39fdE22c76099B1cA6D
            );
            proof = new bytes32[](2);
            proof[0] = 0xbc80e00c1b4cdadcaf1eda19533e3b25c36dcb90ba5bef83622d504b6ddacbd4;
            proof[1] = 0x02299b7d11c9a11ef34cad96ca50ed6048577fe82ad30c974391067d88cd2d67;
            return (vaultChain, vault, strategyHash, key, proof);
        }
        if (i == 2) {
            (vaultChain, vault, strategyHash, key) = (
                0,
                0x0000000000000000000000000000000000000000,
                0x0000000000000000000000000000000000000000000000000000000000000000,
                0xDA5459AA31D19C0ac2fBcD675f3637a659612261
            );
            proof = new bytes32[](1);
            proof[0] = 0x2696849d08824590ba94be26f0f0fd7350139c6fa984d8c4a183a520af93db67;
            return (vaultChain, vault, strategyHash, key, proof);
        }
        revert("no such leaf");
    }

    /// Google's Confidential Space keys as published when the checker's key
    /// set was pinned, by hash.
    function googleKeys() internal pure returns (bytes32[] memory keys) {
        keys = new bytes32[](2);
        keys[0] = 0x8c87b83d047d492d8a05ebcc84333ff23a5c03d8ebfbd0c5bd88aeca78bbf8eb;
        keys[1] = 0x8bcab72feb3cd8f7e7981f51c45e6bd383c3ad446e5e2895683ef5662ca022d5;
    }
}
