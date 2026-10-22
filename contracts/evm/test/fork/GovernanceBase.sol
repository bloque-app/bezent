// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {Enum} from "@gnosis.pm/safe-contracts/contracts/common/Enum.sol";

import {VaultRoles} from "../../src/VaultRoles.sol";
import {ISafe, IRoles, IDelay} from "../../src/interfaces/External.sol";
import {VaultFork} from "./VaultFork.sol";

/// What the governance suites (Governance.t.sol, Guardian.t.sol, Adversarial.t.sol) share: the vault's
/// constants, and the helpers that act as its owners and read its Safe. No tests.
abstract contract GovernanceBase is VaultFork {
    address internal constant SENTINEL = address(0x1);
    // Safe 1.4.1's CompatibilityFallbackHandler: with it, owners' signatures answer EIP-1271 for the Safe.
    address internal constant FALLBACK_HANDLER_141 = 0xfd0732Dc9E303f09fCEf3a7388Ad10A83459Ec99;
    bytes32 internal constant SAFE_MSG_TYPEHASH =
        0x60b3cbf8b4a223d68d641b3b6ddf9a298e7f33710cf3d3a9d1146b5a6150fbca;
    bytes4 internal constant EIP1271_MAGIC = 0x1626ba7e;
    uint256 internal constant GUARD_SLOT = 0x4a204f620c8c5ccdca3fd54d003badd85ba500436a431f0cbda4f558c93c34c8;
    uint256 internal constant FALLBACK_SLOT =
        0x6c9a6c4a39284e37ed1cf53d337577d14212a4870fb976a4366c693b939918d5;

    address internal constant BENEFICIARY = address(0xB0B);

    bytes32 internal allowanceKey;
    uint256 internal cooldown;
    uint256 internal expiration;

    function setUp() public virtual override {
        super.setUp();
        fund(cfg.originToken, uint256(cfg.deliveryBalance) * 3);
        vm.deal(d.safe, 1 ether);
        allowanceKey = VaultRoles.deliveryAllowanceKey(cfg.originToken);
        cooldown = delayMod.txCooldown();
        expiration = delayMod.txExpiration();
    }

    // ------------------------------------------------------------------ helpers

    function reason(string memory why) internal pure returns (bytes memory) {
        return abi.encodeWithSignature("Error(string)", why);
    }

    /// The owners (at the threshold) sign a direct Safe transaction; the guard refuses it for `why`.
    function assertGuardRefuses(address to, bytes memory data, string memory why) internal {
        (bool ok, bytes memory ret) = ownersTry(to, data, Enum.Operation.Call, 2);
        assertFalse(ok, "the guard should refuse");
        assertEq(ret, reason(why), "the guard's reason");
    }

    function lowered() internal view returns (bytes memory) {
        return abi.encodeCall(
            IRoles.setAllowance,
            (
                allowanceKey,
                cfg.deliveryBalance / 2,
                cfg.deliveryMaxRefill / 2,
                cfg.deliveryRefill / 2,
                cfg.deliveryPeriod * 2,
                0
            )
        );
    }

    function raised() internal view returns (bytes memory) {
        return abi.encodeCall(
            IRoles.setAllowance,
            (
                allowanceKey,
                cfg.deliveryBalance * 2,
                cfg.deliveryMaxRefill * 2,
                cfg.deliveryRefill,
                cfg.deliveryPeriod,
                0
            )
        );
    }

    function allowanceBalance() internal view returns (uint128 bal) {
        (,,, bal,) = roles.allowances(allowanceKey);
    }

    /// The owners queue `data` for `to` in the Delay, through the guard.
    function queue(address to, bytes memory data) internal returns (bool, bytes memory) {
        return ownersTry(
            d.delay,
            abi.encodeCall(IDelay.execTransactionFromModule, (to, 0, data, Enum.Operation.Call)),
            Enum.Operation.Call,
            2
        );
    }

    function mustQueue(address to, bytes memory data) internal {
        (bool ok, bytes memory ret) = queue(to, data);
        assertTrue(ok, string(ret));
    }

    /// Anyone runs the oldest queued transaction; returns the Delay's answer.
    function runQueued(address to, bytes memory data) internal returns (bool, bytes memory) {
        vm.prank(outsider);
        return d.delay.call(abi.encodeCall(IDelay.executeNextTx, (to, 0, data, Enum.Operation.Call)));
    }

    function mustRun(address to, bytes memory data) internal {
        (bool ok, bytes memory ret) = runQueued(to, data);
        assertTrue(ok, string(ret));
    }

    function prevOf(address module) internal view returns (address) {
        (address[] memory modules,) = safe.getModulesPaginated(SENTINEL, 10);
        address prev = SENTINEL;
        for (uint256 i; i < modules.length; ++i) {
            if (modules[i] == module) return prev;
            prev = modules[i];
        }
        revert("not a module");
    }

    function guardSlot() internal view returns (address) {
        return address(uint160(uint256(bytes32(safe.getStorageAt(GUARD_SLOT, 1)))));
    }

    function modulesOfTheSafe() internal view returns (uint256) {
        (address[] memory modules,) = safe.getModulesPaginated(SENTINEL, 10);
        return modules.length;
    }

    function _one(bytes32 x) internal pure returns (bytes32[] memory a) {
        a = new bytes32[](1);
        a[0] = x;
    }

    function _one(bool x) internal pure returns (bool[] memory a) {
        a = new bool[](1);
        a[0] = x;
    }
}
