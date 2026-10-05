// SPDX-License-Identifier: BUSL-1.1
pragma solidity ^0.8.30;

// RISC Zero's Groth16 verifier, built with the contracts so a deploy finds
// its artifact: the Confidential Space checker calls it on chains where
// RISC Zero has not deployed one of its own. Its source is vendored under
// lib/risc0-ethereum with its own licence.
import {RiscZeroGroth16Verifier} from "risc0/groth16/RiscZeroGroth16Verifier.sol";
