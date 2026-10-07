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
        hex"73c457ba24da55a1295ffdc9ec7c6c94fdd0763d7c7a9a7b8769b84516318ab2cb98f3ea21a4c32219304320015cf89e2f4ea8cca93da3e4929eb63ee2fcb7739f6a9b291b4b6921fc19e680d3335430e90b89396581bac1ceab6cd2d1f49b123e2003d021c77aee523b0f794a4f2776b84d2914360695469f3340a9585db88d266066122781bdff397d3bd5aa6526963ebb7bbea51680dd0ba3c8896c80782e0a65090f0e662836ddc0b76b434bb57421748cacad98abb0377c7240d6e01870af504c9501fc15a619925a62fceeee597755e72f6f6c1352d6ab19cb820b7207fc3035f11a8f9a9a562f2c6be7ce912bc6a1fd1b4c3e1cf117f6a56f12bf5a8e82d153875a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5ac55036a6821f217db0af5af23b62b32ef509a833e915c942b5eddd649b6ad0a3";
    bytes internal constant JOURNAL =
        hex"3b9e5f2a6c1d4e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7c3c682f613689bf96fc3f8a6076cf3ed9f937f85827784a928d6ab6d7a4eef81000000000000000000000000e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e7e73ccab83f008a9478222adfb9bb8d761d31eebdc69efc8f6321ebf20ce012036c000000000000000000000000000000000000000000000000000000006ab13b80";
    bytes32 internal constant IMAGE_DIGEST =
        0x3b9e5f2a6c1d4e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7;
    bytes32 internal constant PROJECT_ID = 0xc3c682f613689bf96fc3f8a6076cf3ed9f937f85827784a928d6ab6d7a4eef81;
    address internal constant SIGNER = 0xe7e7e7E7E7e7e7e7E7E7e7e7E7e7e7E7e7E7E7e7;
    bytes32 internal constant VAULTS_ROOT = 0x3ccab83f008a9478222adfb9bb8d761d31eebdc69efc8f6321ebf20ce012036c;
    uint64 internal constant ISSUED_AT = 1_790_000_000;
    uint256 internal constant VAULT_CHAIN = 31_337;
    address internal constant VAULT = 0x0A0A0a0a0a0a0a0A0a0a0A0a0A0A0A0a0a0a0a0a;
    bytes32 internal constant STRATEGY_HASH =
        0x5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b;
}
