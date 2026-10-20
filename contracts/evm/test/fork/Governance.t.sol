// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {Enum} from "@gnosis.pm/safe-contracts/contracts/common/Enum.sol";
import {ExecutionOptions} from "zodiac-roles/Types.sol";

import {VaultGuard} from "../../src/VaultGuard.sol";
import {VaultRoles} from "../../src/VaultRoles.sol";
import {ISafe, IRoles, IDelay, IERC20} from "../../src/interfaces/External.sol";
import {Batches} from "../Batches.sol";
import {GovernanceBase} from "./GovernanceBase.sol";

/// T-008-07: the Safe guard and the Delay modifier (plan DEC-10; REQ-GV-3, REQ-GV-5, REQ-KY-3). The owners
/// cannot move the vault's tokens; everything that widens what the vault can do passes the Delay's cooldown;
/// narrowing is immediate; the delivery role pays within its bounds without any delay.
abstract contract GovernanceTest is GovernanceBase {
    // ------------------------------------------------------------------ REQ-GV-5: no owner path moves value

    /// REQ-GV-5, REQ-KY-3: the owners, at the threshold, cannot transfer or approve the vault's tokens, by a
    /// plain call, a delegatecall, a native value, or a gas refund (which makes the Safe pay out of any token it
    /// holds). Without the guard each of these works (the control), so the guard is what refuses them.
    function test_REQ_GV_5_ownersCannotMoveTheVaultsTokens() public {
        uint256 amount = uint256(cfg.deliveryBalance);
        bytes memory transfer = Batches.transferData(BENEFICIARY, amount);
        bytes memory approve = abi.encodeCall(IERC20.approve, (BENEFICIARY, amount));

        assertGuardRefuses(cfg.originToken, transfer, "VaultGuard: not an administrative target");
        assertGuardRefuses(cfg.originToken, approve, "VaultGuard: not an administrative target");
        assertGuardRefuses(BENEFICIARY, "", "VaultGuard: no selector");

        (bool ok, bytes memory ret) = ownersTry(cfg.originToken, transfer, Enum.Operation.DelegateCall, 2);
        assertFalse(ok);
        assertEq(ret, reason("VaultGuard: delegatecall"));
        (ok, ret) =
            ownersTx(BENEFICIARY, 1, hex"01020304", Enum.Operation.Call, 0, 0, address(0), address(0), 2);
        assertFalse(ok);
        assertEq(ret, reason("VaultGuard: value"));
        // The refund: a transaction that does nothing, paid for in the origin token.
        (ok, ret) = ownersTx(
            BENEFICIARY, 0, hex"01020304", Enum.Operation.Call, amount / 2, 1, cfg.originToken, BENEFICIARY, 2
        );
        assertFalse(ok);
        assertEq(ret, reason("VaultGuard: gas refund"));
        assertEq(balance(cfg.originToken, BENEFICIARY), 0, "nothing left the vault");

        // The control: with no guard, the same owners move the tokens, by transfer and by refund.
        vm.store(d.safe, bytes32(GUARD_SLOT), bytes32(0));
        assertTrue(
            ownersExec(cfg.originToken, transfer, Enum.Operation.Call, 2), "transfer without the guard"
        );
        assertEq(balance(cfg.originToken, BENEFICIARY), amount);
        (ok,) = ownersTx(
            BENEFICIARY, 0, hex"01020304", Enum.Operation.Call, amount / 2, 1, cfg.originToken, BENEFICIARY, 2
        );
        assertTrue(ok, "refund without the guard");
        assertGt(balance(cfg.originToken, BENEFICIARY), amount, "the refund paid out of the vault");
    }

    /// REQ-GV-5: the Delay cannot carry value either. What the owners may queue is a plain call, with no value,
    /// to the Delay, the Roles modifier or the Safe; the vault's tokens, a router and MultiSend are none of them.
    function test_REQ_GV_5_theQueueCarriesAdministrationOnly() public {
        bytes memory transfer = Batches.transferData(BENEFICIARY, 1);
        (bool ok, bytes memory ret) = queue(cfg.originToken, transfer);
        assertFalse(ok);
        assertEq(ret, reason("VaultGuard: queued call to a non-administrative target"));
        (ok, ret) =
            queue(cfg.multiSendCallOnly, Batches.delivery(cfg.originToken, BENEFICIARY, 1, d.emitter, 0));
        assertFalse(ok);
        assertEq(ret, reason("VaultGuard: queued call to a non-administrative target"));
        (ok, ret) = queue(BENEFICIARY, "");
        assertFalse(ok);
        assertEq(ret, reason("VaultGuard: queued call without selector"));

        // A delegatecall, or value, inside an otherwise administrative call.
        bytes memory inner = abi.encodeCall(ISafe.enableModule, (outsider));
        (ok, ret) = ownersTry(
            d.delay,
            abi.encodeCall(IDelay.execTransactionFromModule, (d.safe, 0, inner, Enum.Operation.DelegateCall)),
            Enum.Operation.Call,
            2
        );
        assertFalse(ok);
        assertEq(ret, reason("VaultGuard: queued delegatecall"));
        (ok, ret) = ownersTry(
            d.delay,
            abi.encodeCall(IDelay.execTransactionFromModule, (d.safe, 1, inner, Enum.Operation.Call)),
            Enum.Operation.Call,
            2
        );
        assertFalse(ok);
        assertEq(ret, reason("VaultGuard: queued value"));
        assertEq(delayMod.queueNonce(), 0, "nothing was queued");
    }

    /// REQ-GV-5: nor can the queue queue again. A queued call to the Delay that is itself a queue call would run
    /// as the Safe (a module of the Delay) and enter the queue unseen by the guard, carrying a token transfer.
    /// The guard refuses it when it is queued; without the guard, two cooldowns move the tokens (the control).
    function test_REQ_GV_5_theQueueCannotQueueAgain() public {
        bytes memory transfer = Batches.transferData(BENEFICIARY, 1);
        bytes memory again = abi.encodeCall(
            IDelay.execTransactionFromModule, (cfg.originToken, 0, transfer, Enum.Operation.Call)
        );
        bytes memory againWithData = abi.encodeCall(
            IDelay.execTransactionFromModuleReturnData, (cfg.originToken, 0, transfer, Enum.Operation.Call)
        );
        (bool ok, bytes memory ret) = queue(d.delay, again);
        assertFalse(ok);
        assertEq(ret, reason("VaultGuard: queued Delay call not offered"));
        (ok, ret) = queue(d.delay, againWithData);
        assertFalse(ok);
        assertEq(ret, reason("VaultGuard: queued Delay call not offered"));
        assertEq(delayMod.queueNonce(), 0, "nothing was queued");
        // The Delay's own configuration can still be queued.
        mustQueue(d.delay, abi.encodeCall(IDelay.setTxExpiration, (expiration * 2)));

        // The control: with no guard, the nested queue reaches the token after two cooldowns.
        vm.store(d.safe, bytes32(GUARD_SLOT), bytes32(0));
        mustQueue(d.delay, again);
        vm.warp(block.timestamp + cooldown);
        mustRun(d.delay, abi.encodeCall(IDelay.setTxExpiration, (expiration * 2)));
        mustRun(d.delay, again);
        assertEq(delayMod.queueNonce(), 3, "the second entry was queued by the first");
        vm.warp(block.timestamp + cooldown);
        mustRun(cfg.originToken, transfer);
        assertEq(balance(cfg.originToken, BENEFICIARY), 1, "the tokens moved without the guard");
    }

    /// REQ-GV-5: the guard reads calldata the way its target does: a non-canonical encoding of an otherwise
    /// admitted call is refused. (Here a revocation, with the arrays at offsets Roles would also accept.)
    function test_REQ_GV_5_nonCanonicalCalldataIsRefused() public {
        bytes memory canonical =
            abi.encodeCall(IRoles.assignRoles, (delivery, _one(VaultRoles.DELIVERY), _one(false)));
        bytes memory skewed = abi.encodePacked(
            IRoles.assignRoles.selector,
            bytes32(uint256(uint160(delivery))),
            uint256(0x80), // the first array moved down one word,
            uint256(0xc0),
            uint256(0), // leaving a word of padding in between
            uint256(1),
            VaultRoles.DELIVERY,
            uint256(1),
            uint256(0)
        );
        assertEq(canonical.length + 32, skewed.length);
        // Roles reads both the same way (here called by the Safe, its owner)...
        vm.prank(d.safe);
        (bool ok,) = d.roles.call(skewed);
        assertTrue(ok, "Roles accepts the skewed encoding");
        // ...the guard admits one and not the other.
        assertGuardRefuses(d.roles, skewed, "VaultGuard: non-canonical calldata");
        (ok,) = ownersTry(d.roles, canonical, Enum.Operation.Call, 2);
        assertTrue(ok, "canonical revocation");
    }

    /// The guard serves one Safe, and only answers that Safe.
    function test_REQ_GV_5_guardAnswersItsSafeOnly() public {
        vm.prank(outsider);
        vm.expectRevert(bytes("VaultGuard: not the vault's Safe"));
        VaultGuard(d.guard)
            .checkTransaction(
                d.roles,
                0,
                abi.encodeCall(IRoles.revokeTarget, (VaultRoles.DELIVERY, cfg.originToken)),
                Enum.Operation.Call,
                0,
                0,
                0,
                address(0),
                payable(address(0)),
                "",
                outsider
            );
    }

    /// REQ-GV-5: the Safe has no fallback handler, so it answers no EIP-1271 signature. Owners' signatures
    /// could otherwise authorise what the guard refuses without any Safe transaction: a permit or
    /// transferWithAuthorization on a token the vault holds, or a signed module transaction on the Delay.
    function test_REQ_GV_5_theSafeSignsNothingOffChain() public {
        bytes32 dataHash = keccak256("a message the owners signed");
        bytes memory sigs = ownerSignatures(safeMessageHash(dataHash), 2);

        // The control: with the handler the owners' signatures are valid for the Safe...
        vm.store(d.safe, bytes32(FALLBACK_SLOT), bytes32(uint256(uint160(FALLBACK_HANDLER_141))));
        (bool ok, bytes memory ret) = d.safe.staticcall(abi.encodeWithSelector(EIP1271_MAGIC, dataHash, sigs));
        assertTrue(ok && ret.length >= 4 && bytes4(ret) == EIP1271_MAGIC, "valid with the handler");
        vm.store(d.safe, bytes32(FALLBACK_SLOT), bytes32(0));
        // ...and as deployed they are not.
        (ok, ret) = d.safe.staticcall(abi.encodeWithSelector(EIP1271_MAGIC, dataHash, sigs));
        assertFalse(ok && ret.length >= 4 && bytes4(ret) == EIP1271_MAGIC, "no EIP-1271 answer");
    }

    /// REQ-GV-3, REQ-GV-5: Delay 1.1.1 accepts a module's EIP-1271 signature appended to the calldata of a
    /// queue call, so anyone could queue what the owners signed without a Safe transaction, and the guard
    /// would never see it. With no handler the Safe signs nothing and the call is refused.
    function test_REQ_GV_3_signedQueueCallIsRefused() public {
        bytes memory transfer = Batches.transferData(BENEFICIARY, 1);
        bytes memory queueCall = abi.encodeCall(
            IDelay.execTransactionFromModule, (cfg.originToken, 0, transfer, Enum.Operation.Call)
        );
        bytes32 salt = keccak256("salt");
        bytes memory sigs = ownerSignatures(safeMessageHash(delayMod.moduleTxHash(queueCall, salt)), 2);
        // Contract signature: r = the signer (the Safe, a module of the Delay), s = where its signature starts, v = 0.
        bytes memory signed = abi.encodePacked(
            queueCall, sigs, salt, bytes32(uint256(uint160(d.safe))), bytes32(queueCall.length), uint8(0)
        );

        vm.prank(outsider);
        (bool ok, bytes memory ret) = d.delay.call(signed);
        assertFalse(ok, "the signed queue call is refused");
        assertEq(bytes4(ret), bytes4(keccak256("NotAuthorized(address)")));
        assertEq(delayMod.queueNonce(), 0);

        // The control: with the handler, the same call queues a token transfer that no owner transaction could.
        vm.store(d.safe, bytes32(FALLBACK_SLOT), bytes32(uint256(uint160(FALLBACK_HANDLER_141))));
        vm.prank(outsider);
        (ok, ret) = d.delay.call(signed);
        assertTrue(ok, "queued by signature with the handler");
        assertEq(delayMod.queueNonce(), 1);
    }

    function safeMessageHash(bytes32 dataHash) internal view returns (bytes32) {
        bytes32 messageHash = keccak256(abi.encode(SAFE_MSG_TYPEHASH, keccak256(abi.encode(dataHash))));
        return keccak256(abi.encodePacked(bytes1(0x19), bytes1(0x01), safe.domainSeparator(), messageHash));
    }

    // ------------------------------------------------------------------ REQ-GV-3: widening goes through the delay

    /// REQ-GV-3: every widening change, taken as a direct owner transaction, is refused. The same owners at
    /// the same threshold. Nothing about the vault changes.
    function test_REQ_GV_3_wideningDirectlyIsRefused() public {
        bytes32[] memory role = _one(VaultRoles.DELIVERY);
        bool[] memory yes = _one(true);

        // Roles: a member, a scope, a target, a function, an allowance, an unwrapper, the owner, the avatar.
        string memory viaDelay = "VaultGuard: Roles change must be delayed";
        assertGuardRefuses(d.roles, raised(), "VaultGuard: allowance cap raised");
        assertGuardRefuses(
            d.roles,
            abi.encodeCall(IRoles.assignRoles, (outsider, role, yes)),
            "VaultGuard: assignRoles adds a member"
        );
        assertGuardRefuses(
            d.roles,
            abi.encodeCall(IRoles.assignRoles, (delivery, role, yes)),
            "VaultGuard: assignRoles adds a member"
        );
        assertGuardRefuses(
            d.roles, abi.encodeCall(IRoles.scopeTarget, (VaultRoles.DELIVERY, outsider)), viaDelay
        );
        assertGuardRefuses(
            d.roles,
            abi.encodeCall(IRoles.allowTarget, (VaultRoles.DELIVERY, cfg.originToken, ExecutionOptions.Both)),
            viaDelay
        );
        assertGuardRefuses(
            d.roles,
            abi.encodeCall(
                IRoles.allowFunction,
                (VaultRoles.DELIVERY, cfg.originToken, VaultRoles.TRANSFER, ExecutionOptions.None)
            ),
            viaDelay
        );
        assertGuardRefuses(d.roles, abi.encodeCall(IRoles.enableModule, (outsider)), viaDelay);
        assertGuardRefuses(d.roles, abi.encodeCall(IRoles.setAvatar, (outsider)), viaDelay);
        assertGuardRefuses(d.roles, abi.encodeCall(IRoles.transferOwnership, (outsider)), viaDelay);
        assertGuardRefuses(
            d.roles,
            abi.encodeWithSelector(
                bytes4(keccak256("setTransactionUnwrapper(address,bytes4,address)")),
                cfg.originToken,
                VaultRoles.TRANSFER,
                address(0)
            ),
            viaDelay
        );

        // The Safe: a module, an owner, the threshold, the guard, the Delay, the handler.
        string memory safeViaDelay = "VaultGuard: Safe change must be delayed";
        assertGuardRefuses(d.safe, abi.encodeCall(ISafe.enableModule, (outsider)), safeViaDelay);
        assertGuardRefuses(d.safe, abi.encodeCall(ISafe.addOwnerWithThreshold, (outsider, 1)), safeViaDelay);
        assertGuardRefuses(d.safe, abi.encodeCall(ISafe.setGuard, (address(0))), safeViaDelay);
        assertGuardRefuses(
            d.safe,
            abi.encodeWithSelector(bytes4(keccak256("setFallbackHandler(address)")), FALLBACK_HANDLER_141),
            safeViaDelay
        );
        assertGuardRefuses(
            d.safe,
            abi.encodeCall(ISafe.disableModule, (prevOf(d.delay), d.delay)),
            "VaultGuard: only the Roles modifier may be disabled at once"
        );

        // The Delay: its cooldown, its expiration, its modules, its owner. Only the queue and the veto are offered.
        string memory delayNotOffered = "VaultGuard: Delay call not offered";
        assertGuardRefuses(d.delay, abi.encodeCall(IDelay.setTxCooldown, (0)), delayNotOffered);
        assertGuardRefuses(d.delay, abi.encodeCall(IDelay.setTxExpiration, (0)), delayNotOffered);
        assertGuardRefuses(d.delay, abi.encodeCall(IDelay.enableModule, (outsider)), delayNotOffered);
        assertGuardRefuses(d.delay, abi.encodeCall(IDelay.disableModule, (SENTINEL, d.safe)), delayNotOffered);
        assertGuardRefuses(d.delay, abi.encodeCall(IDelay.transferOwnership, (outsider)), delayNotOffered);
        assertGuardRefuses(d.delay, abi.encodeCall(IDelay.setAvatar, (outsider)), delayNotOffered);
        assertGuardRefuses(d.delay, abi.encodeCall(IDelay.setUp, ("")), delayNotOffered);

        // Nothing changed.
        assertEq(guardSlot(), d.guard);
        assertEq(modulesOfTheSafe(), 3);
        assertEq(safe.getOwners().length, 3);
        assertEq(safe.getThreshold(), 2);
        assertEq(roles.owner(), d.safe);
        assertFalse(roles.isModuleEnabled(outsider));
        assertEq(delayMod.txCooldown(), cooldown);
        assertEq(allowanceBalance(), cfg.deliveryBalance);
        assertEq(safe.nonce(), 0, "no owner transaction executed");
    }

    /// REQ-GV-3: a widening change queued by the owners runs only after the cooldown and before it expires.
    /// Anyone may run it; the queue is first in, first out; only the queued call runs.
    function test_REQ_GV_3_delayedChangeRunsOnlyInItsWindow() public {
        bytes memory change = raised();
        mustQueue(d.roles, change);
        uint256 queuedAt = block.timestamp;
        assertEq(delayMod.queueNonce(), 1);

        // At once, and a second before the cooldown ends: still waiting.
        (bool ok, bytes memory ret) = runQueued(d.roles, change);
        assertFalse(ok);
        assertEq(ret, reason("Transaction is still in cooldown"));
        vm.warp(queuedAt + cooldown - 1);
        (ok, ret) = runQueued(d.roles, change);
        assertFalse(ok);
        assertEq(ret, reason("Transaction is still in cooldown"));
        assertEq(allowanceBalance(), cfg.deliveryBalance, "not raised yet");

        // Another call than the queued one does not run.
        vm.warp(queuedAt + cooldown);
        (ok, ret) = runQueued(d.roles, lowered());
        assertFalse(ok);
        assertEq(ret, reason("Transaction hashes do not match"));

        // At the cooldown: runs, by anyone, once.
        mustRun(d.roles, change);
        assertEq(allowanceBalance(), cfg.deliveryBalance * 2, "raised after the cooldown");
        (ok, ret) = runQueued(d.roles, change);
        assertFalse(ok);
        assertEq(ret, reason("Transaction queue is empty"));
    }

    /// REQ-GV-3: a queued change expires: runnable until `cooldown + expiration` after it was queued, dead
    /// after, and the queue moves on.
    function test_REQ_GV_3_delayedChangeExpires() public {
        bytes memory change = raised();
        mustQueue(d.roles, change);
        uint256 queuedAt = block.timestamp;
        mustQueue(d.roles, lowered());

        vm.warp(queuedAt + cooldown + expiration + 1);
        (bool ok, bytes memory ret) = runQueued(d.roles, change);
        assertFalse(ok);
        assertEq(ret, reason("Transaction expired"));
        assertEq(allowanceBalance(), cfg.deliveryBalance, "an expired change does nothing");

        // Both are dead. Skipping them clears the queue, and a new change goes the whole way again.
        delayMod.skipExpired();
        assertEq(delayMod.txNonce(), 2);
        mustQueue(d.roles, change);
        (ok, ret) = runQueued(d.roles, change);
        assertFalse(ok);
        assertEq(ret, reason("Transaction is still in cooldown"));
        vm.warp(block.timestamp + cooldown);
        mustRun(d.roles, change);
        assertEq(allowanceBalance(), cfg.deliveryBalance * 2);
    }

    /// REQ-GV-3: the last second of the window still runs.
    function test_REQ_GV_3_delayedChangeRunsAtTheEndOfItsWindow() public {
        bytes memory change = raised();
        mustQueue(d.roles, change);
        vm.warp(block.timestamp + cooldown + expiration);
        mustRun(d.roles, change);
        assertEq(allowanceBalance(), cfg.deliveryBalance * 2);
    }

    /// REQ-GV-3: the queue is first in, first out: a later change cannot jump the earlier one.
    function test_REQ_GV_3_queueIsFirstInFirstOut() public {
        bytes memory first = raised();
        bytes memory second = lowered();
        mustQueue(d.roles, first);
        mustQueue(d.roles, second);
        vm.warp(block.timestamp + cooldown);
        (bool ok, bytes memory ret) = runQueued(d.roles, second);
        assertFalse(ok);
        assertEq(ret, reason("Transaction hashes do not match"));
        mustRun(d.roles, first);
        mustRun(d.roles, second);
        assertEq(allowanceBalance(), cfg.deliveryBalance / 2, "in the order queued");
    }

    /// REQ-GV-3: queueing takes the owners at the threshold, like any Safe transaction.
    function test_REQ_GV_3_queueingNeedsTheThreshold() public {
        bytes memory data =
            abi.encodeCall(IDelay.execTransactionFromModule, (d.roles, 0, raised(), Enum.Operation.Call));
        assertFalse(ownersExec(d.delay, data, Enum.Operation.Call, 1), "one signature");
        assertEq(delayMod.queueNonce(), 0);
        assertTrue(ownersExec(d.delay, data, Enum.Operation.Call, 2), "two signatures");
        assertEq(delayMod.queueNonce(), 1);
    }

    /// REQ-GV-3: a role cannot reach the Delay: the delivery key is not one of its modules.
    function test_REQ_GV_3_roleKeysCannotQueue() public {
        vm.prank(delivery);
        (bool ok, bytes memory ret) = d.delay
            .call(
                abi.encodeCall(IDelay.execTransactionFromModule, (d.roles, 0, raised(), Enum.Operation.Call))
            );
        assertFalse(ok);
        assertEq(bytes4(ret), bytes4(keccak256("NotAuthorized(address)")));
        (ok, ret) = roleCall(
            delivery,
            VaultRoles.DELIVERY,
            d.delay,
            0,
            abi.encodeCall(IDelay.execTransactionFromModule, (d.roles, 0, raised(), Enum.Operation.Call)),
            Enum.Operation.Call
        );
        assertViolation(ok, ret, TARGET_NOT_ALLOWED);
    }

    // ------------------------------------------------------------------ removing the guard or the Delay

    /// REQ-GV-3, REQ-GV-5: removing the guard is itself delayed. Directly it is refused; queued, it runs
    /// after the cooldown. That is the residual of DEC-10, stated in NOTES.md: once the cooldown has passed
    /// and nobody vetoed, the owners are no longer limited by the guard.
    function test_REQ_GV_3_removingTheGuardIsDelayed() public {
        bytes memory remove = abi.encodeCall(ISafe.setGuard, (address(0)));
        assertGuardRefuses(d.safe, remove, "VaultGuard: Safe change must be delayed");
        assertEq(guardSlot(), d.guard);

        mustQueue(d.safe, remove);
        (bool ok, bytes memory ret) = runQueued(d.safe, remove);
        assertFalse(ok);
        assertEq(ret, reason("Transaction is still in cooldown"));
        assertEq(guardSlot(), d.guard, "still guarded during the cooldown");
        assertGuardRefuses(
            cfg.originToken, Batches.transferData(BENEFICIARY, 1), "VaultGuard: not an administrative target"
        );

        vm.warp(block.timestamp + cooldown);
        mustRun(d.safe, remove);
        assertEq(guardSlot(), address(0), "guard removed after the cooldown");
        // The residual, made visible: without the guard the owners move tokens directly.
        assertTrue(ownersExec(cfg.originToken, Batches.transferData(BENEFICIARY, 1), Enum.Operation.Call, 2));
    }

    /// REQ-GV-3, REQ-GV-5: the Delay module is not removable, directly or through the queue. Nothing could be
    /// queued once it is gone, so a vault paused by then could never be unpaused (the lock-out of the review's
    /// finding 2): the guard refuses the entry when it is queued.
    function test_REQ_GV_3_theDelayCannotBeRemoved() public {
        bytes memory remove = abi.encodeCall(ISafe.disableModule, (prevOf(d.delay), d.delay));
        assertGuardRefuses(d.safe, remove, "VaultGuard: only the Roles modifier may be disabled at once");
        (bool ok, bytes memory ret) = queue(d.safe, remove);
        assertFalse(ok);
        assertEq(ret, reason("VaultGuard: queued call disables the Delay"));
        assertEq(delayMod.queueNonce(), 0, "nothing was queued");
        assertTrue(safe.isModuleEnabled(d.delay));
    }

    /// REQ-GV-3: raising the cooldown is itself delayed: it runs after the old cooldown. (Lowering it is not a
    /// queue operation at all: Guardian.t.sol, test_REQ_GV_3_aQueuedCooldownCannotBeLowered.)
    function test_REQ_GV_3_changingTheDelayIsDelayed() public {
        bytes memory longer = abi.encodeCall(IDelay.setTxCooldown, (cooldown * 2));
        mustQueue(d.delay, longer);
        (bool ok, bytes memory ret) = runQueued(d.delay, longer);
        assertFalse(ok);
        assertEq(ret, reason("Transaction is still in cooldown"));
        vm.warp(block.timestamp + cooldown);
        mustRun(d.delay, longer);
        assertEq(delayMod.txCooldown(), cooldown * 2, "raised after the old cooldown");
        assertEq(delayMod.txExpiration(), expiration, "expiration untouched");
    }

    // ------------------------------------------------------------------ narrowing is immediate

    /// REQ-GV-5: lowering an allowance is immediate (a pause), in every field at once.
    function test_REQ_GV_5_loweringAnAllowanceIsImmediate() public {
        assertTrue(ownersExec(d.roles, lowered(), Enum.Operation.Call, 2), "lowered at once");
        (uint128 refill, uint128 maxRefill, uint64 period, uint128 bal,) = roles.allowances(allowanceKey);
        assertEq(bal, cfg.deliveryBalance / 2);
        assertEq(maxRefill, cfg.deliveryMaxRefill / 2);
        assertEq(refill, cfg.deliveryRefill / 2);
        assertEq(period, cfg.deliveryPeriod * 2);

        // A delivery above the lowered balance is refused; one within it still pays.
        (bool ok, bytes memory ret) =
            deliver(BENEFICIARY, uint256(cfg.deliveryBalance) / 2 + 1, keccak256("over"));
        assertViolation(ok, ret, ALLOWANCE_EXCEEDED);
        (ok, ret) = deliver(BENEFICIARY, uint256(cfg.deliveryBalance) / 2, keccak256("within"));
        assertTrue(ok, string(ret));
    }

    /// REQ-GV-5: the owners can stop deliveries at once by lowering the allowance to nothing.
    function test_REQ_GV_5_loweringToNothingPausesDeliveries() public {
        bytes memory zero = abi.encodeCall(IRoles.setAllowance, (allowanceKey, 0, 1, 0, 0, 0));
        assertTrue(ownersExec(d.roles, zero, Enum.Operation.Call, 2));
        (bool ok, bytes memory ret) = deliver(BENEFICIARY, 1, keccak256("paused"));
        assertViolation(ok, ret, ALLOWANCE_EXCEEDED);
        vm.warp(block.timestamp + 30 days);
        (ok, ret) = deliver(BENEFICIARY, 1, keccak256("still paused"));
        assertViolation(ok, ret, ALLOWANCE_EXCEEDED);
    }

    /// REQ-GV-5: no field of an allowance can be raised, and the refill clock cannot be moved, at once.
    function test_REQ_GV_5_raisingAnAllowanceNeedsTheDelay() public {
        uint128 b = cfg.deliveryBalance;
        uint128 m = cfg.deliveryMaxRefill;
        uint128 r = cfg.deliveryRefill;
        uint64 p = cfg.deliveryPeriod;
        bytes32 k = allowanceKey;

        assertGuardRefuses(
            d.roles,
            abi.encodeCall(IRoles.setAllowance, (k, b + 1, m, r, p, 0)),
            "VaultGuard: allowance balance raised"
        );
        assertGuardRefuses(
            d.roles,
            abi.encodeCall(IRoles.setAllowance, (k, b, m + 1, r, p, 0)),
            "VaultGuard: allowance cap raised"
        );
        // Roles reads a cap of zero as "no cap".
        assertGuardRefuses(
            d.roles,
            abi.encodeCall(IRoles.setAllowance, (k, b, 0, r, p, 0)),
            "VaultGuard: allowance cap raised"
        );
        assertGuardRefuses(
            d.roles,
            abi.encodeCall(IRoles.setAllowance, (k, b, m, r + 1, p, 0)),
            "VaultGuard: allowance refill raised"
        );
        assertGuardRefuses(
            d.roles,
            abi.encodeCall(IRoles.setAllowance, (k, b, m, r, p - 1, 0)),
            "VaultGuard: allowance refill raised"
        );
        // An old refill clock refills at once; a future one only delays. Neither is offered.
        assertGuardRefuses(
            d.roles,
            abi.encodeCall(IRoles.setAllowance, (k, b, m, r, p, 1)),
            "VaultGuard: allowance clock set"
        );
        assertGuardRefuses(
            d.roles,
            abi.encodeCall(IRoles.setAllowance, (k, b, m, r, p, uint64(block.timestamp + 1 days))),
            "VaultGuard: allowance clock set"
        );
        // An allowance nobody set.
        assertGuardRefuses(
            d.roles,
            abi.encodeCall(IRoles.setAllowance, (keccak256("another"), 1, 1, 0, 0, 0)),
            "VaultGuard: allowance cap raised"
        );

        // One that does not refill cannot start to.
        assertTrue(
            ownersExec(
                d.roles, abi.encodeCall(IRoles.setAllowance, (k, b, m, 0, p, 0)), Enum.Operation.Call, 2
            )
        );
        assertGuardRefuses(
            d.roles,
            abi.encodeCall(IRoles.setAllowance, (k, b, m, 1, p, 0)),
            "VaultGuard: allowance refill raised"
        );
        assertTrue(
            ownersExec(
                d.roles, abi.encodeCall(IRoles.setAllowance, (k, b, m, 1, 0, 0)), Enum.Operation.Call, 2
            ),
            "no period: no refill"
        );
        assertGuardRefuses(
            d.roles,
            abi.encodeCall(IRoles.setAllowance, (k, b, m, 1, p, 0)),
            "VaultGuard: allowance refill raised"
        );
    }

    /// REQ-GV-5: revoking a role member is immediate and takes effect at once; adding one is not offered.
    function test_REQ_GV_5_revokingAMemberIsImmediate() public {
        (bool ok, bytes memory ret) = deliver(BENEFICIARY, 1, keccak256("before"));
        assertTrue(ok, string(ret));

        assertTrue(
            ownersExec(
                d.roles,
                abi.encodeCall(IRoles.assignRoles, (delivery, _one(VaultRoles.DELIVERY), _one(false))),
                Enum.Operation.Call,
                2
            )
        );
        (ok, ret) = deliver(BENEFICIARY, 1, keccak256("after"));
        assertRevertSelector(ok, ret, NO_MEMBERSHIP);

        // A revocation cannot enable a module that is not one (assignRoles does that as a side effect).
        assertGuardRefuses(
            d.roles,
            abi.encodeCall(IRoles.assignRoles, (outsider, _one(VaultRoles.DELIVERY), _one(false))),
            "VaultGuard: assignRoles enables a module"
        );
        assertFalse(roles.isModuleEnabled(outsider));
        // Putting the member back is a widening: the queue.
        assertGuardRefuses(
            d.roles,
            abi.encodeCall(IRoles.assignRoles, (delivery, _one(VaultRoles.DELIVERY), _one(true))),
            "VaultGuard: assignRoles adds a member"
        );
    }

    /// REQ-GV-5: revoking a target or a function, or disabling a member's module, is immediate.
    function test_REQ_GV_5_revokingScopesIsImmediate() public {
        assertTrue(
            ownersExec(
                d.roles,
                abi.encodeCall(
                    IRoles.revokeFunction,
                    (VaultRoles.DELIVERY, d.emitter, bytes4(keccak256("emitReference(bytes32)")))
                ),
                Enum.Operation.Call,
                2
            )
        );
        (bool ok,) = deliver(BENEFICIARY, 1, keccak256("no emitter"));
        assertFalse(ok, "the pair is no longer admitted");

        assertTrue(
            ownersExec(
                d.roles,
                abi.encodeCall(IRoles.revokeTarget, (VaultRoles.DELIVERY, cfg.originToken)),
                Enum.Operation.Call,
                2
            )
        );
        (ok,) = deliver(BENEFICIARY, 1, keccak256("no token"));
        assertFalse(ok, "the token is no longer a target");

        address prev = SENTINEL;
        (address[] memory modules,) = roles.getModulesPaginated(SENTINEL, 10);
        for (uint256 i; i < modules.length; ++i) {
            if (modules[i] == delivery) break;
            prev = modules[i];
        }
        assertTrue(
            ownersExec(
                d.roles, abi.encodeCall(IRoles.disableModule, (prev, delivery)), Enum.Operation.Call, 2
            )
        );
        assertFalse(roles.isModuleEnabled(delivery));
    }

    /// REQ-GV-5: the owners can switch the whole vault off at once, by disabling the Roles module on the Safe.
    /// Switching it on again is a widening: the queue.
    function test_REQ_GV_5_pausingTheVaultIsImmediateAndUnpausingIsDelayed() public {
        assertTrue(
            ownersExec(
                d.safe,
                abi.encodeCall(ISafe.disableModule, (prevOf(d.roles), d.roles)),
                Enum.Operation.Call,
                2
            )
        );
        assertFalse(safe.isModuleEnabled(d.roles));
        (bool ok,) = deliver(BENEFICIARY, 1, keccak256("paused"));
        assertFalse(ok, "paused");
        assertEq(balance(cfg.originToken, BENEFICIARY), 0);

        bytes memory enable = abi.encodeCall(ISafe.enableModule, (d.roles));
        assertGuardRefuses(d.safe, enable, "VaultGuard: Safe change must be delayed");
        mustQueue(d.safe, enable);
        vm.warp(block.timestamp + cooldown);
        mustRun(d.safe, enable);
        (ok,) = deliver(BENEFICIARY, 1, keccak256("resumed"));
        assertTrue(ok, "resumed");
    }

    /// REQ-GV-3: the owners can veto a queued change (skip it) at once, with no cooldown.
    function test_REQ_GV_3_ownersCanVetoAQueuedChange() public {
        bytes memory change = raised();
        mustQueue(d.roles, change);
        mustQueue(d.roles, lowered());
        assertTrue(
            ownersExec(d.delay, abi.encodeCall(IDelay.setTxNonce, (1)), Enum.Operation.Call, 2),
            "skip the first"
        );
        assertEq(delayMod.txNonce(), 1);

        vm.warp(block.timestamp + cooldown);
        (bool ok, bytes memory ret) = runQueued(d.roles, change);
        assertFalse(ok, "the vetoed change does not run");
        assertEq(ret, reason("Transaction hashes do not match"));
        mustRun(d.roles, lowered());
        assertEq(allowanceBalance(), cfg.deliveryBalance / 2);
    }

    // ------------------------------------------------------------------ the delivery role is untouched

    /// AC-I1.1, REQ-KY-3: the delivery role pays within its bounds, at once, with no delay, with a change
    /// pending in the queue, and is still bounded by its allowance.
    function test_AC_I1_1_deliveryPaysWithoutDelay() public {
        uint256 amount = uint256(cfg.deliveryBalance) / 2;
        (bool ok, bytes memory ret) = deliver(BENEFICIARY, amount, keccak256("first"));
        assertTrue(ok, string(ret));
        assertEq(balance(cfg.originToken, BENEFICIARY), amount);

        // A widening is pending (and not yet runnable); deliveries are unaffected.
        mustQueue(d.roles, raised());
        (ok, ret) = deliver(BENEFICIARY, amount, keccak256("second"));
        assertTrue(ok, string(ret));
        assertEq(balance(cfg.originToken, BENEFICIARY), 2 * amount);

        // The bound holds: the window is spent, and the pending change has not raised it.
        (ok, ret) = deliver(BENEFICIARY, 1, keccak256("third"));
        assertViolation(ok, ret, ALLOWANCE_EXCEEDED);
        assertEq(allowanceBalance(), 0);

        // The change, once it runs, is what widens it.
        vm.warp(block.timestamp + cooldown);
        mustRun(d.roles, raised());
        (ok, ret) = deliver(BENEFICIARY, 1, keccak256("fourth"));
        assertTrue(ok, string(ret));
    }

    /// AC-I1.1: the allowance still refills per period as deployed (the guard and the Delay do not touch it).
    function test_AC_I1_1_deliveryRefillsPerPeriodWithTheGuardOn() public {
        (bool ok, bytes memory ret) = deliver(BENEFICIARY, uint256(cfg.deliveryBalance), keccak256("all"));
        assertTrue(ok, string(ret));
        (ok, ret) = deliver(BENEFICIARY, 1, keccak256("none left"));
        assertViolation(ok, ret, ALLOWANCE_EXCEEDED);
        vm.warp(block.timestamp + cfg.deliveryPeriod);
        (ok, ret) = deliver(BENEFICIARY, uint256(cfg.deliveryBalance), keccak256("refilled"));
        assertTrue(ok, string(ret));
    }
}

contract PolygonGovernanceTest is GovernanceTest {
    function network() internal pure override returns (string memory) {
        return "polygon";
    }
}

contract BaseGovernanceTest is GovernanceTest {
    function network() internal pure override returns (string memory) {
        return "base";
    }
}

contract BscGovernanceTest is GovernanceTest {
    function network() internal pure override returns (string memory) {
        return "bsc";
    }
}
