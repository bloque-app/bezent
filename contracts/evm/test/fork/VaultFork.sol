// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {Test, Vm} from "forge-std/Test.sol";
import {Enum} from "@gnosis.pm/safe-contracts/contracts/common/Enum.sol";

import {DeployVault} from "../../script/DeployVault.s.sol";
import {VaultRoles} from "../../src/VaultRoles.sol";
import {ISafe, IRoles, IERC20} from "../../src/interfaces/External.sol";
import {Batches} from "../Batches.sol";

/// Fork harness for T-008-03 to T-008-06: a fork at the block PR #19 pins (e2e/evm/forks.env), the vault
/// deployed by DeployVault exactly as on a real network, and helpers that act as the vault's roles and owners.
///
/// Upstream RPC: POLYGON_RPC_URL, BASE_RPC_URL, BSC_RPC_URL, else the same public archive endpoints as
/// e2e/evm/up.sh. Keys: Foundry's default test mnemonic only (index 0 deployer, 1–3 owners, 4 delivery,
/// 5 conversion, 6 outsider). Nothing is sent to a real network.
abstract contract VaultFork is Test {
    // e2e/evm/forks.env (F-013, PR #19).
    uint256 internal constant POLYGON_BLOCK = 95_400_000;
    uint256 internal constant BASE_BLOCK = 52_500_000;
    uint256 internal constant BSC_BLOCK = 127_300_000;

    string internal constant MNEMONIC = "test test test test test test test test test test test junk";

    // Roles v2.1.1 PermissionChecker.Status (lib/zodiac-modifier-roles, PermissionChecker.sol).
    uint8 internal constant DELEGATECALL_NOT_ALLOWED = 1;
    uint8 internal constant TARGET_NOT_ALLOWED = 2;
    uint8 internal constant FUNCTION_NOT_ALLOWED = 3;
    uint8 internal constant SEND_NOT_ALLOWED = 4;
    uint8 internal constant OR_VIOLATION = 5;
    uint8 internal constant PARAMETER_NOT_ALLOWED = 7;
    uint8 internal constant ALLOWANCE_EXCEEDED = 17;

    bytes4 internal constant CONDITION_VIOLATION = bytes4(keccak256("ConditionViolation(uint8,bytes32)"));
    bytes4 internal constant MALFORMED_MULTI_ENTRYPOINT = bytes4(keccak256("MalformedMultiEntrypoint()"));
    bytes4 internal constant NO_MEMBERSHIP = bytes4(keccak256("NoMembership()"));

    DeployVault internal script;
    DeployVault.NetworkConfig internal cfg;
    DeployVault.Deployment internal d;
    ISafe internal safe;
    IRoles internal roles;

    address internal deployer;
    address[] internal owners;
    uint256[] internal ownerKeys;
    address internal delivery;
    address internal conversion;
    address internal outsider;

    /// The network this suite forks: "polygon", "base" or "bsc".
    function network() internal pure virtual returns (string memory);

    function setUp() public virtual {
        _fork(network());
        script = new DeployVault();
        _store(script.load(network()));
        configure();
        _keys();
        d = script.deployAs(cfg, governance(), deployer);
        safe = ISafe(d.safe);
        roles = IRoles(d.roles);
    }

    /// Field-by-field copy (the legacy pipeline cannot copy a memory struct array to storage at once).
    function _store(DeployVault.NetworkConfig memory c) internal {
        DeployVault.Convertible[] memory list = c.convertibles;
        c.convertibles = new DeployVault.Convertible[](0);
        delete cfg.convertibles;
        cfg.network = c.network;
        cfg.chainId = c.chainId;
        cfg.originSymbol = c.originSymbol;
        cfg.originToken = c.originToken;
        cfg.originDecimals = c.originDecimals;
        cfg.deliveryBalance = c.deliveryBalance;
        cfg.deliveryMaxRefill = c.deliveryMaxRefill;
        cfg.deliveryRefill = c.deliveryRefill;
        cfg.deliveryPeriod = c.deliveryPeriod;
        cfg.venue = c.venue;
        cfg.router = c.router;
        cfg.safeSingleton = c.safeSingleton;
        cfg.safeProxyFactory = c.safeProxyFactory;
        cfg.fallbackHandler = c.fallbackHandler;
        cfg.multiSendCallOnly = c.multiSendCallOnly;
        cfg.rolesMastercopy = c.rolesMastercopy;
        cfg.moduleProxyFactory = c.moduleProxyFactory;
        for (uint256 i; i < list.length; ++i) {
            cfg.convertibles.push(list[i]);
        }
    }

    /// Hook for suites that change the network configuration before deployment.
    function configure() internal virtual {}

    function _fork(string memory net) internal {
        (string memory env, string memory fallbackUrl, uint256 blockNumber) = _forkOf(net);
        string memory url = vm.envOr(env, string(""));
        if (bytes(url).length == 0) url = fallbackUrl;
        vm.createSelectFork(url, blockNumber);
    }

    function _forkOf(string memory net) internal pure returns (string memory, string memory, uint256) {
        bytes32 n = keccak256(bytes(net));
        if (n == keccak256("polygon")) return ("POLYGON_RPC_URL", "https://polygon.drpc.org", POLYGON_BLOCK);
        if (n == keccak256("base")) return ("BASE_RPC_URL", "https://mainnet.base.org", BASE_BLOCK);
        if (n == keccak256("bsc")) {
            return ("BSC_RPC_URL", "https://bsc-mainnet.public.blastapi.io", BSC_BLOCK);
        }
        revert("unknown network");
    }

    function _keys() internal {
        deployer = vm.addr(vm.deriveKey(MNEMONIC, 0));
        delete owners;
        delete ownerKeys;
        for (uint32 i = 1; i <= 3; ++i) {
            uint256 k = vm.deriveKey(MNEMONIC, i);
            ownerKeys.push(k);
            owners.push(vm.addr(k));
        }
        delivery = vm.addr(vm.deriveKey(MNEMONIC, 4));
        conversion = vm.addr(vm.deriveKey(MNEMONIC, 5));
        outsider = vm.addr(vm.deriveKey(MNEMONIC, 6));
    }

    /// Governance parameters of the fork vault: 2 of 3 owners.
    function governance() internal view virtual returns (DeployVault.Governance memory) {
        return DeployVault.Governance({
            owners: owners,
            threshold: 2,
            deliveryMember: delivery,
            conversionMember: cfg.venue == DeployVault.Venue.None ? address(0) : conversion,
            saltNonce: uint256(keccak256(bytes(string.concat("bezent.vault.test.", network()))))
        });
    }

    // ------------------------------------------------------------------ acting as a role

    /// Calls the Roles modifier as `member` assuming `role`; returns the raw result.
    function roleCall(
        address member,
        bytes32 role,
        address to,
        uint256 value,
        bytes memory data,
        Enum.Operation op
    ) internal returns (bool ok, bytes memory ret) {
        vm.prank(member);
        (ok, ret) = address(roles)
            .call(abi.encodeCall(IRoles.execTransactionWithRole, (to, value, data, op, role, true)));
    }

    /// The delivery: one MultiSendCallOnly batch of (origin token transfer, reference).
    function deliver(address to, uint256 amount, bytes32 ref) internal returns (bool ok, bytes memory ret) {
        return roleCall(
            delivery,
            VaultRoles.DELIVERY,
            cfg.multiSendCallOnly,
            0,
            Batches.delivery(cfg.originToken, to, amount, d.emitter, ref),
            Enum.Operation.DelegateCall
        );
    }

    function assertViolation(bool ok, bytes memory ret, uint8 status) internal pure {
        assertFalse(ok, "expected a Roles condition violation");
        assertGe(ret.length, 68, "revert data");
        assertEq(bytes4(ret), CONDITION_VIOLATION, "not a ConditionViolation");
        bytes memory args = new bytes(ret.length - 4);
        for (uint256 i; i < args.length; ++i) {
            args[i] = ret[i + 4];
        }
        (uint8 got,) = abi.decode(args, (uint8, bytes32));
        assertEq(got, status, "violation status");
    }

    function assertRevertSelector(bool ok, bytes memory ret, bytes4 selector) internal pure {
        assertFalse(ok, "expected a revert");
        assertGe(ret.length, 4, "revert data");
        assertEq(bytes4(ret), selector, "revert selector");
    }

    // ------------------------------------------------------------------ acting as governance

    /// Executes a Safe transaction signed by the first `signers` owners (sorted by address, as Safe requires).
    function ownersExec(address to, bytes memory data, Enum.Operation op, uint256 signers)
        internal
        returns (bool)
    {
        bytes32 h = safe.getTransactionHash(to, 0, data, op, 0, 0, 0, address(0), address(0), safe.nonce());
        uint256[] memory keys = new uint256[](signers);
        for (uint256 i; i < signers; ++i) {
            keys[i] = ownerKeys[i];
        }
        // Sort by signer address, ascending.
        for (uint256 i; i < signers; ++i) {
            for (uint256 j = i + 1; j < signers; ++j) {
                if (vm.addr(keys[j]) < vm.addr(keys[i])) (keys[i], keys[j]) = (keys[j], keys[i]);
            }
        }
        bytes memory sigs;
        for (uint256 i; i < signers; ++i) {
            (uint8 v, bytes32 r, bytes32 s) = vm.sign(keys[i], h);
            sigs = bytes.concat(sigs, abi.encodePacked(r, s, v));
        }
        vm.prank(outsider); // anyone may relay a fully signed Safe transaction
        (bool ok,) = address(safe)
            .call(
                abi.encodeCall(
                    ISafe.execTransaction, (to, 0, data, op, 0, 0, 0, address(0), payable(address(0)), sigs)
                )
            );
        return ok;
    }

    // ------------------------------------------------------------------ helpers

    function fund(address token, uint256 amount) internal {
        deal(token, d.safe, amount);
    }

    function balance(address token, address who) internal view returns (uint256) {
        return IERC20(token).balanceOf(who);
    }

    /// Logs of `topic0` emitted by `emitter` among `logs`.
    function count(Vm.Log[] memory logs, address emitter, bytes32 topic0) internal pure returns (uint256 n) {
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].emitter == emitter && logs[i].topics.length > 0 && logs[i].topics[0] == topic0) ++n;
        }
    }
}
