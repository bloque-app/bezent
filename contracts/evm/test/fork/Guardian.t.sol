// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {Enum} from "@gnosis.pm/safe-contracts/contracts/common/Enum.sol";
import {ExecutionOptions} from "zodiac-roles/Types.sol";

import {
    VaultGuard,
    MIN_COOLDOWN,
    MAX_COOLDOWN,
    MIN_EXPIRATION,
    MAX_EXPIRATION
} from "../../src/VaultGuard.sol";
import {VaultRoles} from "../../src/VaultRoles.sol";
import {ISafe, IRoles, IDelay, IERC20} from "../../src/interfaces/External.sol";
import {Batches} from "../Batches.sol";
import {GovernanceBase} from "./GovernanceBase.sol";

/// F-008, review of PR 41 (findings 1, 2 and 3): the guardian, an independent defender that is neither an owner
/// nor the delivery key (REQ-GV-2, REQ-KY-4), and the guard's refusal of queued calls that would lock the
/// owners out. The guardian holds a role in a second Roles modifier, enabled as a Safe module and owned by the
/// Safe, scoped to `Delay.setTxNonce` (veto) and `Safe.disableModule(prev, roles)` (pause).
abstract contract GuardianTest is GovernanceBase {
    // Roles v2.1.1 Modifier: the caller is not an enabled module of this instance.
    bytes4 internal constant NOT_AUTHORIZED = bytes4(keccak256("NotAuthorized(address)"));

    string internal constant IN_THE_QUEUE = "VaultGuard: the guardian's powers change through the queue only";

    function _veto() internal returns (bool ok, bytes memory ret) {
        return guardianCall(
            d.delay, 0, abi.encodeCall(IDelay.setTxNonce, (delayMod.queueNonce())), Enum.Operation.Call
        );
    }

    function _pause() internal returns (bool ok, bytes memory ret) {
        return guardianCall(
            d.safe, 0, abi.encodeCall(ISafe.disableModule, (prevOf(d.roles), d.roles)), Enum.Operation.Call
        );
    }

    // ------------------------------------------------------------------ what the guardian can do

    /// REQ-GV-2, review finding 1: a compromised quorum queues the removal of the guard. One honest owner cannot
    /// skip it (a Safe transaction needs the threshold); the guardian, alone, can. Without the veto the removal
    /// runs (the control).
    function test_REQ_GV_2_guardianVetoesAQueuedGuardRemoval() public {
        bytes memory remove = abi.encodeCall(ISafe.setGuard, (address(0)));
        mustQueue(d.safe, remove);
        (bool ok,) = ownersTry(d.delay, abi.encodeCall(IDelay.setTxNonce, (1)), Enum.Operation.Call, 1);
        assertFalse(ok, "one owner cannot veto");

        bool vetoed;
        (vetoed,) = _veto();
        assertTrue(vetoed, "the guardian vetoes alone");
        assertEq(delayMod.txNonce(), 1);
        vm.warp(block.timestamp + cooldown);
        (ok,) = runQueued(d.safe, remove);
        assertFalse(ok, "the vetoed removal does not run");
        assertEq(guardSlot(), d.guard, "the guard stands");

        // The control: the same removal, not vetoed, runs after the cooldown.
        mustQueue(d.safe, remove);
        vm.warp(block.timestamp + cooldown);
        mustRun(d.safe, remove);
        assertEq(guardSlot(), address(0), "removed when nobody vetoes");
    }

    /// REQ-GV-2: the guardian pauses the vault alone: deliveries stop at once. Unpausing is not its power, and
    /// not an instant power of the owners either: it is queued, and waits out the cooldown.
    function test_REQ_GV_2_guardianPausesAlone() public {
        (bool ok, bytes memory ret) = deliver(BENEFICIARY, 1, keccak256("before"));
        assertTrue(ok, string(ret));

        (ok, ret) = _pause();
        assertTrue(ok, string(ret));
        assertFalse(safe.isModuleEnabled(d.roles), "the delivery role's module is off");
        (ok,) = deliver(BENEFICIARY, 1, keccak256("paused"));
        assertFalse(ok, "paused");

        // The guardian cannot switch it back on; the owners cannot do it at once.
        (ok, ret) =
            guardianCall(d.safe, 0, abi.encodeCall(ISafe.enableModule, (d.roles)), Enum.Operation.Call);
        assertViolation(ok, ret, FUNCTION_NOT_ALLOWED);
        bytes memory enable = abi.encodeCall(ISafe.enableModule, (d.roles));
        assertGuardRefuses(d.safe, enable, "VaultGuard: Safe change must be delayed");
        mustQueue(d.safe, enable);
        vm.warp(block.timestamp + cooldown);
        mustRun(d.safe, enable);
        (ok, ret) = deliver(BENEFICIARY, 1, keccak256("resumed"));
        assertTrue(ok, string(ret));
    }

    /// REQ-GV-2: the pause does not take the veto with it (the guardian's module is not the paused one). A paused vault whose unpause is queued can still have it vetoed.
    function test_REQ_GV_2_pauseThenVetoStillWorks() public {
        (bool ok,) = _pause();
        assertTrue(ok);
        bytes memory enable = abi.encodeCall(ISafe.enableModule, (d.roles));
        mustQueue(d.safe, enable);
        (ok,) = _veto();
        assertTrue(ok, "the veto after the pause");
        vm.warp(block.timestamp + cooldown);
        (ok,) = runQueued(d.safe, enable);
        assertFalse(ok, "the vetoed unpause does not run");
        assertFalse(safe.isModuleEnabled(d.roles), "still paused");
        assertTrue(safe.isModuleEnabled(d.guardianRoles), "the guardian's module is untouched");
    }

    // ------------------------------------------------------------------ what the guardian cannot do

    /// REQ-GV-2, REQ-GV-5, REQ-KY-3: the guardian cannot move value, queue anything, widen anything, remove the
    /// Delay, itself or the guard, or change a Roles modifier; it has no value and no delegatecall. Each is
    /// refused by its Roles scope, and nothing about the vault changes.
    function test_REQ_GV_2_guardianCannotMoveValueOrWiden() public {
        uint256 amount = uint256(cfg.deliveryBalance);
        bytes memory transfer = Batches.transferData(BENEFICIARY, amount);
        Enum.Operation call_ = Enum.Operation.Call;
        bool ok;
        bytes memory ret;

        // Value: the vault's tokens, by transfer, approval and the delivery batch; native value.
        (ok, ret) = guardianCall(cfg.originToken, 0, transfer, call_);
        assertViolation(ok, ret, TARGET_NOT_ALLOWED);
        (ok, ret) =
            guardianCall(cfg.originToken, 0, abi.encodeCall(IERC20.approve, (BENEFICIARY, amount)), call_);
        assertViolation(ok, ret, TARGET_NOT_ALLOWED);
        (ok, ret) = guardianCall(
            cfg.multiSendCallOnly,
            0,
            Batches.delivery(cfg.originToken, BENEFICIARY, 1, d.emitter, keccak256("g")),
            Enum.Operation.DelegateCall
        );
        assertViolation(ok, ret, TARGET_NOT_ALLOWED);
        (ok, ret) = guardianCall(BENEFICIARY, 1, "", call_);
        assertViolation(ok, ret, TARGET_NOT_ALLOWED);
        (ok, ret) = guardianCall(d.delay, 1, abi.encodeCall(IDelay.setTxNonce, (1)), call_);
        assertViolation(ok, ret, SEND_NOT_ALLOWED);
        (ok, ret) =
            guardianCall(d.delay, 0, abi.encodeCall(IDelay.setTxNonce, (1)), Enum.Operation.DelegateCall);
        assertViolation(ok, ret, DELEGATECALL_NOT_ALLOWED);

        // The queue: a queued transfer would reach the Delay as the Safe, unseen by the guard.
        (ok, ret) = guardianCall(
            d.delay,
            0,
            abi.encodeCall(IDelay.execTransactionFromModule, (cfg.originToken, 0, transfer, call_)),
            call_
        );
        assertViolation(ok, ret, FUNCTION_NOT_ALLOWED);
        (ok, ret) = guardianCall(
            d.delay,
            0,
            abi.encodeCall(IDelay.execTransactionFromModuleReturnData, (cfg.originToken, 0, transfer, call_)),
            call_
        );
        assertViolation(ok, ret, FUNCTION_NOT_ALLOWED);
        // The rest of the Delay.
        bytes[4] memory delayCalls = [
            abi.encodeCall(IDelay.setTxCooldown, (MIN_COOLDOWN)),
            abi.encodeCall(IDelay.setTxExpiration, (MAX_EXPIRATION)),
            abi.encodeCall(IDelay.enableModule, (outsider)),
            abi.encodeCall(IDelay.transferOwnership, (outsider))
        ];
        for (uint256 i; i < delayCalls.length; ++i) {
            (ok, ret) = guardianCall(d.delay, 0, delayCalls[i], call_);
            assertViolation(ok, ret, FUNCTION_NOT_ALLOWED);
        }

        // The Safe: everything but the pause (a widening, an unpause, the guard, the owners).
        bytes[6] memory safeCalls = [
            abi.encodeCall(ISafe.enableModule, (outsider)),
            abi.encodeCall(ISafe.enableModule, (d.roles)),
            abi.encodeCall(ISafe.setGuard, (address(0))),
            abi.encodeCall(ISafe.addOwnerWithThreshold, (outsider, 1)),
            abi.encodeWithSelector(bytes4(keccak256("setFallbackHandler(address)")), FALLBACK_HANDLER_141),
            abi.encodeWithSignature(
                "execTransactionFromModule(address,uint256,bytes,uint8)",
                cfg.originToken,
                0,
                transfer,
                uint8(0)
            )
        ];
        for (uint256 i; i < safeCalls.length; ++i) {
            (ok, ret) = guardianCall(d.safe, 0, safeCalls[i], call_);
            assertViolation(ok, ret, FUNCTION_NOT_ALLOWED);
        }
        // The pause, aimed at anything but the delivery role's module: the Delay, itself, another address.
        address[3] memory notTheDeliveryModule = [d.delay, d.guardianRoles, outsider];
        for (uint256 i; i < notTheDeliveryModule.length; ++i) {
            (ok, ret) = guardianCall(
                d.safe,
                0,
                abi.encodeCall(ISafe.disableModule, (prevOf(d.delay), notTheDeliveryModule[i])),
                call_
            );
            assertViolation(ok, ret, PARAMETER_NOT_ALLOWED);
        }

        // Both Roles modifiers: not targets of the role, so neither can be reconfigured through the Safe.
        bytes[2] memory rolesCalls = [
            abi.encodeCall(IRoles.allowTarget, (VaultRoles.GUARDIAN, cfg.originToken, ExecutionOptions.Both)),
            abi.encodeCall(IRoles.assignRoles, (outsider, _one(VaultRoles.DELIVERY), _one(true)))
        ];
        for (uint256 i; i < rolesCalls.length; ++i) {
            (ok, ret) = guardianCall(d.guardianRoles, 0, rolesCalls[i], call_);
            assertViolation(ok, ret, TARGET_NOT_ALLOWED);
            (ok, ret) = guardianCall(d.roles, 0, rolesCalls[i], call_);
            assertViolation(ok, ret, TARGET_NOT_ALLOWED);
            // Nor directly: the guardian is a member, not the owner.
            vm.prank(guardian);
            (ok,) = d.guardianRoles.call(rolesCalls[i]);
            assertFalse(ok, "the guardian does not own its Roles modifier");
        }

        // It is not a member of any other role, nor does the delivery key hold the guardian's.
        (ok, ret) = guardianAs(guardian, VaultRoles.DELIVERY, cfg.originToken, 0, transfer, call_);
        assertRevertSelector(ok, ret, NO_MEMBERSHIP);
        (ok, ret) = guardianAs(
            delivery, VaultRoles.GUARDIAN, d.delay, 0, abi.encodeCall(IDelay.setTxNonce, (1)), call_
        );
        assertRevertSelector(ok, ret, NOT_AUTHORIZED); // not even a module of the guardian's Roles modifier
        // Without a role the module entry point is closed.
        vm.prank(guardian);
        (ok,) = d.guardianRoles
            .call(abi.encodeCall(IRoles.execTransactionFromModule, (cfg.originToken, 0, transfer, call_)));
        assertFalse(ok);

        // Nothing moved, nothing changed.
        assertEq(balance(cfg.originToken, BENEFICIARY), 0);
        assertEq(guardSlot(), d.guard);
        assertEq(modulesOfTheSafe(), 3);
        assertEq(safe.getOwners().length, 3);
        assertEq(delayMod.queueNonce(), 0);
        assertEq(delayMod.txCooldown(), cooldown);
        assertTrue(safe.isModuleEnabled(d.roles) && safe.isModuleEnabled(d.delay));
        assertEq(safe.nonce(), 0);
    }

    // ------------------------------------------------------------------ the owners and the guardian

    /// REQ-GV-2, review finding 1: the quorum cannot remove, rescope or reconfigure the guardian at once. Every
    /// direct owner transaction that touches the guardian's Roles modifier or its module is refused, narrowing
    /// ones included (they would let a compromised quorum strip the defender before attacking).
    function test_REQ_GV_2_ownersCannotRemoveTheGuardianAtOnce() public {
        assertGuardRefuses(
            d.safe,
            abi.encodeCall(ISafe.disableModule, (prevOf(d.guardianRoles), d.guardianRoles)),
            "VaultGuard: only the Roles modifier may be disabled at once"
        );
        bytes[8] memory touches = [
            abi.encodeCall(IRoles.assignRoles, (guardian, _one(VaultRoles.GUARDIAN), _one(false))),
            abi.encodeCall(IRoles.disableModule, (address(0x1), guardian)),
            abi.encodeCall(IRoles.revokeTarget, (VaultRoles.GUARDIAN, d.delay)),
            abi.encodeCall(IRoles.revokeFunction, (VaultRoles.GUARDIAN, d.delay, IDelay.setTxNonce.selector)),
            abi.encodeCall(IRoles.setAvatar, (outsider)),
            abi.encodeCall(IRoles.setTarget, (outsider)),
            abi.encodeCall(IRoles.transferOwnership, (outsider)),
            abi.encodeCall(IRoles.allowTarget, (VaultRoles.GUARDIAN, cfg.originToken, ExecutionOptions.Both))
        ];
        for (uint256 i; i < touches.length; ++i) {
            assertGuardRefuses(d.guardianRoles, touches[i], IN_THE_QUEUE);
        }
        // The owners' own immediate pause does not reach the guardian either: only the delivery module.
        assertTrue(
            ownersExec(
                d.safe,
                abi.encodeCall(ISafe.disableModule, (prevOf(d.roles), d.roles)),
                Enum.Operation.Call,
                2
            )
        );
        assertTrue(safe.isModuleEnabled(d.guardianRoles));
        (bool ok,) = _veto(); // nothing queued: the call is the guardian's, and the Delay says so
        assertFalse(ok);
        assertTrue(guardianMod.isModuleEnabled(guardian));
        assertEq(guardianMod.owner(), d.safe);
    }

    /// REQ-GV-2, REQ-GV-3: removing the guardian is a delayed change, not a forbidden one: queued, it runs after
    /// the cooldown if nobody vetoes. The guardian can veto its own removal; that is the cost of an independent
    /// defender (it can freeze governance), stated in NOTES.md.
    function test_REQ_GV_2_removingTheGuardianIsDelayedAndVetoable() public {
        bytes memory remove = abi.encodeCall(ISafe.disableModule, (prevOf(d.guardianRoles), d.guardianRoles));
        mustQueue(d.safe, remove);
        (bool ok, bytes memory ret) = runQueued(d.safe, remove);
        assertFalse(ok);
        assertEq(ret, reason("Transaction is still in cooldown"));
        assertTrue(safe.isModuleEnabled(d.guardianRoles), "still a module during the cooldown");

        // The guardian vetoes its removal.
        (ok,) = _veto();
        assertTrue(ok);
        vm.warp(block.timestamp + cooldown);
        (ok,) = runQueued(d.safe, remove);
        assertFalse(ok, "vetoed");
        assertTrue(safe.isModuleEnabled(d.guardianRoles));

        // Not vetoed (here: the guardian is away), it goes.
        mustQueue(d.safe, remove);
        vm.warp(block.timestamp + cooldown);
        mustRun(d.safe, remove);
        assertFalse(safe.isModuleEnabled(d.guardianRoles), "removed after the cooldown");
        (ok, ret) = _pause();
        assertFalse(ok, "a removed guardian can do nothing");
    }

    /// REQ-GV-2: rotating the guardian is a delayed change as well (the old member out, the new one in), and
    /// so is changing its scope: nothing changes before the cooldown has passed.
    function test_REQ_GV_2_rotatingAndRescopingTheGuardianIsDelayed() public {
        address successor = outsider;
        bytes memory out =
            abi.encodeCall(IRoles.assignRoles, (guardian, _one(VaultRoles.GUARDIAN), _one(false)));
        bytes memory inn =
            abi.encodeCall(IRoles.assignRoles, (successor, _one(VaultRoles.GUARDIAN), _one(true)));
        bytes memory widen =
            abi.encodeCall(IRoles.allowTarget, (VaultRoles.GUARDIAN, cfg.originToken, ExecutionOptions.Both));
        mustQueue(d.guardianRoles, inn);
        mustQueue(d.guardianRoles, out);
        mustQueue(d.guardianRoles, widen);

        // Before the cooldown: the guardian is as it was, and cannot touch the token.
        (bool ok, bytes memory ret) =
            guardianCall(cfg.originToken, 0, Batches.transferData(BENEFICIARY, 1), Enum.Operation.Call);
        assertViolation(ok, ret, TARGET_NOT_ALLOWED);

        vm.warp(block.timestamp + cooldown);
        mustRun(d.guardianRoles, inn);
        mustRun(d.guardianRoles, out);
        (ok,) = _pause();
        assertFalse(ok, "the old guardian is out");
        (ok,) = guardianAs(
            successor,
            VaultRoles.GUARDIAN,
            d.safe,
            0,
            abi.encodeCall(ISafe.disableModule, (prevOf(d.roles), d.roles)),
            Enum.Operation.Call
        );
        assertTrue(ok, "the new guardian pauses");
    }

    // ------------------------------------------------------------------ review finding 2: no lock-out

    /// REQ-GV-3, review finding 2: a queued call that would lock the owners out for good is refused when it is
    /// queued. `Delay.setAvatar` is the review's case (every owner transaction then fails the guard's check that
    /// the Delay's avatar is the Safe, a pause and a veto included); the others break governance the same way, or
    /// add a Delay module that queues without the guard seeing it.
    function test_REQ_GV_3_queuedCallsThatLockTheOwnersOutAreRefused() public {
        string memory notOffered = "VaultGuard: queued Delay call not offered";
        bytes[9] memory refused = [
            abi.encodeCall(IDelay.setAvatar, (outsider)),
            abi.encodeCall(IDelay.setTarget, (outsider)),
            abi.encodeCall(IDelay.transferOwnership, (outsider)),
            abi.encodeCall(IDelay.disableModule, (SENTINEL, d.safe)),
            abi.encodeCall(IDelay.enableModule, (outsider)),
            abi.encodeCall(IDelay.setTxNonce, (1)),
            abi.encodeCall(IDelay.setUp, ("")),
            abi.encodeCall(IDelay.skipExpired, ()),
            hex"deadbeef"
        ];
        for (uint256 i; i < refused.length; ++i) {
            (bool ok, bytes memory ret) = queue(d.delay, refused[i]);
            assertFalse(ok);
            assertEq(ret, reason(notOffered));
        }
        // Taking the Delay off the Safe is the same lock-out.
        (bool ok2, bytes memory ret2) =
            queue(d.safe, abi.encodeCall(ISafe.disableModule, (prevOf(d.delay), d.delay)));
        assertFalse(ok2);
        assertEq(ret2, reason("VaultGuard: queued call disables the Delay"));
        assertEq(delayMod.queueNonce(), 0, "nothing was queued");
        assertEq(delayMod.avatar(), d.safe);

        // The owners still govern: the pause, the veto and the queue all work.
        assertTrue(
            ownersExec(
                d.safe,
                abi.encodeCall(ISafe.disableModule, (prevOf(d.roles), d.roles)),
                Enum.Operation.Call,
                2
            )
        );
        mustQueue(d.roles, lowered());
    }

    /// REQ-GV-3: the same refusal holds for a non-canonical encoding of the call. Safe 1.4.1 is compiled with
    /// solc 0.7.6 (ABI coder v1), which masks the high bits of an address argument instead of reverting, so a
    /// module word with dirty high bits still names the Delay to the Safe. The guard compares the address, not the word.
    function test_REQ_GV_3_removingTheDelayWithDirtyAddressBitsIsRefused() public {
        bytes memory dirty = abi.encodePacked(
            ISafe.disableModule.selector,
            bytes32(uint256(uint160(prevOf(d.delay)))),
            bytes32(uint256(uint160(d.delay)) | (uint256(1) << 255))
        );
        (bool ok, bytes memory ret) = queue(d.safe, dirty);
        assertFalse(ok);
        assertEq(ret, reason("VaultGuard: queued call disables the Delay"));
        assertEq(delayMod.queueNonce(), 0, "nothing was queued");

        // The control: with no guard, the dirty encoding does take the Delay off the Safe.
        vm.store(d.safe, bytes32(GUARD_SLOT), bytes32(0));
        mustQueue(d.safe, dirty);
        vm.warp(block.timestamp + cooldown);
        mustRun(d.safe, dirty);
        assertFalse(safe.isModuleEnabled(d.delay), "the Safe read the dirty word as the Delay");
    }

    /// REQ-GV-3, review findings 2 and 3: a queued change of the cooldown or the expiration is accepted only
    /// within the bounds the deployment also enforces: no zero, no cooldown below the floor, none above the cap,
    /// and no expiration of 0 (a queue that never clears).
    function test_REQ_GV_3_queuedCooldownAndExpirationStayWithinBounds() public {
        string memory cooldownBound = "VaultGuard: cooldown out of bounds";
        string memory expirationBound = "VaultGuard: expiration out of bounds";
        uint256[4] memory badCooldowns = [uint256(0), MIN_COOLDOWN - 1, MAX_COOLDOWN + 1, type(uint256).max];
        for (uint256 i; i < badCooldowns.length; ++i) {
            (bool ok, bytes memory ret) =
                queue(d.delay, abi.encodeCall(IDelay.setTxCooldown, (badCooldowns[i])));
            assertFalse(ok);
            assertEq(ret, reason(cooldownBound));
        }
        uint256[4] memory badExpirations =
            [uint256(0), MIN_EXPIRATION - 1, MAX_EXPIRATION + 1, type(uint256).max];
        for (uint256 i; i < badExpirations.length; ++i) {
            (bool ok, bytes memory ret) =
                queue(d.delay, abi.encodeCall(IDelay.setTxExpiration, (badExpirations[i])));
            assertFalse(ok);
            assertEq(ret, reason(expirationBound));
        }
        assertEq(delayMod.queueNonce(), 0);

        // The bounds themselves are fine.
        mustQueue(d.delay, abi.encodeCall(IDelay.setTxCooldown, (MIN_COOLDOWN)));
        mustQueue(d.delay, abi.encodeCall(IDelay.setTxCooldown, (MAX_COOLDOWN)));
        mustQueue(d.delay, abi.encodeCall(IDelay.setTxExpiration, (MIN_EXPIRATION)));
        mustQueue(d.delay, abi.encodeCall(IDelay.setTxExpiration, (MAX_EXPIRATION)));
        assertEq(delayMod.queueNonce(), 4);

        // And they are the ones the deployment holds the recommended values to.
        assertGe(cooldown, MIN_COOLDOWN);
        assertLe(cooldown, MAX_COOLDOWN);
        assertGe(expiration, MIN_EXPIRATION);
        assertLe(expiration, MAX_EXPIRATION);
    }

    /// The guard's address constructor: three different modifiers.
    function test_REQ_GV_5_guardNamesThreeDistinctModifiers() public {
        vm.expectRevert(bytes("VaultGuard: one modifier twice"));
        new VaultGuard(d.roles, d.delay, d.roles);
        vm.expectRevert(bytes("VaultGuard: one modifier twice"));
        new VaultGuard(d.roles, d.delay, d.delay);
        vm.expectRevert(bytes("VaultGuard: zero address"));
        new VaultGuard(d.roles, d.delay, address(0));
        assertEq(VaultGuard(d.guard).guardianRoles(), d.guardianRoles);
    }
}

contract PolygonGuardianTest is GuardianTest {
    function network() internal pure override returns (string memory) {
        return "polygon";
    }
}

contract BaseGuardianTest is GuardianTest {
    function network() internal pure override returns (string memory) {
        return "base";
    }
}

contract BscGuardianTest is GuardianTest {
    function network() internal pure override returns (string memory) {
        return "bsc";
    }
}
