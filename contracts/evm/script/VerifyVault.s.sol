// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {Script, console} from "forge-std/Script.sol";
import {Vm} from "forge-std/Vm.sol";

import {DeployVault} from "./DeployVault.s.sol";
import {ReferenceEmitter} from "../src/ReferenceEmitter.sol";
import {ReferencedTransferUnwrapper} from "../src/ReferencedTransferUnwrapper.sol";
import {VaultRoles} from "../src/VaultRoles.sol";
import {ISafe, IRoles, IRolesEvents} from "../src/interfaces/External.sol";

/// @title VerifyVault (T-008-03; REQ-KY-4, REQ-KY-3, REQ-GV-3)
/// @notice Checks a deployed vault against its configuration before it is funded. Fails if any role, target,
/// function scope, allowance, unwrapper or member of the Roles modifier differs from what
/// `DeployVault.permissionCalls` sets for config/<NETWORK>.json and the governance parameters, or if the Safe,
/// the modifier or the vault's own contracts are not what the deployment makes.
///
///   NETWORK=polygon VAULT_OWNERS=0x..,0x.. VAULT_THRESHOLD=2 DELIVERY_MEMBER=0x.. CONVERSION_MEMBER=0x.. \
///   VAULT_SAFE=0x.. VAULT_ROLES=0x.. VAULT_EMITTER=0x.. VAULT_UNWRAPPER=0x.. VAULT_FROM_BLOCK=<creation> \
///   [VAULT_TO_BLOCK=..] [VAULT_LOG_RANGE=10000] \
///   forge script script/VerifyVault.s.sol --rpc-url <network>
///
/// Roles keeps its permissions in mappings that cannot be listed, so the check replays every event the
/// modifier has emitted since its creation (VAULT_FROM_BLOCK must be at or before the block that created it;
/// the check refuses a range that misses the creation). Each permission event must be one the configuration
/// produces, and each one the configuration produces must be there, one for one. Any other event the mastercopy
/// does not emit fails the check. It fails closed: a permission granted and then revoked during deployment
/// still fails it. What a list can be read for (owners, threshold, modules, owner, avatar, target) is read from
/// state.
contract VerifyVault is Script {
    /// One log of the Roles modifier, as the network or the test recorder returns it.
    struct Log {
        bytes32[] topics;
        bytes data;
    }

    address internal constant SENTINEL = address(0x1);
    // Safe's guard slot: keccak256("guard_manager.guard.address").
    uint256 internal constant GUARD_SLOT = 0x4a204f620c8c5ccdca3fd54d003badd85ba500436a431f0cbda4f558c93c34c8;
    // Safe's fallback handler slot: keccak256("fallback_manager.handler.address").
    uint256 internal constant FALLBACK_SLOT =
        0x6c9a6c4a39284e37ed1cf53d337577d14212a4870fb976a4366c693b939918d5;
    // OpenZeppelin upgradeable 4.x Initializable (5.x is IRolesEvents.Initialized(uint64)).
    bytes32 internal constant INITIALIZED_V4 = keccak256("Initialized(uint8)");

    function run() external {
        DeployVault deploy = new DeployVault();
        DeployVault.NetworkConfig memory cfg = deploy.load(vm.envString("NETWORK"));
        require(block.chainid == cfg.chainId, "VerifyVault: wrong chain");
        DeployVault.Governance memory gov = DeployVault.Governance({
            owners: vm.envAddress("VAULT_OWNERS", ","),
            threshold: vm.envUint("VAULT_THRESHOLD"),
            deliveryMember: vm.envAddress("DELIVERY_MEMBER"),
            conversionMember: vm.envOr("CONVERSION_MEMBER", address(0)),
            saltNonce: 0
        });
        DeployVault.Deployment memory d = DeployVault.Deployment({
            safe: vm.envAddress("VAULT_SAFE"),
            roles: vm.envAddress("VAULT_ROLES"),
            emitter: vm.envAddress("VAULT_EMITTER"),
            unwrapper: vm.envAddress("VAULT_UNWRAPPER"),
            moduleSetup: address(0)
        });
        Log[] memory logs = fetch(
            d.roles,
            vm.envUint("VAULT_FROM_BLOCK"),
            vm.envOr("VAULT_TO_BLOCK", block.number),
            vm.envOr("VAULT_LOG_RANGE", uint256(10_000))
        );
        check(deploy, cfg, gov, d, logs);
        console.log("VerifyVault: vault matches its configuration; Roles events replayed:", logs.length);
    }

    /// Every log of `roles` in [from, to], fetched in ranges of `range` blocks.
    function fetch(address roles, uint256 from, uint256 to, uint256 range)
        public
        returns (Log[] memory logs)
    {
        require(range > 0 && from <= to, "VerifyVault: bad block range");
        logs = new Log[](0);
        for (uint256 start = from; start <= to; start += range) {
            uint256 end = start + range - 1 > to ? to : start + range - 1;
            Vm.EthGetLogs[] memory got = vm.eth_getLogs(start, end, roles, new bytes32[](0));
            Log[] memory next = new Log[](logs.length + got.length);
            for (uint256 i; i < logs.length; ++i) {
                next[i] = logs[i];
            }
            for (uint256 i; i < got.length; ++i) {
                require(got[i].emitter == roles && !got[i].removed, "VerifyVault: foreign or removed log");
                next[logs.length + i] = Log(got[i].topics, got[i].data);
            }
            logs = next;
        }
    }

    /// Reverts unless the vault `d` is exactly what `deploy` makes for `cfg` and `gov`, given every log the
    /// Roles modifier has emitted since its creation, in order.
    function check(
        DeployVault deploy,
        DeployVault.NetworkConfig memory cfg,
        DeployVault.Governance memory gov,
        DeployVault.Deployment memory d,
        Log[] memory logs
    ) public {
        _checkPermissions(deploy.permissionCalls(cfg, gov, d), logs);
        _checkRoles(cfg, gov, d);
        _checkSafe(cfg, gov, d);
        _checkOwnCode(cfg, d);
    }

    // ------------------------------------------------------------------ permissions, from the events

    function _checkPermissions(bytes[] memory calls, Log[] memory logs) internal view {
        // What the configuration emits: one event per call, plus the EnabledModule that assignRoles emits for
        // a member that is not yet a module (every member is new: the script refuses a key in two roles).
        uint256 members;
        for (uint256 i; i < calls.length; ++i) {
            if (bytes4(calls[i]) == IRoles.assignRoles.selector) ++members;
        }
        bytes32[] memory topic = new bytes32[](calls.length + members);
        bytes32[] memory data = new bytes32[](calls.length + members);
        uint256 k;
        for (uint256 i; i < calls.length; ++i) {
            bytes memory args = _args(calls[i]);
            topic[k] = _eventOf(bytes4(calls[i]));
            data[k] = _digest(topic[k], args);
            ++k;
            if (bytes4(calls[i]) == IRoles.assignRoles.selector) {
                (address member,,) = abi.decode(args, (address, bytes32[], bool[]));
                topic[k] = IRolesEvents.EnabledModule.selector;
                data[k++] = keccak256(abi.encode(member));
            }
        }

        bool[] memory seen = new bool[](topic.length);
        bool created;
        for (uint256 i; i < logs.length; ++i) {
            require(logs[i].topics.length > 0, "VerifyVault: anonymous Roles event");
            bytes32 t = logs[i].topics[0];
            if (t == IRolesEvents.RolesModSetup.selector) {
                require(!created, "VerifyVault: modifier set up twice");
                created = true;
                continue;
            }
            if (_isStateEvent(t) || _isOperationEvent(t)) continue;
            require(_isPermissionEvent(t), "VerifyVault: unknown Roles event");
            require(logs[i].topics.length == 1, "VerifyVault: permission event with indexed topics");
            bytes32 h = _digest(t, logs[i].data);
            bool matched;
            for (uint256 j; j < topic.length; ++j) {
                if (!seen[j] && topic[j] == t && data[j] == h) {
                    seen[j] = matched = true;
                    break;
                }
            }
            require(matched, "VerifyVault: permission not in the configuration");
        }
        require(created, "VerifyVault: logs do not start at the modifier's creation");
        for (uint256 j; j < seen.length; ++j) {
            require(seen[j], "VerifyVault: configured permission missing");
        }
    }

    /// The event's data as compared. `setAllowance` with timestamp 0 records the block's timestamp (the
    /// allowance's refill clock, which starts at the deployment), so that one word is left out of the
    /// comparison; it must still lie in the past.
    function _digest(bytes32 t, bytes memory data) internal view returns (bytes32) {
        if (t != IRolesEvents.SetAllowance.selector) return keccak256(data);
        require(data.length == 6 * 32, "VerifyVault: malformed SetAllowance");
        (bytes32 key, uint128 bal, uint128 maxRefill, uint128 refill, uint64 period, uint64 timestamp) =
            abi.decode(data, (bytes32, uint128, uint128, uint128, uint64, uint64));
        // Only an order check against the current block, not a time bound.
        // forge-lint: disable-next-line(block-timestamp)
        require(timestamp <= block.timestamp, "VerifyVault: allowance clock in the future");
        return keccak256(abi.encode(key, bal, maxRefill, refill, period, uint64(0)));
    }

    function _eventOf(bytes4 selector) internal pure returns (bytes32) {
        if (selector == IRoles.scopeTarget.selector) return IRolesEvents.ScopeTarget.selector;
        if (selector == IRoles.scopeFunction.selector) return IRolesEvents.ScopeFunction.selector;
        if (selector == IRoles.allowFunction.selector) return IRolesEvents.AllowFunction.selector;
        if (selector == IRoles.setAllowance.selector) return IRolesEvents.SetAllowance.selector;
        if (selector == IRoles.setTransactionUnwrapper.selector) {
            return IRolesEvents.SetUnwrapAdapter.selector;
        }
        if (selector == IRoles.assignRoles.selector) return IRolesEvents.AssignRoles.selector;
        revert("VerifyVault: no event for a configured call");
    }

    function _isPermissionEvent(bytes32 t) internal pure returns (bool) {
        return t == IRolesEvents.AssignRoles.selector || t == IRolesEvents.SetDefaultRole.selector
            || t == IRolesEvents.AllowTarget.selector || t == IRolesEvents.RevokeTarget.selector
            || t == IRolesEvents.ScopeTarget.selector || t == IRolesEvents.AllowFunction.selector
            || t == IRolesEvents.RevokeFunction.selector || t == IRolesEvents.ScopeFunction.selector
            || t == IRolesEvents.SetAllowance.selector || t == IRolesEvents.SetUnwrapAdapter.selector
            || t == IRolesEvents.EnabledModule.selector || t == IRolesEvents.DisabledModule.selector;
    }

    /// Their effect is read from state by `_checkRoles`.
    function _isStateEvent(bytes32 t) internal pure returns (bool) {
        return t == IRolesEvents.OwnershipTransferred.selector || t == IRolesEvents.AvatarSet.selector
            || t == IRolesEvents.TargetSet.selector || t == IRolesEvents.Initialized.selector
            || t == INITIALIZED_V4;
    }

    /// Emitted by use, not by administration: they change no permission.
    function _isOperationEvent(bytes32 t) internal pure returns (bool) {
        return t == IRolesEvents.ConsumeAllowance.selector
            || t == IRolesEvents.ExecutionFromModuleSuccess.selector
            || t == IRolesEvents.ExecutionFromModuleFailure.selector
            || t == IRolesEvents.HashExecuted.selector || t == IRolesEvents.HashInvalidated.selector;
    }

    /// The call's arguments: what the matching event carries as its data.
    function _args(bytes memory call) internal pure returns (bytes memory args) {
        args = new bytes(call.length - 4);
        for (uint256 i; i < args.length; ++i) {
            args[i] = call[i + 4];
        }
    }

    // ------------------------------------------------------------------ state

    function _checkRoles(
        DeployVault.NetworkConfig memory cfg,
        DeployVault.Governance memory gov,
        DeployVault.Deployment memory d
    ) internal view {
        IRoles roles = IRoles(d.roles);
        // A ModuleProxyFactory 1.2.0 clone of the pinned mastercopy: no other code, no upgrade path.
        assert(d.roles.code.length > 0);
        require(
            keccak256(d.roles.code)
                == keccak256(
                    abi.encodePacked(
                        hex"363d3d373d3d3d363d73", cfg.rolesMastercopy, hex"5af43d82803e903d91602b57fd5bf3"
                    )
                ),
            "VerifyVault: Roles is not a clone of the mastercopy"
        );
        require(roles.owner() == d.safe, "VerifyVault: Roles not owned by the Safe");
        require(roles.avatar() == d.safe, "VerifyVault: Roles avatar is not the Safe");
        require(roles.target() == d.safe, "VerifyVault: Roles target is not the Safe");

        // Its modules are the role members, and nothing else.
        bool converts = cfg.venue != DeployVault.Venue.None;
        (address[] memory modules, address next) = roles.getModulesPaginated(SENTINEL, 10);
        require(next == SENTINEL, "VerifyVault: too many Roles modules");
        require(modules.length == (converts ? 2 : 1), "VerifyVault: Roles modules differ from the members");
        for (uint256 i; i < modules.length; ++i) {
            require(
                modules[i] == gov.deliveryMember || (converts && modules[i] == gov.conversionMember),
                "VerifyVault: Roles modules differ from the members"
            );
        }

        // The unwrappers (also in the events; read back as a cross-check).
        require(
            address(roles.unwrappers(_unwrapperKey(cfg.multiSendCallOnly, 0x8d80ff0a))) == d.unwrapper,
            "VerifyVault: MultiSend unwrapper"
        );
        require(
            address(roles.unwrappers(_unwrapperKey(cfg.originToken, VaultRoles.TRANSFER))) == d.unwrapper,
            "VerifyVault: transfer unwrapper"
        );
    }

    function _checkSafe(
        DeployVault.NetworkConfig memory cfg,
        DeployVault.Governance memory gov,
        DeployVault.Deployment memory d
    ) internal view {
        ISafe safe = ISafe(d.safe);
        require(
            address(uint160(uint256(vm.load(d.safe, bytes32(0))))) == cfg.safeSingleton,
            "VerifyVault: Safe singleton"
        );
        require(safe.getThreshold() == gov.threshold, "VerifyVault: Safe threshold");
        address[] memory got = safe.getOwners();
        require(got.length == gov.owners.length, "VerifyVault: Safe owners");
        for (uint256 i; i < gov.owners.length; ++i) {
            bool found;
            for (uint256 j; j < got.length; ++j) {
                if (got[j] == gov.owners[i]) found = true;
            }
            require(found, "VerifyVault: Safe owners");
        }
        (address[] memory modules, address next) = safe.getModulesPaginated(SENTINEL, 10);
        require(next == SENTINEL && modules.length == 1 && modules[0] == d.roles, "VerifyVault: Safe modules");
        require(uint256(bytes32(safe.getStorageAt(GUARD_SLOT, 1))) == 0, "VerifyVault: Safe has a guard");
        require(
            address(uint160(uint256(bytes32(safe.getStorageAt(FALLBACK_SLOT, 1))))) == cfg.fallbackHandler,
            "VerifyVault: Safe fallback handler"
        );
    }

    /// The emitter and the unwrapper are this repository's code, built here and compared byte for byte.
    function _checkOwnCode(DeployVault.NetworkConfig memory cfg, DeployVault.Deployment memory d) internal {
        require(d.emitter.codehash == address(new ReferenceEmitter()).codehash, "VerifyVault: emitter code");
        require(
            d.unwrapper.codehash
                == address(new ReferencedTransferUnwrapper(cfg.multiSendCallOnly, d.emitter)).codehash,
            "VerifyVault: unwrapper code"
        );
    }

    function _unwrapperKey(address to, bytes4 selector) internal pure returns (bytes32) {
        return bytes32(bytes20(to)) | (bytes32(selector) >> 160);
    }
}
