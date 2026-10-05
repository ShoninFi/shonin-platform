// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

// Written by the attestation-fixture binary of packages/attestation/host:
// a real RISC Zero Groth16 proof of the attestation guest over a test token
// (signed by the test key, not Google's). Regenerate it whenever the guest
// changes, since its image id changes with it.
library Groth16Fixture {
    bytes32 internal constant IMAGE_ID = 0x173f3361de2092dc0eb630570982375f05ef92fbbd19119b6860549f3c7250e0;
    bytes32 internal constant JWK_HASH = 0xc55036a6821f217db0af5af23b62b32ef509a833e915c942b5eddd649b6ad0a3;
    bytes internal constant SEAL =
        hex"73c457ba1ed57a85dcf84f58c1ceb011be85b592855dff3ca0ed8b9f595e01c9cba7769116cee350fbba7a3c2526dd97a36b05f5998ba4bc12bb367e1e7ac5bbe1ba3b072ac648dfc311752c955c2a9b2f04a4c01adb109820682eb04dc211ce3495f33022338b4982d2ef1ab8a629b88b3c6b513fe9e6c0e421ddde2fc64cd88d7f6ee90795fef5c1c3ae8f7208b8836184a82c17ac58c977b94b3638942110cc91c3cc25d277ad80ab3364d46d60966a8bbe354ec143cc2d2a80a89a8fdcf543a5b1a724f2595bdfb6a787f55e29a4f4cf1e95e2c24a8457efe13006eda03cb73ec3b4039482ec48002bb7120b244ea09ebcd5881c6d4959ca565c0a5b3686a9da55505a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5ac55036a6821f217db0af5af23b62b32ef509a833e915c942b5eddd649b6ad0a3";
    bytes internal constant JOURNAL =
        hex"3b9e5f2a6c1d4e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7c3c682f613689bf96fc3f8a6076cf3ed9f937f85827784a928d6ab6d7a4eef81000000000000000000000000e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e71c80a77d3da64ad52b7b23c33e907ec20b40f446aff7b4b75a6e49835959ffee000000000000000000000000000000000000000000000000000000006ab13b80";
    bytes32 internal constant IMAGE_DIGEST =
        0x3b9e5f2a6c1d4e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7;
    bytes32 internal constant PROJECT_ID = 0xc3c682f613689bf96fc3f8a6076cf3ed9f937f85827784a928d6ab6d7a4eef81;
    address internal constant SIGNER = 0xe7e7e7E7E7e7e7e7E7E7e7e7E7e7e7E7e7E7E7e7;
    bytes32 internal constant VAULTS_ROOT = 0x1c80a77d3da64ad52b7b23c33e907ec20b40f446aff7b4b75a6e49835959ffee;
    uint64 internal constant ISSUED_AT = 1_790_000_000;
    uint256 internal constant VAULT_CHAIN = 31_337;
    address internal constant VAULT = 0x0A0A0a0a0a0a0a0A0a0a0A0a0A0A0A0a0a0a0a0a;
    bytes32 internal constant STRATEGY_HASH =
        0x5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b;
}
