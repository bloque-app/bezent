// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {Test, Vm} from "forge-std/Test.sol";
import {ReferenceEmitter} from "../src/ReferenceEmitter.sol";

/// T-008-01 (project builds; an empty test passes in CI) and T-008-02 (ReferenceEmitter), no fork.
contract ReferenceEmitterTest is Test {
    event Reference(bytes32 indexed ref);

    ReferenceEmitter internal emitter;

    function setUp() public {
        emitter = new ReferenceEmitter();
    }

    /// T-008-01: the Foundry project compiles and runs.
    function test_DEC_10_projectBuilds() public pure {
        assertTrue(true);
    }

    /// T-008-02: the only function emits `Reference(bytes32)` with the reference it was given.
    function test_REQ_RN_5_emitsReference() public {
        bytes32 ref = keccak256("bridge reference 1");
        vm.expectEmit(true, true, true, true, address(emitter));
        emit Reference(ref);
        emitter.emitReference(ref);
    }

    /// T-008-02: the topic is the event signature `Reference(bytes32)`, the reference is the indexed topic.
    function test_REQ_RN_5_eventShape() public {
        bytes32 ref = bytes32(uint256(42));
        vm.recordLogs();
        emitter.emitReference(ref);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(logs.length, 1);
        assertEq(logs[0].emitter, address(emitter));
        assertEq(logs[0].topics.length, 2);
        assertEq(logs[0].topics[0], keccak256("Reference(bytes32)"));
        assertEq(logs[0].topics[1], ref);
        assertEq(logs[0].data.length, 0);
    }

    /// T-008-02: stateless — no storage is read or written, and it accepts no value.
    function test_REQ_RN_5_stateless() public {
        vm.record();
        emitter.emitReference(bytes32(uint256(7)));
        (bytes32[] memory reads, bytes32[] memory writes) = vm.accesses(address(emitter));
        assertEq(reads.length, 0);
        assertEq(writes.length, 0);

        (bool ok,) =
            address(emitter).call{value: 1}(abi.encodeCall(ReferenceEmitter.emitReference, (bytes32(0))));
        assertFalse(ok, "payable");
        (ok,) = address(emitter).call(hex"deadbeef");
        assertFalse(ok, "fallback");
    }
}
