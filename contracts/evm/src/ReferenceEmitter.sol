// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

/// @title ReferenceEmitter
/// @notice Stateless: its only function emits `Reference(bytes32)` (plan DEC-10). A vault's delivery is one
/// MultiSend of the origin-token transfer and a call to this contract, so the transfer and its on-chain
/// reference land in the same transaction or not at all (REQ-RN-5, REQ-ID-2, INV-14).
/// @dev `emit` is a Solidity keyword, so the function is `emitReference`. The reference is indexed so that
/// an observer can filter for it; the event signature stays `Reference(bytes32)`. Anyone may call this
/// contract: a reference means nothing unless the same transaction carries the vault's transfer, which only
/// the vault's delivery role can produce.
contract ReferenceEmitter {
    event Reference(bytes32 indexed ref);

    function emitReference(bytes32 ref) external {
        emit Reference(ref);
    }
}
