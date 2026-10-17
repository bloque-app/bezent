// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {Test} from "forge-std/Test.sol";
import {Enum} from "@gnosis.pm/safe-contracts/contracts/common/Enum.sol";
import {UnwrappedTransaction} from "zodiac-roles/adapters/Types.sol";

import {ReferencedTransferUnwrapper} from "../src/ReferencedTransferUnwrapper.sol";
import {Batches} from "./Batches.sol";

/// T-008-04 (no fork): the unwrapper admits exactly the referenced pair and refuses every other shape.
/// The fork suites check the same through the deployed Roles modifier.
contract ReferencedTransferUnwrapperTest is Test {
    address internal constant MULTISEND = 0x9641d764fc13c8B624c04430C7356C1C7C8102e2;
    address internal constant TOKEN = address(0x7070);
    address internal constant EMITTER = address(0xE111);
    address internal constant TO = address(0xBEEF);
    bytes32 internal constant REF = keccak256("ref");

    ReferencedTransferUnwrapper internal u;

    function setUp() public {
        u = new ReferencedTransferUnwrapper(MULTISEND, EMITTER);
    }

    function _unwrap(bytes memory data) internal view returns (UnwrappedTransaction[] memory) {
        return u.unwrap(MULTISEND, 0, data, Enum.Operation.DelegateCall);
    }

    function _refused(address to, uint256 value, bytes memory data, Enum.Operation op) internal view {
        try u.unwrap(to, value, data, op) returns (UnwrappedTransaction[] memory) {
            revert("admitted");
        } catch {}
    }

    /// INV-14: the pair unwraps to the transfer and the reference, each pointing at its own calldata.
    function test_INV_14_pairUnwrapsToTransferAndReference() public view {
        bytes memory data = Batches.delivery(TOKEN, TO, 1e6, EMITTER, REF);
        UnwrappedTransaction[] memory t = _unwrap(data);
        assertEq(t.length, 2);
        assertEq(t[0].to, TOKEN);
        assertEq(t[0].value, 0);
        assertEq(uint8(t[0].operation), 0);
        assertEq(t[1].to, EMITTER);
        assertEq(t[1].value, 0);
        assertEq(uint8(t[1].operation), 0);
        assertEq(_slice(data, t[0].dataLocation, t[0].dataSize), Batches.transferData(TO, 1e6));
        assertEq(_slice(data, t[1].dataLocation, t[1].dataSize), Batches.emitData(REF));
    }

    /// REQ-RN-5: a transfer without the emitter call is refused, alone, called directly, or out of order.
    function test_REQ_RN_5_transferWithoutReferenceRefused() public view {
        bytes memory transfer = Batches.entry(0, TOKEN, 0, Batches.transferData(TO, 1));
        bytes memory refEntry = Batches.entry(0, EMITTER, 0, Batches.emitData(REF));
        Enum.Operation dc = Enum.Operation.DelegateCall;

        _refused(MULTISEND, 0, Batches.multiSend(transfer), dc);
        _refused(MULTISEND, 0, Batches.multiSend(refEntry), dc);
        _refused(MULTISEND, 0, Batches.multiSend(bytes.concat(refEntry, transfer)), dc);
        _refused(MULTISEND, 0, Batches.multiSend(bytes.concat(transfer, transfer)), dc);
        _refused(MULTISEND, 0, Batches.multiSend(bytes.concat(transfer, refEntry, transfer)), dc);
        _refused(MULTISEND, 0, Batches.multiSend(bytes.concat(transfer, refEntry, refEntry)), dc);
        // A bare transfer, which is how the unwrapper refuses it when registered at (token, transfer).
        _refused(TOKEN, 0, Batches.transferData(TO, 1), Enum.Operation.Call);
    }

    /// REQ-RN-5: the second call must be the configured emitter's emitReference.
    function test_REQ_RN_5_onlyTheConfiguredEmitter() public view {
        bytes memory transfer = Batches.entry(0, TOKEN, 0, Batches.transferData(TO, 1));
        Enum.Operation dc = Enum.Operation.DelegateCall;
        _refused(
            MULTISEND,
            0,
            Batches.multiSend(
                bytes.concat(transfer, Batches.entry(0, address(0xBAD), 0, Batches.emitData(REF)))
            ),
            dc
        );
        // Same length, other selector.
        bytes memory other = abi.encodeWithSelector(bytes4(0x12345678), REF);
        _refused(
            MULTISEND, 0, Batches.multiSend(bytes.concat(transfer, Batches.entry(0, EMITTER, 0, other))), dc
        );
        // transferFrom in place of transfer (other selector, other length).
        bytes memory tf = abi.encodeWithSelector(bytes4(0x23b872dd), address(1), TO, uint256(1));
        _refused(
            MULTISEND,
            0,
            Batches.multiSend(
                bytes.concat(
                    Batches.entry(0, TOKEN, 0, tf), Batches.entry(0, EMITTER, 0, Batches.emitData(REF))
                )
            ),
            dc
        );
    }

    /// REQ-KY-3: no value and no delegatecall inside the pair; the batch itself only as a value-free
    /// delegatecall to MultiSendCallOnly.
    function test_REQ_KY_3_noValueNoInnerDelegatecall() public view {
        bytes memory e = Batches.entry(0, EMITTER, 0, Batches.emitData(REF));
        Enum.Operation dc = Enum.Operation.DelegateCall;
        _refused(
            MULTISEND,
            0,
            Batches.multiSend(bytes.concat(Batches.entry(1, TOKEN, 0, Batches.transferData(TO, 1)), e)),
            dc
        );
        _refused(
            MULTISEND,
            0,
            Batches.multiSend(bytes.concat(Batches.entry(0, TOKEN, 1, Batches.transferData(TO, 1)), e)),
            dc
        );
        bytes memory t = Batches.entry(0, TOKEN, 0, Batches.transferData(TO, 1));
        _refused(
            MULTISEND,
            0,
            Batches.multiSend(bytes.concat(t, Batches.entry(1, EMITTER, 0, Batches.emitData(REF)))),
            dc
        );
        _refused(
            MULTISEND,
            0,
            Batches.multiSend(bytes.concat(t, Batches.entry(0, EMITTER, 1, Batches.emitData(REF)))),
            dc
        );

        bytes memory good = Batches.delivery(TOKEN, TO, 1, EMITTER, REF);
        _refused(MULTISEND, 1, good, dc);
        _refused(MULTISEND, 0, good, Enum.Operation.Call);
        _refused(address(0x38869bf66a61cF6bDB996A6aE40D5853Fd43B526), 0, good, dc); // MultiSend, not CallOnly
    }

    /// INV-14: only the canonical encoding, so what is checked is what MultiSendCallOnly executes.
    function test_INV_14_nonCanonicalEncodingRefused() public view {
        bytes memory good = Batches.delivery(TOKEN, TO, 1, EMITTER, REF);
        Enum.Operation dc = Enum.Operation.DelegateCall;
        assertEq(good.length, 356);

        bytes memory b = bytes.concat(good); // other offset
        b[35] = 0x40;
        _refused(MULTISEND, 0, b, dc);
        b = bytes.concat(good); // dirty padding
        b[355] = 0x01;
        _refused(MULTISEND, 0, b, dc);
        _refused(MULTISEND, 0, bytes.concat(good, bytes32(0)), dc); // trailing bytes
        b = bytes.concat(good); // other selector
        b[0] = 0x00;
        _refused(MULTISEND, 0, b, dc);
        b = bytes.concat(good); // wrong length word
        b[67] = bytes1(uint8(b[67]) + 1);
        _refused(MULTISEND, 0, b, dc);
    }

    function _slice(bytes memory data, uint256 at, uint256 size) internal pure returns (bytes memory out) {
        out = new bytes(size);
        for (uint256 i; i < size; ++i) {
            out[i] = data[at + i];
        }
    }
}
