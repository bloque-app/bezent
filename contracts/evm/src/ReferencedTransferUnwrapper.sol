// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {Enum} from "@gnosis.pm/safe-contracts/contracts/common/Enum.sol";
import {ITransactionUnwrapper, UnwrappedTransaction} from "zodiac-roles/adapters/Types.sol";

/// @title ReferencedTransferUnwrapper
/// @notice A Zodiac Roles v2 transaction unwrapper that admits exactly one batch shape: a MultiSendCallOnly
/// of (1) a `transfer(address,uint256)` call and (2) a `ReferenceEmitter.emitReference(bytes32)` call, both
/// plain calls without value. It returns the two calls so that Roles checks each against the role's scopes
/// (the transfer against the origin token and its allowance). Any other batch, and any call that is not to
/// the MultiSendCallOnly, reverts, which Roles reports as `MalformedMultiEntrypoint`.
///
/// It is registered twice on the vault's Roles modifier (plan DEC-10, T-008-04):
///   - at (MultiSendCallOnly, multiSend): the referenced delivery, the only way the delivery role moves value;
///   - at (origin token, transfer): so a bare transfer by any role is refused. Roles routes a call through
///     the unwrapper registered for its (to, selector) before any scope is read, and this contract refuses
///     every `to` but the MultiSendCallOnly. Inside an unwrapped batch Roles checks each call directly,
///     without looking up unwrappers again, so the transfer in a referenced pair still reaches its scope.
///
/// Why not Roles' stock MultiSendUnwrapper: it checks every call of a batch on its own, so a batch holding
/// only a transfer would pass. The pairing (INV-14, REQ-RN-5: "a transfer without the emitter call is
/// refused") needs a check across the batch, which this contract adds.
///
/// @dev The layout is fixed, so it is checked byte by byte rather than parsed in a loop. MultiSendCallOnly
/// v1.4.1 reads the `transactions` bytes argument at the ABI offset; this contract requires the canonical
/// encoding (offset 0x20, exact length, zero padding), so what it checks is exactly what MultiSendCallOnly
/// executes. It must never return an empty array: Roles treats an empty unwrap as authorised.
contract ReferencedTransferUnwrapper is ITransactionUnwrapper {
    /// MultiSendCallOnly v1.4.1, the only batch target admitted.
    address public immutable MULTISEND;
    /// The ReferenceEmitter every delivery must call.
    address public immutable EMITTER;

    bytes4 internal constant MULTISEND_SELECTOR = 0x8d80ff0a; // multiSend(bytes)
    bytes4 internal constant TRANSFER_SELECTOR = 0xa9059cbb; // transfer(address,uint256)
    bytes4 internal constant EMIT_SELECTOR = bytes4(keccak256("emitReference(bytes32)"));

    // Layout of multiSend(bytes) calldata holding exactly (transfer, emitReference):
    //   [0, 4)     selector
    //   [4, 36)    offset of `transactions`, 0x20
    //   [36, 68)   length of `transactions`, 274 = (1+20+32+32+68) + (1+20+32+32+36)
    //   [68, 221)  entry 0: operation (1) | to (20) | value (32) | data length (32) = 68 | transfer data (68)
    //   [221, 342) entry 1: operation (1) | to (20) | value (32) | data length (32) = 36 | emit data (36)
    //   [342, 356) zero padding to a multiple of 32
    uint256 internal constant TX_BYTES = 274;
    uint256 internal constant CALLDATA_BYTES = 356;
    uint256 internal constant E0 = 68;
    uint256 internal constant E1 = 221;
    uint256 internal constant TRANSFER_DATA = E0 + 85; // 153
    uint256 internal constant TRANSFER_SIZE = 68;
    uint256 internal constant EMIT_DATA = E1 + 85; // 306
    uint256 internal constant EMIT_SIZE = 36;
    uint256 internal constant END = EMIT_DATA + EMIT_SIZE; // 342

    error NotReferencedTransfer();
    error UnsupportedMode();
    error MalformedBatch();

    constructor(address multiSendCallOnly, address emitter) {
        MULTISEND = multiSendCallOnly;
        EMITTER = emitter;
    }

    function unwrap(address to, uint256 value, bytes calldata data, Enum.Operation operation)
        external
        view
        returns (UnwrappedTransaction[] memory result)
    {
        if (to != MULTISEND) revert NotReferencedTransfer();
        if (value != 0 || operation != Enum.Operation.DelegateCall) revert UnsupportedMode();

        // Header: canonical ABI encoding of multiSend(bytes) with exactly two entries.
        if (data.length != CALLDATA_BYTES) revert MalformedBatch();
        if (bytes4(data[0:4]) != MULTISEND_SELECTOR) revert MalformedBatch();
        if (uint256(bytes32(data[4:36])) != 0x20) revert MalformedBatch();
        if (uint256(bytes32(data[36:68])) != TX_BYTES) revert MalformedBatch();
        if (bytes14(data[END:CALLDATA_BYTES]) != bytes14(0)) revert MalformedBatch();

        // Entry 0: a plain call of transfer(address,uint256), no value. Its target is left to the role's
        // scopes, which admit only the origin token.
        if (uint8(data[E0]) != uint8(Enum.Operation.Call)) revert MalformedBatch();
        address transferTo = address(bytes20(data[E0 + 1:E0 + 21]));
        if (uint256(bytes32(data[E0 + 21:E0 + 53])) != 0) revert MalformedBatch();
        if (uint256(bytes32(data[E0 + 53:E0 + 85])) != TRANSFER_SIZE) revert MalformedBatch();
        if (bytes4(data[TRANSFER_DATA:TRANSFER_DATA + 4]) != TRANSFER_SELECTOR) revert MalformedBatch();

        // Entry 1: a plain call of ReferenceEmitter.emitReference(bytes32), no value.
        if (uint8(data[E1]) != uint8(Enum.Operation.Call)) revert MalformedBatch();
        if (address(bytes20(data[E1 + 1:E1 + 21])) != EMITTER) revert MalformedBatch();
        if (uint256(bytes32(data[E1 + 21:E1 + 53])) != 0) revert MalformedBatch();
        if (uint256(bytes32(data[E1 + 53:E1 + 85])) != EMIT_SIZE) revert MalformedBatch();
        if (bytes4(data[EMIT_DATA:EMIT_DATA + 4]) != EMIT_SELECTOR) revert MalformedBatch();

        result = new UnwrappedTransaction[](2);
        result[0] = UnwrappedTransaction({
            operation: Enum.Operation.Call,
            to: transferTo,
            value: 0,
            dataLocation: TRANSFER_DATA,
            dataSize: TRANSFER_SIZE
        });
        result[1] = UnwrappedTransaction({
            operation: Enum.Operation.Call,
            to: EMITTER,
            value: 0,
            dataLocation: EMIT_DATA,
            dataSize: EMIT_SIZE
        });
    }
}
