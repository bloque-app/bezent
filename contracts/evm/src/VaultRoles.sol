// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {ConditionFlat, ParameterType, Operator} from "zodiac-roles/Types.sol";

/// @title VaultRoles
/// @notice The vault's two Roles v2 roles, as condition trees (plan DEC-10, DEC-13). Used by the deployment
/// script and checked by the fork tests; nothing here is deployed on its own.
///
/// Delivery (bezent's signer key; REQ-KY-3, REQ-RN-5, INV-14, AC-I1.1):
///   origin token `transfer(any recipient, amount within the delivery allowance)` and
///   `ReferenceEmitter.emitReference(any)`, reachable only as the pair that ReferencedTransferUnwrapper
///   admits. No value, no delegatecall but the MultiSendCallOnly batch, no other target.
///
/// Guardian (an independent key, held in a second Roles instance; REQ-GV-2, REQ-KY-4): on the Delay,
///   `setTxNonce(any)` (veto a queued change), and on the Safe `disableModule(any prev, the delivery Roles
///   instance)` (pause the vault). No value, no delegatecall, no other target or function, and no unwrapper.
///
/// Conversion (the requester's key; REQ-CV-7, REQ-VN-4):
///   per admitted convertible token, `approve(router, any)`, and on the router `exactInputSingle` with
///   tokenIn that convertible, tokenOut the origin token, the pool pinned (the fee tier of the Uniswap v3
///   pool declared for it), recipient the vault (EqualToAvatar), and amountIn within that convertible's
///   allowance. Only Polygon has this role (DEC-13). No value, no delegatecall, no other target or function.
library VaultRoles {
    // Role keys are short ASCII names, padded right (as the Roles SDK encodes them).
    // forge-lint: disable-next-line(unsafe-typecast)
    bytes32 internal constant DELIVERY = bytes32("bezent.delivery");
    // forge-lint: disable-next-line(unsafe-typecast)
    bytes32 internal constant CONVERSION = bytes32("bezent.conversion");

    // forge-lint: disable-next-line(unsafe-typecast)
    bytes32 internal constant GUARDIAN = bytes32("bezent.guardian");

    bytes4 internal constant TRANSFER = 0xa9059cbb; // transfer(address,uint256)
    bytes4 internal constant APPROVE = 0x095ea7b3; // approve(address,uint256)

    /// The allowance a delivery of `token` consumes.
    function deliveryAllowanceKey(address token) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked("bezent.delivery.allowance", token));
    }

    /// The allowance a conversion spending `tokenIn` consumes.
    function conversionAllowanceKey(address tokenIn) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked("bezent.conversion.allowance", tokenIn));
    }

    /// transfer(address to, uint256 amount): any `to`, `amount` within `allowanceKey`.
    function transferConditions(bytes32 allowanceKey) internal pure returns (ConditionFlat[] memory c) {
        c = new ConditionFlat[](3);
        c[0] = ConditionFlat(0, ParameterType.Calldata, Operator.Matches, "");
        c[1] = ConditionFlat(0, ParameterType.Static, Operator.Pass, "");
        c[2] = ConditionFlat(0, ParameterType.Static, Operator.WithinAllowance, abi.encode(allowanceKey));
    }

    /// approve(address spender, uint256 amount): `spender` must be the router. The approval moves nothing by
    /// itself: the router pulls only from its caller, and the vault calls the router only under the swap
    /// scope below, whose amountIn is the bounded quantity.
    function approveConditions(address router) internal pure returns (ConditionFlat[] memory c) {
        c = new ConditionFlat[](3);
        c[0] = ConditionFlat(0, ParameterType.Calldata, Operator.Matches, "");
        c[1] = ConditionFlat(0, ParameterType.Static, Operator.EqualTo, abi.encode(router));
        c[2] = ConditionFlat(0, ParameterType.Static, Operator.Pass, "");
    }

    /// disableModule(address prevModule, address module): any `prevModule` (the Safe checks it is the real
    /// predecessor), `module` the delivery Roles instance and nothing else, so the guardian can switch the
    /// delivery role off and cannot remove the Delay, itself, or any other module.
    function pauseConditions(address deliveryRoles) internal pure returns (ConditionFlat[] memory c) {
        c = new ConditionFlat[](3);
        c[0] = ConditionFlat(0, ParameterType.Calldata, Operator.Matches, "");
        c[1] = ConditionFlat(0, ParameterType.Static, Operator.Pass, "");
        c[2] = ConditionFlat(0, ParameterType.Static, Operator.EqualTo, abi.encode(deliveryRoles));
    }

    /// Field positions of an exact-input single-hop params struct (all fields static, so the tuple is inline).
    struct SwapLayout {
        uint8 fields;
        uint8 tokenIn;
        uint8 tokenOut;
        uint8 pool; // fee tier: with the tokens, it names the pool
        uint8 recipient;
        uint8 amountIn;
    }

    /// Uniswap v3 SwapRouter02: (tokenIn, tokenOut, fee, recipient, amountIn, amountOutMinimum, sqrtPriceLimitX96).
    function uniswapLayout() internal pure returns (SwapLayout memory) {
        return SwapLayout({fields: 7, tokenIn: 0, tokenOut: 1, pool: 2, recipient: 3, amountIn: 4});
    }

    /// exactInputSingle(params): one branch per admitted convertible (an Or when there are several), each
    /// pinning tokenIn, tokenOut = origin, the declared pool's fee tier (`pools[i]`;
    /// REQ-VN-4: value goes only to a declared venue, and on this router the pool is the venue), recipient
    /// = avatar, amountIn within the convertible's allowance. The remaining fields (deadline, minimum output,
    /// price limit) are the requester's (REQ-CV-5).
    function swapConditions(
        SwapLayout memory l,
        address originToken,
        address[] memory convertibles,
        int24[] memory pools
    ) internal pure returns (ConditionFlat[] memory c) {
        uint256 n = convertibles.length;
        require(n > 0, "VaultRoles: no convertible");
        require(pools.length == n, "VaultRoles: one pool per convertible");
        bool branched = n > 1;
        uint256 head = branched ? 2 + n : 2; // root, [Or], tuples
        // Parents are uint8 in ConditionFlat: every node index must fit.
        require(head + n * l.fields <= 256, "VaultRoles: too many convertibles");
        c = new ConditionFlat[](head + n * l.fields);
        c[0] = ConditionFlat(0, ParameterType.Calldata, Operator.Matches, "");
        if (branched) c[1] = ConditionFlat(0, ParameterType.None, Operator.Or, "");
        for (uint256 i; i < n; ++i) {
            uint256 tuple = branched ? 2 + i : 1;
            c[tuple] = ConditionFlat(branched ? 1 : 0, ParameterType.Tuple, Operator.Matches, "");
            // Fits: every index is below 256 (checked above).
            // forge-lint: disable-next-line(unsafe-typecast)
            uint8 parent = uint8(tuple);
            for (uint256 f; f < l.fields; ++f) {
                ConditionFlat memory node = ConditionFlat(parent, ParameterType.Static, Operator.Pass, "");
                if (f == l.tokenIn) {
                    node.operator = Operator.EqualTo;
                    node.compValue = abi.encode(convertibles[i]);
                } else if (f == l.tokenOut) {
                    node.operator = Operator.EqualTo;
                    node.compValue = abi.encode(originToken);
                } else if (f == l.pool) {
                    // uint24 fee is one ABI word (the pool parameter is carried as int24).
                    node.operator = Operator.EqualTo;
                    node.compValue = abi.encode(int256(pools[i]));
                } else if (f == l.recipient) {
                    node.operator = Operator.EqualToAvatar;
                } else if (f == l.amountIn) {
                    node.operator = Operator.WithinAllowance;
                    node.compValue = abi.encode(conversionAllowanceKey(convertibles[i]));
                }
                c[head + i * l.fields + f] = node;
            }
        }
    }
}
