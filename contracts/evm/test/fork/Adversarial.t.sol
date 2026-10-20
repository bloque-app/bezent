// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {Enum} from "@gnosis.pm/safe-contracts/contracts/common/Enum.sol";

import {VaultGuard} from "../../src/VaultGuard.sol";
import {ISafe, IRoles} from "../../src/interfaces/External.sol";
import {Batches} from "../Batches.sol";
import {GovernanceBase} from "./GovernanceBase.sol";

/// Scenarios and a fuzz run from the adversarial review of PR 41 (the review's own tests, adapted): the
/// guard under attack from the owners' side. The review's lock-out scenario (a queued `Delay.setAvatar`) is
/// `Guardian.t.sol`, where the guard now refuses it. Polygon only: they do not depend on the network.
contract AdversarialPolygonTest is GovernanceBase {
    function network() internal pure override returns (string memory) {
        return "polygon";
    }

    /// REQ-GV-5: a queued call to the Safe that is the Safe's own module entry point is accepted into the queue
    /// and fails when it runs (the Safe is not its own module), moving nothing.
    function test_REQ_GV_5_queuedSelfExecDoesNotBypass() public {
        bytes memory transfer = Batches.transferData(BENEFICIARY, 1);
        bytes memory inner = abi.encodeWithSignature(
            "execTransactionFromModule(address,uint256,bytes,uint8)", cfg.originToken, 0, transfer, uint8(0)
        );
        mustQueue(d.safe, inner);
        vm.warp(block.timestamp + cooldown);
        (bool ok,) = runQueued(d.safe, inner);
        assertFalse(ok, "must not run");
        assertEq(balance(cfg.originToken, BENEFICIARY), 0);
    }

    /// REQ-GV-5: owners who pre-approve a hash on the Safe and have it relayed as `execTransaction` are still
    /// stopped by the guard.
    function test_REQ_GV_5_approvedHashStillGuarded() public {
        bytes memory transfer = Batches.transferData(BENEFICIARY, 1000);
        bytes32 h = safe.getTransactionHash(
            cfg.originToken, 0, transfer, Enum.Operation.Call, 0, 0, 0, address(0), address(0), safe.nonce()
        );
        vm.prank(owners[0]);
        (bool a,) = d.safe.call(abi.encodeWithSignature("approveHash(bytes32)", h));
        assertTrue(a);
        vm.prank(owners[1]);
        (a,) = d.safe.call(abi.encodeWithSignature("approveHash(bytes32)", h));
        assertTrue(a);
        // Pre-validated signatures (v = 1), sorted by owner address.
        address lo = owners[0] < owners[1] ? owners[0] : owners[1];
        address hi = owners[0] < owners[1] ? owners[1] : owners[0];
        bytes memory sigs = abi.encodePacked(
            bytes32(uint256(uint160(lo))),
            bytes32(0),
            uint8(1),
            bytes32(uint256(uint160(hi))),
            bytes32(0),
            uint8(1)
        );
        vm.prank(outsider);
        (bool ok, bytes memory ret) = d.safe
            .call(
                abi.encodeCall(
                    ISafe.execTransaction,
                    (
                        cfg.originToken,
                        0,
                        transfer,
                        Enum.Operation.Call,
                        0,
                        0,
                        0,
                        address(0),
                        payable(address(0)),
                        sigs
                    )
                )
            );
        assertFalse(ok);
        assertEq(ret, reason("VaultGuard: not an administrative target"));
    }

    /// What an allowance releases by time `t`, as Roles' AllowanceTracker accrues it.
    function _accrued(uint128 refill, uint128 maxRefill, uint64 period, uint128 bal, uint64 ts, uint64 t)
        internal
        pure
        returns (uint128 accrued)
    {
        if (period == 0 || t < ts + period) return bal;
        uint64 n = (t - ts) / period;
        if (bal < maxRefill) {
            accrued = bal + refill * n;
            accrued = accrued < maxRefill ? accrued : maxRefill;
        } else {
            accrued = bal;
        }
    }

    struct Old {
        uint128 balance;
        uint128 maxRefill;
        uint128 refill;
        uint32 period;
        uint32 age;
        uint32 wait;
    }

    struct Next {
        uint128 balance;
        uint128 maxRefill;
        uint128 refill;
        uint32 period;
        uint32 later;
    }

    /// REQ-GV-5 (fuzz): an allowance change the guard lets through at once never releases more than the old one
    /// would have, at any later time.
    function testFuzz_REQ_GV_5_loweringAnAllowanceIsMonotone(Old memory o, Next memory n) public {
        bytes32 key = keccak256("fuzz");
        o.balance = uint128(bound(o.balance, 0, 1e24));
        o.maxRefill = uint128(bound(o.maxRefill, 1, 1e24));
        o.refill = uint128(bound(o.refill, 0, 1e18));
        o.period = uint32(bound(o.period, 0, 1e6));
        o.age = uint32(bound(o.age, 0, 1e7));
        o.wait = uint32(bound(o.wait, 0, 1e7));
        n.balance = uint128(bound(n.balance, 0, 1e24));
        n.maxRefill = uint128(bound(n.maxRefill, 0, 1e24));
        n.refill = uint128(bound(n.refill, 0, 1e18));
        n.period = uint32(bound(n.period, 0, 1e6));
        n.later = uint32(bound(n.later, 0, 1e8));

        // About `age` seconds ago; Roles reads a clock of 0 as "now".
        uint64 t0 = uint64(block.timestamp);
        vm.prank(d.safe);
        roles.setAllowance(key, o.balance, o.maxRefill, o.refill, o.period, t0 > o.age ? t0 - o.age : 1);
        vm.warp(block.timestamp + o.wait);
        (uint128 r1, uint128 m1, uint64 p1, uint128 b1, uint64 s1) = roles.allowances(key);

        bytes memory data =
            abi.encodeCall(IRoles.setAllowance, (key, n.balance, n.maxRefill, n.refill, n.period, 0));
        vm.prank(d.safe);
        try VaultGuard(d.guard)
            .checkTransaction(
                d.roles,
                0,
                data,
                Enum.Operation.Call,
                0,
                0,
                0,
                address(0),
                payable(address(0)),
                "",
                address(0)
            ) {
            vm.prank(d.safe);
            roles.setAllowance(key, n.balance, n.maxRefill, n.refill, n.period, 0);
            (uint128 r2, uint128 m2, uint64 p2, uint128 b2, uint64 s2) = roles.allowances(key);
            uint64 t = uint64(block.timestamp) + n.later;
            assertLe(
                _accrued(r2, m2, p2, b2, s2, t),
                _accrued(r1, m1, p1, b1, s1, t),
                "the guard let a raise through"
            );
        } catch {}
    }
}
