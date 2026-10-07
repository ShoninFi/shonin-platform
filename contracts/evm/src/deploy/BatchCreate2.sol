// SPDX-License-Identifier: BUSL-1.1
pragma solidity 0.8.30;

/// Creates several contracts in one transaction, each through the standard
/// deterministic deployer, so each lands at the address that deployer gives
/// its salt and creation code: the same address it would have had sent on
/// its own, on every chain. A chain whose transactions land slowly (one large
/// block a minute) then needs one transaction for many creations.
///
/// Holds nothing and answers to nobody. A creation already on the chain is
/// passed over, so the same batch sent twice creates nothing the second
/// time; a creation that fails reverts the whole batch, so a batch either
/// landed in full or not at all. Whoever sends it, the contracts it creates
/// are the same: their creation code names everything they are given.
contract BatchCreate2 {
    /// The deterministic deployer (present on every chain the platform
    /// serves): calldata is a 32-byte salt followed by the creation code.
    address public constant DEPLOYER = 0x4e59b44847b379578588920cA78FbF26c0B4956C;

    /// One creation: its salt, and its creation code with its constructor's
    /// arguments appended.
    struct Creation {
        bytes32 salt;
        bytes code;
    }

    /// The creation at `index` in the batch left no code at its address.
    error NotCreated(uint256 index, address at);

    /// Create each contract not yet on the chain, in order, and return every
    /// one's address with the gas each creation used (zero for one already
    /// there): asked without sending, it measures a batch before it is sent.
    function create(Creation[] calldata creations)
        external
        returns (address[] memory created, uint256[] memory gasUsed)
    {
        created = new address[](creations.length);
        gasUsed = new uint256[](creations.length);
        for (uint256 i = 0; i < creations.length; i++) {
            bytes calldata code = creations[i].code;
            address at = addressOf(creations[i].salt, keccak256(code));
            created[i] = at;
            if (at.code.length != 0) continue;
            uint256 before = gasleft();
            (bool ok,) = DEPLOYER.call(abi.encodePacked(creations[i].salt, code));
            if (!ok || at.code.length == 0) revert NotCreated(i, at);
            gasUsed[i] = before - gasleft();
        }
    }

    /// Where the deterministic deployer puts `codeHash`'s contract under
    /// `salt`.
    function addressOf(bytes32 salt, bytes32 codeHash) public pure returns (address) {
        return address(uint160(uint256(keccak256(abi.encodePacked(bytes1(0xff), DEPLOYER, salt, codeHash)))));
    }
}
