// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {Vm} from "forge-std/Test.sol";
import {Enum} from "@gnosis.pm/safe-contracts/contracts/common/Enum.sol";

import {VaultRoles} from "../../src/VaultRoles.sol";
import {IRoles} from "../../src/interfaces/External.sol";
import {Batches} from "../Batches.sol";
import {VaultFork} from "./VaultFork.sol";

/// T-008-04: the delivery role. The MultiSend of (origin-token transfer, reference) within the allowance,
/// and nothing else.
abstract contract DeliveryTest is VaultFork {
    bytes32 internal constant TRANSFER_TOPIC = keccak256("Transfer(address,address,uint256)");
    bytes32 internal constant REFERENCE_TOPIC = keccak256("Reference(bytes32)");

    address internal constant BENEFICIARY = address(0xB0B);

    function setUp() public override {
        super.setUp();
        fund(cfg.originToken, uint256(cfg.deliveryMaxRefill) * 3);
    }

    /// INV-14, REQ-RN-5: the outflow and its reference land in one transaction, from the vault, together.
    function test_INV_14_deliveryCarriesItsReferenceAtomically() public {
        bytes32 ref = keccak256("bridge reference 1");
        uint256 amount = uint256(cfg.deliveryBalance) / 3;
        uint256 before = balance(cfg.originToken, BENEFICIARY);

        vm.recordLogs();
        (bool ok, bytes memory ret) = deliver(BENEFICIARY, amount, ref);
        assertTrue(ok, string(ret));
        Vm.Log[] memory logs = vm.getRecordedLogs();

        assertEq(balance(cfg.originToken, BENEFICIARY) - before, amount);
        assertEq(count(logs, cfg.originToken, TRANSFER_TOPIC), 1, "one transfer");
        assertEq(count(logs, d.emitter, REFERENCE_TOPIC), 1, "one reference");
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].emitter == cfg.originToken && logs[i].topics[0] == TRANSFER_TOPIC) {
                assertEq(address(uint160(uint256(logs[i].topics[1]))), d.safe, "from the vault");
                assertEq(address(uint160(uint256(logs[i].topics[2]))), BENEFICIARY);
                assertEq(abi.decode(logs[i].data, (uint256)), amount);
            }
            if (logs[i].emitter == d.emitter) assertEq(logs[i].topics[1], ref);
        }
    }

    /// INV-14: if the transfer fails, the reference is not emitted and the allowance is not consumed.
    function test_INV_14_failedTransferLeavesNoReference() public {
        uint256 held = balance(cfg.originToken, d.safe);
        (,,, uint128 allowanceBefore,) = roles.allowances(VaultRoles.deliveryAllowanceKey(cfg.originToken));
        // Within the allowance after a top-up of it, above what the vault holds.
        // Governance (2 of 3 owners) raises the allowance above what the vault holds.
        bytes memory raise = abi.encodeCall(
            IRoles.setAllowance,
            (VaultRoles.deliveryAllowanceKey(cfg.originToken), uint128(held + 1), 0, 0, 0, 0)
        );
        assertTrue(ownersExec(d.roles, raise, Enum.Operation.Call, 2));
        vm.recordLogs();
        (bool ok,) = deliver(BENEFICIARY, held + 1, keccak256("too much"));
        assertFalse(ok);
        assertEq(count(vm.getRecordedLogs(), d.emitter, REFERENCE_TOPIC), 0);
        (,,, uint128 allowanceAfter,) = roles.allowances(VaultRoles.deliveryAllowanceKey(cfg.originToken));
        assertEq(allowanceAfter, uint128(held + 1));
        assertGt(allowanceBefore, 0);
    }

    /// AC-I1.1, REQ-KY-3: deliveries are bounded by the allowance in the window.
    function test_AC_I1_1_outflowBoundedByAllowance() public {
        uint256 cap = cfg.deliveryBalance;
        (bool ok, bytes memory ret) = deliver(BENEFICIARY, cap / 2, keccak256("a"));
        assertTrue(ok, string(ret));
        (ok, ret) = deliver(BENEFICIARY, cap - cap / 2 + 1, keccak256("b"));
        assertViolation(ok, ret, ALLOWANCE_EXCEEDED);
        (ok, ret) = deliver(BENEFICIARY, cap - cap / 2, keccak256("c"));
        assertTrue(ok, string(ret));
        (ok, ret) = deliver(BENEFICIARY, 1, keccak256("d"));
        assertViolation(ok, ret, ALLOWANCE_EXCEEDED);
    }

    /// AC-I1.1: the allowance refills per period, never above its maximum.
    function test_AC_I1_1_allowanceRefillsPerPeriod() public {
        (bool ok, bytes memory ret) = deliver(BENEFICIARY, cfg.deliveryBalance, keccak256("drain"));
        assertTrue(ok, string(ret));
        (ok, ret) = deliver(BENEFICIARY, 1, keccak256("empty"));
        assertViolation(ok, ret, ALLOWANCE_EXCEEDED);

        vm.warp(block.timestamp + cfg.deliveryPeriod - 1);
        (ok, ret) = deliver(BENEFICIARY, 1, keccak256("early"));
        assertViolation(ok, ret, ALLOWANCE_EXCEEDED);

        // Ten periods later the balance is capped at maxRefill, not ten refills.
        vm.warp(block.timestamp + 10 * uint256(cfg.deliveryPeriod));
        (ok, ret) = deliver(BENEFICIARY, uint256(cfg.deliveryMaxRefill) + 1, keccak256("over cap"));
        assertViolation(ok, ret, ALLOWANCE_EXCEEDED);
        (ok, ret) = deliver(BENEFICIARY, cfg.deliveryMaxRefill, keccak256("refilled"));
        assertTrue(ok, string(ret));
    }

    /// REQ-RN-5: a transfer without the emitter call is refused, in every form.
    function test_REQ_RN_5_transferWithoutReferenceRefused() public {
        bytes memory t = Batches.entry(0, cfg.originToken, 0, Batches.transferData(BENEFICIARY, 1));
        bytes memory e = Batches.entry(0, d.emitter, 0, Batches.emitData(keccak256("r")));
        Enum.Operation dc = Enum.Operation.DelegateCall;
        bytes32 role = VaultRoles.DELIVERY;

        // Bare transfer: the unwrapper registered at (token, transfer) refuses it.
        (bool ok, bytes memory ret) = roleCall(
            delivery, role, cfg.originToken, 0, Batches.transferData(BENEFICIARY, 1), Enum.Operation.Call
        );
        assertRevertSelector(ok, ret, MALFORMED_MULTI_ENTRYPOINT);
        // A batch of the transfer alone, of two transfers, reversed, or with a third call.
        (ok, ret) = roleCall(delivery, role, cfg.multiSendCallOnly, 0, Batches.multiSend(t), dc);
        assertRevertSelector(ok, ret, MALFORMED_MULTI_ENTRYPOINT);
        (ok, ret) =
            roleCall(delivery, role, cfg.multiSendCallOnly, 0, Batches.multiSend(bytes.concat(t, t)), dc);
        assertRevertSelector(ok, ret, MALFORMED_MULTI_ENTRYPOINT);
        (ok, ret) =
            roleCall(delivery, role, cfg.multiSendCallOnly, 0, Batches.multiSend(bytes.concat(e, t)), dc);
        assertRevertSelector(ok, ret, MALFORMED_MULTI_ENTRYPOINT);
        (ok, ret) =
            roleCall(delivery, role, cfg.multiSendCallOnly, 0, Batches.multiSend(bytes.concat(t, e, t)), dc);
        assertRevertSelector(ok, ret, MALFORMED_MULTI_ENTRYPOINT);
        assertEq(balance(cfg.originToken, BENEFICIARY), 0);
    }

    /// REQ-KY-3: only the origin token. A referenced batch moving any other token is refused.
    function test_REQ_KY_3_deliveryMovesOnlyTheOriginToken() public {
        address other = cfg.convertibles.length > 0 ? cfg.convertibles[0].token : address(0xdead);
        if (cfg.convertibles.length > 0) fund(other, 1e6);
        (bool ok, bytes memory ret) = roleCall(
            delivery,
            VaultRoles.DELIVERY,
            cfg.multiSendCallOnly,
            0,
            Batches.delivery(other, BENEFICIARY, 1, d.emitter, keccak256("other")),
            Enum.Operation.DelegateCall
        );
        assertViolation(ok, ret, TARGET_NOT_ALLOWED);
    }

    /// REQ-KY-3: the emitter alone moves nothing, and a reference may be emitted alone (it binds no value).
    function test_REQ_KY_3_referenceAloneMovesNothing() public {
        uint256 held = balance(cfg.originToken, d.safe);
        (bool ok, bytes memory ret) = roleCall(
            delivery,
            VaultRoles.DELIVERY,
            d.emitter,
            0,
            Batches.emitData(keccak256("lonely")),
            Enum.Operation.Call
        );
        assertTrue(ok, string(ret));
        assertEq(balance(cfg.originToken, d.safe), held);
    }
}

contract PolygonDeliveryTest is DeliveryTest {
    function network() internal pure override returns (string memory) {
        return "polygon";
    }
}

contract BaseDeliveryTest is DeliveryTest {
    function network() internal pure override returns (string memory) {
        return "base";
    }
}

contract BscDeliveryTest is DeliveryTest {
    function network() internal pure override returns (string memory) {
        return "bsc";
    }
}
