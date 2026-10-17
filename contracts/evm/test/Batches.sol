// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {ReferenceEmitter} from "../src/ReferenceEmitter.sol";
import {IERC20} from "../src/interfaces/External.sol";

/// MultiSend batch encodings used by the tests.
library Batches {
    bytes4 internal constant MULTISEND = 0x8d80ff0a; // multiSend(bytes)

    /// One packed MultiSend entry.
    function entry(uint8 operation, address to, uint256 value, bytes memory data)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encodePacked(operation, to, value, data.length, data);
    }

    function transferData(address to, uint256 amount) internal pure returns (bytes memory) {
        return abi.encodeCall(IERC20.transfer, (to, amount));
    }

    function emitData(bytes32 ref) internal pure returns (bytes memory) {
        return abi.encodeCall(ReferenceEmitter.emitReference, (ref));
    }

    /// multiSend(transactions) calldata.
    function multiSend(bytes memory transactions) internal pure returns (bytes memory) {
        return abi.encodeWithSelector(MULTISEND, transactions);
    }

    /// The delivery: (token.transfer(to, amount), emitter.emitReference(ref)).
    function delivery(address token, address to, uint256 amount, address emitter, bytes32 ref)
        internal
        pure
        returns (bytes memory)
    {
        return multiSend(
            bytes.concat(entry(0, token, 0, transferData(to, amount)), entry(0, emitter, 0, emitData(ref)))
        );
    }
}
