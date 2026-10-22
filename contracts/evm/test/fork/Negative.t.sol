// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {Enum} from "@gnosis.pm/safe-contracts/contracts/common/Enum.sol";

import {VaultRoles} from "../../src/VaultRoles.sol";
import {ISafe, IRoles, IDelay} from "../../src/interfaces/External.sol";
import {Batches} from "../Batches.sol";
import {VaultFork} from "./VaultFork.sol";

/// T-008-06: neither role can change roles, add modules, delegatecall anything but the referenced batch,
/// send value, or reach any other target; only the Safe's owners, at its threshold, change the roles.
abstract contract NegativeTest is VaultFork {
    address internal constant MULTISEND_141 = 0x38869bf66a61cF6bDB996A6aE40D5853Fd43B526; // delegatecall-capable

    function setUp() public override {
        super.setUp();
        fund(cfg.originToken, uint256(cfg.deliveryBalance) * 2);
        vm.deal(d.safe, 1 ether);
    }

    function _members() internal view returns (address[] memory m, bytes32[] memory r) {
        bool conv = cfg.convertibles.length > 0;
        m = new address[](conv ? 2 : 1);
        r = new bytes32[](m.length);
        m[0] = delivery;
        r[0] = VaultRoles.DELIVERY;
        if (conv) {
            m[1] = conversion;
            r[1] = VaultRoles.CONVERSION;
        }
    }

    /// REQ-KY-4: no role member can change a role, an allowance, an unwrapper, the avatar or the owner,
    /// whether calling the Roles modifier directly or through the vault.
    function test_REQ_KY_4_rolesCannotChangeRoles() public {
        (address[] memory m, bytes32[] memory r) = _members();
        bytes32[] memory keys = new bytes32[](1);
        keys[0] = VaultRoles.DELIVERY;
        bool[] memory yes = new bool[](1);
        yes[0] = true;
        bytes[6] memory admin = [
            abi.encodeCall(IRoles.assignRoles, (outsider, keys, yes)),
            abi.encodeCall(IRoles.scopeTarget, (VaultRoles.DELIVERY, outsider)),
            abi.encodeCall(
                IRoles.setAllowance,
                (VaultRoles.deliveryAllowanceKey(cfg.originToken), type(uint128).max, 0, 0, 0, 0)
            ),
            abi.encodeCall(IRoles.setAvatar, (outsider)),
            abi.encodeCall(IRoles.transferOwnership, (outsider)),
            abi.encodeWithSelector(
                bytes4(keccak256("setTransactionUnwrapper(address,bytes4,address)")),
                cfg.originToken,
                VaultRoles.TRANSFER,
                address(0)
            )
        ];
        for (uint256 i; i < m.length; ++i) {
            for (uint256 j; j < admin.length; ++j) {
                vm.prank(m[i]);
                (bool ok,) = d.roles.call(admin[j]);
                assertFalse(ok, "direct admin call");
                (bool ok2, bytes memory ret) = roleCall(m[i], r[i], d.roles, 0, admin[j], Enum.Operation.Call);
                assertViolation(ok2, ret, TARGET_NOT_ALLOWED);
            }
        }
        assertEq(roles.owner(), d.safe);
    }

    /// REQ-KY-4: no role member can add a module, to the Safe or to the Roles modifier, or swap an owner.
    function test_REQ_KY_4_rolesCannotAddModules() public {
        (address[] memory m, bytes32[] memory r) = _members();
        bytes[3] memory safeAdmin = [
            abi.encodeCall(ISafe.enableModule, (outsider)),
            abi.encodeCall(ISafe.addOwnerWithThreshold, (outsider, 1)),
            abi.encodeWithSelector(bytes4(keccak256("setGuard(address)")), outsider)
        ];
        for (uint256 i; i < m.length; ++i) {
            for (uint256 j; j < safeAdmin.length; ++j) {
                (bool ok, bytes memory ret) =
                    roleCall(m[i], r[i], d.safe, 0, safeAdmin[j], Enum.Operation.Call);
                assertViolation(ok, ret, TARGET_NOT_ALLOWED);
            }
            (bool ok3, bytes memory ret3) = roleCall(
                m[i], r[i], d.roles, 0, abi.encodeCall(IRoles.enableModule, (outsider)), Enum.Operation.Call
            );
            assertViolation(ok3, ret3, TARGET_NOT_ALLOWED);
            vm.prank(m[i]);
            (bool ok4,) = d.roles.call(abi.encodeCall(IRoles.enableModule, (outsider)));
            assertFalse(ok4);
            // Not a module of the Safe: it cannot bypass the Roles modifier.
            vm.prank(m[i]);
            (bool ok5,) = d.safe
                .call(
                    abi.encodeWithSignature(
                        "execTransactionFromModule(address,uint256,bytes,uint8)", outsider, 0, "", uint8(0)
                    )
                );
            assertFalse(ok5);
        }
        // The Roles modifier, the Delay and the guardian's Roles modifier, and no other.
        (address[] memory modules,) = safe.getModulesPaginated(address(0x1), 10);
        assertEq(modules.length, 3);
        assertEq(safe.getOwners().length, owners.length);
    }

    /// REQ-KY-4: no delegatecall but the referenced MultiSendCallOnly batch.
    function test_REQ_KY_4_noDelegatecallButTheReferencedBatch() public {
        bytes memory pair = Batches.delivery(cfg.originToken, outsider, 1, d.emitter, keccak256("r"));
        bytes32 role = VaultRoles.DELIVERY;
        Enum.Operation dc = Enum.Operation.DelegateCall;

        // The delegatecall-capable MultiSend, with the same pair.
        (bool ok, bytes memory ret) = roleCall(delivery, role, MULTISEND_141, 0, pair, dc);
        assertViolation(ok, ret, TARGET_NOT_ALLOWED);
        // Delegatecall to the emitter, to the token, to anything else.
        (ok, ret) = roleCall(delivery, role, d.emitter, 0, Batches.emitData(keccak256("r")), dc);
        assertViolation(ok, ret, DELEGATECALL_NOT_ALLOWED);
        (ok, ret) = roleCall(delivery, role, cfg.originToken, 0, Batches.transferData(outsider, 1), dc);
        assertRevertSelector(ok, ret, MALFORMED_MULTI_ENTRYPOINT);
        (ok, ret) = roleCall(delivery, role, outsider, 0, "", dc);
        assertViolation(ok, ret, TARGET_NOT_ALLOWED);
        // MultiSendCallOnly as a plain call, and a batch with an inner delegatecall.
        (ok, ret) = roleCall(delivery, role, cfg.multiSendCallOnly, 0, pair, Enum.Operation.Call);
        assertRevertSelector(ok, ret, MALFORMED_MULTI_ENTRYPOINT);
        bytes memory inner = Batches.multiSend(
            bytes.concat(
                Batches.entry(1, cfg.originToken, 0, Batches.transferData(outsider, 1)),
                Batches.entry(0, d.emitter, 0, Batches.emitData(keccak256("r")))
            )
        );
        (ok, ret) = roleCall(delivery, role, cfg.multiSendCallOnly, 0, inner, dc);
        assertRevertSelector(ok, ret, MALFORMED_MULTI_ENTRYPOINT);
    }

    /// REQ-KY-4: no native value leaves the vault under a role.
    function test_REQ_KY_4_noValue() public {
        (bool ok, bytes memory ret) = roleCall(
            delivery, VaultRoles.DELIVERY, d.emitter, 1, Batches.emitData(keccak256("r")), Enum.Operation.Call
        );
        assertViolation(ok, ret, SEND_NOT_ALLOWED);
        (ok, ret) = roleCall(delivery, VaultRoles.DELIVERY, outsider, 1, "", Enum.Operation.Call);
        assertViolation(ok, ret, TARGET_NOT_ALLOWED);
        bytes memory pair = Batches.delivery(cfg.originToken, outsider, 1, d.emitter, keccak256("r"));
        (ok, ret) = roleCall(
            delivery, VaultRoles.DELIVERY, cfg.multiSendCallOnly, 1, pair, Enum.Operation.DelegateCall
        );
        assertRevertSelector(ok, ret, MALFORMED_MULTI_ENTRYPOINT);
        assertEq(d.safe.balance, 1 ether);
    }

    /// REQ-KY-4: roles are not interchangeable, there is no default role, and outsiders hold none.
    function test_REQ_KY_4_membershipIsPerRole() public {
        bytes memory pair = Batches.delivery(cfg.originToken, outsider, 1, d.emitter, keccak256("r"));
        Enum.Operation dc = Enum.Operation.DelegateCall;
        (bool ok, bytes memory ret) =
            roleCall(outsider, VaultRoles.DELIVERY, cfg.multiSendCallOnly, 0, pair, dc);
        // Not even a module of the Roles modifier.
        assertRevertSelector(ok, ret, bytes4(keccak256("NotAuthorized(address)")));
        // The conversion key is a member of its own role on Polygon only (DEC-13): elsewhere it is no module.
        if (cfg.convertibles.length > 0) {
            (ok, ret) = roleCall(conversion, VaultRoles.DELIVERY, cfg.multiSendCallOnly, 0, pair, dc);
            assertRevertSelector(ok, ret, NO_MEMBERSHIP);
        }
        (ok, ret) = roleCall(delivery, VaultRoles.CONVERSION, cfg.multiSendCallOnly, 0, pair, dc);
        assertRevertSelector(ok, ret, NO_MEMBERSHIP);
        vm.prank(delivery);
        (ok, ret) = d.roles
            .call(abi.encodeCall(IRoles.execTransactionFromModule, (cfg.multiSendCallOnly, 0, pair, dc)));
        assertRevertSelector(ok, ret, NO_MEMBERSHIP);
    }

    /// REQ-GV-3: role changes pass the Safe's owners at its threshold, and then the Delay: fewer signatures
    /// are refused, a direct owner transaction is refused by the guard, and the queued change takes effect only
    /// after the cooldown (the guard's and the Delay's own suites are in Governance.t.sol).
    function test_REQ_GV_3_onlyOwnersAtThresholdChangeRoles() public {
        bytes32 key = VaultRoles.deliveryAllowanceKey(cfg.originToken);
        uint128 raised = cfg.deliveryBalance * 2;
        bytes memory raise = abi.encodeCall(
            IRoles.setAllowance, (key, raised, raised, cfg.deliveryRefill, cfg.deliveryPeriod, 0)
        );
        bytes memory queued =
            abi.encodeCall(IDelay.execTransactionFromModule, (d.roles, 0, raise, Enum.Operation.Call));

        assertFalse(ownersExec(d.roles, raise, Enum.Operation.Call, 2), "directly, even at the threshold");
        assertFalse(ownersExec(d.delay, queued, Enum.Operation.Call, 1), "queued with one signature");
        assertTrue(ownersExec(d.delay, queued, Enum.Operation.Call, 2), "queued at the threshold");
        (,,, uint128 bal,) = roles.allowances(key);
        assertEq(bal, cfg.deliveryBalance, "not before the cooldown");

        vm.warp(block.timestamp + delayMod.txCooldown());
        delayMod.executeNextTx(d.roles, 0, raise, Enum.Operation.Call);
        (,,, bal,) = roles.allowances(key);
        assertEq(bal, raised);
    }
}

contract PolygonNegativeTest is NegativeTest {
    function network() internal pure override returns (string memory) {
        return "polygon";
    }
}

contract BaseNegativeTest is NegativeTest {
    function network() internal pure override returns (string memory) {
        return "base";
    }
}
