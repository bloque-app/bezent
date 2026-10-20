// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {BaseGuard} from "@gnosis.pm/safe-contracts/contracts/base/GuardManager.sol";
import {Enum} from "@gnosis.pm/safe-contracts/contracts/common/Enum.sol";

import {IDelay, IRoles, ISafe} from "./interfaces/External.sol";

/// @title VaultGuard (plan DEC-10; REQ-GV-5, REQ-GV-3, REQ-KY-3)
/// @notice The Safe guard of a reserve vault: an allowlist for the owners' direct transactions. Safe 1.4.1 runs
/// a guard on `execTransaction` only, never on a module's transaction, so the guard has two jobs. It decides
/// what the owners may do at once, and it decides what the owners may queue in the Zodiac Delay modifier, whose
/// executions the guard never sees. Everything the guard does not name is refused.
///
/// Every owner transaction is a plain call with no value and no gas refund (a refund pays out of any token the
/// Safe holds, so it is a transfer). Then, by target:
///
/// - **Delay**: `execTransactionFromModule`, which queues a transaction (the Safe is a module of the Delay).
///   What is queued must itself be a plain call, with no value, to the Delay, the Roles modifier or the Safe
///   itself: a queued call to a token, to a router or to MultiSend is refused when it is queued, so the delay
///   cannot be used to move the vault's tokens. And `setTxNonce`, which skips queued transactions (the owners'
///   veto). Setting the cooldown or expiration, enabling a module and handing over ownership are not
///   offered: they reach the Delay only through the queue.
/// - **Roles**: only changes that narrow what a role may do, which are pauses. Revoking a member (`assignRoles`
///   with every flag false, for a module that is already enabled), disabling a module, revoking a target or a
///   function, and lowering an allowance. Adding a member, a scope, a function, a target, an unwrapper, a default
///   role, or raising an allowance, is not offered; it goes through the queue.
/// - **Safe**: `disableModule(prev, roles)` only, which pauses the vault's outflows. Every other change to the
///   Safe (owners, threshold, modules, guard, fallback handler) goes through the queue.
///
/// A queued call is held for the Delay's cooldown and expires after its expiration; anyone may execute it in
/// that window. After that the call runs as a module transaction, which no guard sees: what it may do is what
/// was allowed in the queue above (administrative targets only), and administration is what the cooldown covers.
///
/// Calldata is accepted only in its canonical ABI encoding (it is decoded and encoded again and compared), so
/// the guard and the target read the same arguments. The guard keeps no state and has no owner.
contract VaultGuard is BaseGuard {
    /// The Roles modifier and the Delay modifier of the vault this guard serves. The Safe is not a
    /// constructor argument (its address depends on this guard's): it is the caller, and must be the
    /// Delay's avatar.
    address public immutable roles;
    address public immutable delay;

    constructor(address roles_, address delay_) {
        require(roles_ != address(0) && delay_ != address(0), "VaultGuard: zero address");
        require(roles_ != delay_, "VaultGuard: one modifier twice");
        roles = roles_;
        delay = delay_;
    }

    /// Called by the Safe before an owners' transaction. Reverts unless the transaction is on the allowlist.
    function checkTransaction(
        address to,
        uint256 value,
        bytes calldata data,
        Enum.Operation operation,
        uint256, /* safeTxGas */
        uint256, /* baseGas */
        uint256 gasPrice,
        address, /* gasToken */
        address payable, /* refundReceiver */
        bytes memory, /* signatures */
        address /* msgSender */
    ) external view override {
        address safe = msg.sender;
        require(IDelay(delay).avatar() == safe, "VaultGuard: not the vault's Safe");
        require(operation == Enum.Operation.Call, "VaultGuard: delegatecall");
        require(value == 0, "VaultGuard: value");
        // With a gas price the Safe pays `gasToken` to `refundReceiver` itself: a transfer of any token it holds.
        require(gasPrice == 0, "VaultGuard: gas refund");
        require(data.length >= 4, "VaultGuard: no selector");

        if (to == delay) _checkDelay(safe, data);
        else if (to == roles) _checkRoles(data);
        else if (to == safe) _checkSafe(data);
        else revert("VaultGuard: not an administrative target");
    }

    /// Nothing to check after the fact: the allowlist above is the whole policy.
    function checkAfterExecution(bytes32, bool) external pure override {}

    // ------------------------------------------------------------------ Delay

    function _checkDelay(address safe, bytes calldata data) internal view {
        bytes4 selector = _selector(data);
        if (selector == IDelay.setTxNonce.selector) {
            // Skip queued transactions: cancels pending administration, never starts any.
            require(data.length == 36, "VaultGuard: malformed setTxNonce");
            return;
        }
        require(selector == IDelay.execTransactionFromModule.selector, "VaultGuard: Delay call not offered");
        (address queuedTo, uint256 queuedValue, bytes memory queuedData, Enum.Operation queuedOp) =
            abi.decode(data[4:], (address, uint256, bytes, Enum.Operation));
        _canonical(
            data,
            abi.encodeCall(IDelay.execTransactionFromModule, (queuedTo, queuedValue, queuedData, queuedOp))
        );
        require(queuedOp == Enum.Operation.Call, "VaultGuard: queued delegatecall");
        require(queuedValue == 0, "VaultGuard: queued value");
        require(queuedData.length >= 4, "VaultGuard: queued call without selector");
        require(
            queuedTo == delay || queuedTo == roles || queuedTo == safe,
            "VaultGuard: queued call to a non-administrative target"
        );
    }

    // ------------------------------------------------------------------ Roles

    function _checkRoles(bytes calldata data) internal view {
        bytes4 selector = _selector(data);
        if (selector == IRoles.assignRoles.selector) {
            (address module, bytes32[] memory keys, bool[] memory memberOf) =
                abi.decode(data[4:], (address, bytes32[], bool[]));
            _canonical(data, abi.encodeCall(IRoles.assignRoles, (module, keys, memberOf)));
            // Only revocations, and only for a module that exists: assignRoles enables a new module as a side effect.
            for (uint256 i; i < memberOf.length; ++i) {
                require(!memberOf[i], "VaultGuard: assignRoles adds a member");
            }
            require(IRoles(roles).isModuleEnabled(module), "VaultGuard: assignRoles enables a module");
        } else if (selector == IRoles.disableModule.selector) {
            (address prev, address module) = abi.decode(data[4:], (address, address));
            _canonical(data, abi.encodeCall(IRoles.disableModule, (prev, module)));
        } else if (selector == IRoles.revokeTarget.selector) {
            (bytes32 role, address target) = abi.decode(data[4:], (bytes32, address));
            _canonical(data, abi.encodeCall(IRoles.revokeTarget, (role, target)));
        } else if (selector == IRoles.revokeFunction.selector) {
            (bytes32 role, address target, bytes4 fn) = abi.decode(data[4:], (bytes32, address, bytes4));
            _canonical(data, abi.encodeCall(IRoles.revokeFunction, (role, target, fn)));
        } else if (selector == IRoles.setAllowance.selector) {
            _checkLowered(data);
        } else {
            revert("VaultGuard: Roles change must be delayed");
        }
    }

    /// An allowance may be lowered at once, never raised. "Lower" is monotone in every field, so the new
    /// allowance can never release more than the old one would have, at any later time:
    /// - the balance is at most the stored balance (the stored balance never exceeds the accrued one);
    /// - the cap on refills is at most the old cap, and is not zero (Roles reads zero as "no cap");
    /// - refills, if any, are no larger and no more frequent (an allowance with no period or no refill amount
    ///   does not refill; one that does not refill now may not start to);
    /// - the refill clock starts now (timestamp 0), never in the past (an old clock refills at once).
    function _checkLowered(bytes calldata data) internal view {
        (bytes32 key, uint128 balance, uint128 maxRefill, uint128 refill, uint64 period, uint64 timestamp) =
            abi.decode(data[4:], (bytes32, uint128, uint128, uint128, uint64, uint64));
        _canonical(
            data, abi.encodeCall(IRoles.setAllowance, (key, balance, maxRefill, refill, period, timestamp))
        );
        (uint128 oldRefill, uint128 oldMaxRefill, uint64 oldPeriod, uint128 oldBalance,) =
            IRoles(roles).allowances(key);
        require(timestamp == 0, "VaultGuard: allowance clock set");
        require(maxRefill != 0 && maxRefill <= oldMaxRefill, "VaultGuard: allowance cap raised");
        require(balance <= oldBalance, "VaultGuard: allowance balance raised");
        bool refills = period != 0 && refill != 0;
        if (refills) {
            bool refilled = oldPeriod != 0 && oldRefill != 0;
            require(
                refilled && period >= oldPeriod && refill <= oldRefill, "VaultGuard: allowance refill raised"
            );
        }
    }

    // ------------------------------------------------------------------ Safe

    function _checkSafe(bytes calldata data) internal view {
        require(_selector(data) == ISafe.disableModule.selector, "VaultGuard: Safe change must be delayed");
        (address prev, address module) = abi.decode(data[4:], (address, address));
        _canonical(data, abi.encodeCall(ISafe.disableModule, (prev, module)));
        // The pause: the Roles modifier stops being a module, so no role can move anything. Removing the Delay
        // (or anything else) is a widening of what the owners can do directly, so it goes through the queue.
        require(module == roles, "VaultGuard: only the Roles modifier may be disabled at once");
    }

    /// The first four bytes. Every caller has checked `data.length >= 4`.
    function _selector(bytes calldata data) internal pure returns (bytes4) {
        // forge-lint: disable-next-line(unsafe-typecast)
        return bytes4(data[:4]);
    }

    function _canonical(bytes calldata data, bytes memory expected) internal pure {
        require(keccak256(data) == keccak256(expected), "VaultGuard: non-canonical calldata");
    }
}
