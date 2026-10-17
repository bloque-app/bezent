// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";
import {stdJson} from "forge-std/StdJson.sol";
import {ExecutionOptions} from "zodiac-roles/Types.sol";
import {ITransactionUnwrapper} from "zodiac-roles/adapters/Types.sol";

import {ReferenceEmitter} from "../src/ReferenceEmitter.sol";
import {ReferencedTransferUnwrapper} from "../src/ReferencedTransferUnwrapper.sol";
import {VaultModuleSetup} from "../src/VaultModuleSetup.sol";
import {VaultRoles} from "../src/VaultRoles.sol";
import {ISafe, ISafeProxyFactory, IModuleProxyFactory, IRoles} from "../src/interfaces/External.sol";

/// @title DeployVault (T-008-03)
/// @notice Deploys one origin network's reserve vault (plan DEC-10): a Safe v1.4.1 whose only module is a
/// Zodiac Roles v2 modifier with the delivery and conversion roles of `VaultRoles`, owned by the Safe.
///
///   NETWORK=polygon VAULT_OWNERS=0x..,0x..,0x.. VAULT_THRESHOLD=2 \
///   DELIVERY_MEMBER=0x.. CONVERSION_MEMBER=0x.. [VAULT_SALT_NONCE=..] \
///   forge script script/DeployVault.s.sol --rpc-url <fork> --sender <deployer> [--broadcast]
///
/// Network constants come from config/<NETWORK>.json; owners, threshold and members are parameters
/// (governance's; REQ-KY-5). The default salt nonce is derived from the network, the deployer and the
/// deployer's nonce at the start of the run, so no two deployments share one.
///
/// The deployer never owns an enabled modifier. The Safe's address is computed before the Safe exists
/// (CREATE2), so the Roles modifier is configured and handed to that address first, and only then is the
/// Safe created, born with the modifier as its only module. A run that stops midway leaves a modifier that
/// no Safe has enabled. What the deployer did while it owned the modifier is checked afterwards by
/// script/VerifyVault.s.sol, which must pass before the vault is funded.
contract DeployVault is Script {
    using stdJson for string;

    bytes4 internal constant MULTISEND = 0x8d80ff0a; // multiSend(bytes)

    enum Venue {
        None,
        UniswapV3SwapRouter02,
        AerodromeSlipstream
    }

    struct Convertible {
        string symbol;
        address token;
        uint128 balance;
        uint128 maxRefill;
        uint128 refill;
        uint64 period;
        // The declared pool (REQ-VN-4): its fee tier on Uniswap v3, its tick spacing on Slipstream. The role
        // admits swaps through this pool only; `poolAddress` is the pool those parameters name, recorded so
        // the fork tests can check it against the venue's factory.
        int24 pool;
        address poolAddress;
    }

    struct NetworkConfig {
        string network;
        uint256 chainId;
        string originSymbol;
        address originToken;
        uint8 originDecimals;
        uint128 deliveryBalance;
        uint128 deliveryMaxRefill;
        uint128 deliveryRefill;
        uint64 deliveryPeriod;
        Venue venue;
        address router;
        Convertible[] convertibles;
        address safeSingleton;
        address safeProxyFactory;
        address fallbackHandler;
        address multiSendCallOnly;
        address rolesMastercopy;
        address moduleProxyFactory;
    }

    struct Governance {
        address[] owners;
        uint256 threshold;
        address deliveryMember;
        address conversionMember;
        uint256 saltNonce;
    }

    struct Deployment {
        address safe;
        address roles;
        address emitter;
        address unwrapper;
        address moduleSetup;
    }

    function run() external returns (Deployment memory d) {
        NetworkConfig memory cfg = load(vm.envString("NETWORK"));
        require(block.chainid == cfg.chainId, "DeployVault: wrong chain");
        Governance memory gov = Governance({
            owners: vm.envAddress("VAULT_OWNERS", ","),
            threshold: vm.envUint("VAULT_THRESHOLD"),
            deliveryMember: vm.envAddress("DELIVERY_MEMBER"),
            conversionMember: vm.envOr("CONVERSION_MEMBER", address(0)),
            saltNonce: 0
        });
        vm.startBroadcast();
        (, address deployer,) = vm.readCallers();
        gov.saltNonce = vm.envOr("VAULT_SALT_NONCE", defaultSaltNonce(cfg.network, deployer));
        d = _deploy(cfg, gov, deployer);
        vm.stopBroadcast();
    }

    /// One salt nonce per (network, deployment): the deployer's nonce moves with every transaction, so a
    /// second run with the same parameters gets a new salt instead of colliding with the first vault.
    function defaultSaltNonce(string memory network, address deployer) public view returns (uint256) {
        return uint256(
            keccak256(abi.encode("bezent.vault", network, block.chainid, deployer, vm.getNonce(deployer)))
        );
    }

    /// Deploys as `deployer` (used by the fork tests, which have no broadcast sender).
    function deployAs(NetworkConfig memory cfg, Governance memory gov, address deployer)
        public
        returns (Deployment memory d)
    {
        vm.startBroadcast(deployer);
        d = _deploy(cfg, gov, deployer);
        vm.stopBroadcast();
    }

    /// Reads config/<network>.json.
    function load(string memory network) public view returns (NetworkConfig memory cfg) {
        string memory json = vm.readFile(string.concat(vm.projectRoot(), "/config/", network, ".json"));
        cfg.network = json.readString(".network");
        cfg.chainId = json.readUint(".chainId");
        cfg.originSymbol = json.readString(".originToken.symbol");
        cfg.originToken = json.readAddress(".originToken.address");
        cfg.originDecimals = uint8(json.readUint(".originToken.decimals"));
        cfg.deliveryBalance = uint128(json.readUint(".delivery.balance"));
        cfg.deliveryMaxRefill = uint128(json.readUint(".delivery.maxRefill"));
        cfg.deliveryRefill = uint128(json.readUint(".delivery.refill"));
        cfg.deliveryPeriod = uint64(json.readUint(".delivery.period"));

        bytes32 venue = keccak256(bytes(json.readString(".conversion.venue")));
        if (venue == keccak256("uniswap-v3-swaprouter02")) cfg.venue = Venue.UniswapV3SwapRouter02;
        else if (venue == keccak256("aerodrome-slipstream")) cfg.venue = Venue.AerodromeSlipstream;
        else require(venue == keccak256("none"), "DeployVault: unknown venue");
        cfg.router = json.readAddress(".conversion.router");

        uint256 n;
        while (vm.keyExistsJson(json, string.concat(".conversion.convertibles[", vm.toString(n), "]"))) ++n;
        cfg.convertibles = new Convertible[](n);
        // The field that names the pool: the fee tier on Uniswap v3, the tick spacing on Slipstream.
        string memory poolField = cfg.venue == Venue.UniswapV3SwapRouter02 ? ".fee" : ".tickSpacing";
        for (uint256 i; i < n; ++i) {
            string memory p = string.concat(".conversion.convertibles[", vm.toString(i), "]");
            Convertible memory c = cfg.convertibles[i];
            c.symbol = json.readString(string.concat(p, ".symbol"));
            c.token = json.readAddress(string.concat(p, ".address"));
            c.balance = uint128(json.readUint(string.concat(p, ".balance")));
            c.maxRefill = uint128(json.readUint(string.concat(p, ".maxRefill")));
            c.refill = uint128(json.readUint(string.concat(p, ".refill")));
            c.period = uint64(json.readUint(string.concat(p, ".period")));
            int256 pool = json.readInt(string.concat(p, poolField));
            require(pool > 0 && pool <= type(int24).max, "DeployVault: pool parameter out of range");
            // In range (checked above).
            // forge-lint: disable-next-line(unsafe-typecast)
            c.pool = int24(pool);
            c.poolAddress = json.readAddress(string.concat(p, ".pool"));
        }

        cfg.safeSingleton = json.readAddress(".safe.singleton");
        cfg.safeProxyFactory = json.readAddress(".safe.proxyFactory");
        cfg.fallbackHandler = json.readAddress(".safe.fallbackHandler");
        cfg.multiSendCallOnly = json.readAddress(".safe.multiSendCallOnly");
        cfg.rolesMastercopy = json.readAddress(".zodiac.rolesMastercopy");
        cfg.moduleProxyFactory = json.readAddress(".zodiac.moduleProxyFactory");
    }

    function _check(NetworkConfig memory cfg, Governance memory gov, address deployer) internal pure {
        // REQ-KY-5: several independent signers for every administrative action.
        require(gov.threshold >= 2, "DeployVault: threshold below 2");
        require(gov.owners.length >= gov.threshold, "DeployVault: fewer owners than threshold");
        // REQ-KY-4: the role keys are separate from each other and from governance.
        require(gov.deliveryMember != address(0), "DeployVault: no delivery member");
        // The deployer holds a key for the length of the run only: it is neither governance nor a role.
        require(deployer != gov.deliveryMember, "DeployVault: deployer is the delivery member");
        require(deployer != gov.conversionMember, "DeployVault: deployer is the conversion member");
        for (uint256 i; i < gov.owners.length; ++i) {
            require(gov.owners[i] != deployer, "DeployVault: deployer is an owner");
            require(gov.owners[i] != gov.deliveryMember, "DeployVault: delivery member is an owner");
            require(gov.owners[i] != gov.conversionMember, "DeployVault: conversion member is an owner");
        }
        if (cfg.venue == Venue.None) {
            require(cfg.convertibles.length == 0, "DeployVault: convertibles without a venue");
            require(gov.conversionMember == address(0), "DeployVault: conversion member without a venue");
        } else {
            require(cfg.router != address(0), "DeployVault: no router");
            require(cfg.convertibles.length > 0, "DeployVault: venue without convertibles");
            require(gov.conversionMember != address(0), "DeployVault: no conversion member");
            require(gov.conversionMember != gov.deliveryMember, "DeployVault: one key for two roles");
            // REQ-CV-7 (a): the conversion role never spends the origin token.
            for (uint256 i; i < cfg.convertibles.length; ++i) {
                require(
                    cfg.convertibles[i].token != cfg.originToken, "DeployVault: origin token as convertible"
                );
                // REQ-VN-4: every conversion goes through one declared pool.
                require(cfg.convertibles[i].pool > 0, "DeployVault: no declared pool");
                for (uint256 j; j < i; ++j) {
                    require(cfg.convertibles[i].token != cfg.convertibles[j].token, "DeployVault: duplicate");
                }
            }
        }
    }

    function _deploy(NetworkConfig memory cfg, Governance memory gov, address deployer)
        internal
        returns (Deployment memory d)
    {
        _check(cfg, gov, deployer);

        d.emitter = address(new ReferenceEmitter());
        d.unwrapper = address(new ReferencedTransferUnwrapper(cfg.multiSendCallOnly, d.emitter));
        d.moduleSetup = address(new VaultModuleSetup());

        // The Roles modifier first, owned by the deployer only while it is configured and enabled nowhere.
        d.roles = IModuleProxyFactory(cfg.moduleProxyFactory)
            .deployModule(
                cfg.rolesMastercopy,
                abi.encodeCall(IRoles.setUp, (abi.encode(deployer, deployer, deployer))),
                gov.saltNonce
            );

        // The Safe's initializer: born with the Roles modifier as its only module (VaultModuleSetup), no
        // guard. Its address follows from the initializer and the salt, so it is known before it exists.
        bytes memory setup = abi.encodeCall(
            ISafe.setup,
            (
                gov.owners,
                gov.threshold,
                d.moduleSetup,
                abi.encodeCall(VaultModuleSetup.enableModule, (d.roles)),
                cfg.fallbackHandler,
                address(0),
                0,
                payable(address(0))
            )
        );
        d.safe = predictSafe(cfg, setup, gov.saltNonce);

        IRoles roles = IRoles(d.roles);
        roles.setAvatar(d.safe);
        roles.setTarget(d.safe);

        bytes[] memory calls = permissionCalls(cfg, gov, d);
        for (uint256 i; i < calls.length; ++i) {
            (bool ok,) = d.roles.call(calls[i]);
            require(ok, "DeployVault: Roles call failed");
        }
        _beforeHandover(roles, cfg, d);

        // From here on only the Safe's owners, at the Safe's threshold, can change a role (REQ-GV-3). The
        // Safe does not exist yet, so the modifier is enabled nowhere while the deployer owns it.
        roles.transferOwnership(d.safe);
        address safe = ISafeProxyFactory(cfg.safeProxyFactory)
            .createProxyWithNonce(cfg.safeSingleton, setup, gov.saltNonce);
        require(safe == d.safe, "DeployVault: Safe not at its predicted address");
    }

    /// Hook between the configuration and the handover. Empty: the fork tests override it to plant a role
    /// the way a compromised deployer key would, and show that VerifyVault catches it.
    function _beforeHandover(IRoles, NetworkConfig memory, Deployment memory) internal virtual {}

    /// The Safe that SafeProxyFactory 1.4.1 `createProxyWithNonce(singleton, initializer, saltNonce)` creates:
    /// CREATE2 by the factory, salt `keccak256(keccak256(initializer) ‖ saltNonce)`, init code the factory's
    /// `proxyCreationCode()` followed by the singleton as one word. The deployment checks the result.
    function predictSafe(NetworkConfig memory cfg, bytes memory initializer, uint256 saltNonce)
        public
        view
        returns (address)
    {
        bytes32 salt = keccak256(abi.encodePacked(keccak256(initializer), saltNonce));
        bytes32 codeHash = keccak256(
            abi.encodePacked(
                ISafeProxyFactory(cfg.safeProxyFactory).proxyCreationCode(),
                uint256(uint160(cfg.safeSingleton))
            )
        );
        return address(
            uint160(uint256(keccak256(abi.encodePacked(bytes1(0xff), cfg.safeProxyFactory, salt, codeHash))))
        );
    }

    /// Every call that sets the vault's permissions on the Roles modifier, in order: the scopes, allowances
    /// and unwrappers of both roles, then the members. The deployment makes exactly these calls, and
    /// VerifyVault expects exactly the events they emit and nothing else.
    function permissionCalls(NetworkConfig memory cfg, Governance memory gov, Deployment memory d)
        public
        pure
        returns (bytes[] memory calls)
    {
        bool converts = cfg.venue != Venue.None;
        calls = new bytes[](8 + (converts ? 3 * cfg.convertibles.length + 3 : 0));
        uint256 k = _deliveryCalls(cfg, d, calls, 0);
        if (converts) k = _conversionCalls(cfg, calls, k);
        calls[k++] = _assign(gov.deliveryMember, VaultRoles.DELIVERY);
        if (converts) calls[k++] = _assign(gov.conversionMember, VaultRoles.CONVERSION);
        assert(k == calls.length);
    }

    /// Delivery: the referenced pair is the only batch, and a bare transfer of the origin token is refused.
    function _deliveryCalls(NetworkConfig memory cfg, Deployment memory d, bytes[] memory calls, uint256 k)
        internal
        pure
        returns (uint256)
    {
        bytes32 key = VaultRoles.deliveryAllowanceKey(cfg.originToken);
        ITransactionUnwrapper u = ITransactionUnwrapper(d.unwrapper);
        calls[k++] = abi.encodeCall(IRoles.scopeTarget, (VaultRoles.DELIVERY, cfg.originToken));
        calls[k++] = abi.encodeCall(
            IRoles.scopeFunction,
            (
                VaultRoles.DELIVERY,
                cfg.originToken,
                VaultRoles.TRANSFER,
                VaultRoles.transferConditions(key),
                ExecutionOptions.None
            )
        );
        calls[k++] = abi.encodeCall(IRoles.scopeTarget, (VaultRoles.DELIVERY, d.emitter));
        calls[k++] = abi.encodeCall(
            IRoles.allowFunction,
            (VaultRoles.DELIVERY, d.emitter, ReferenceEmitter.emitReference.selector, ExecutionOptions.None)
        );
        calls[k++] = abi.encodeCall(
            IRoles.setAllowance,
            (key, cfg.deliveryBalance, cfg.deliveryMaxRefill, cfg.deliveryRefill, cfg.deliveryPeriod, 0)
        );
        calls[k++] = abi.encodeCall(IRoles.setTransactionUnwrapper, (cfg.multiSendCallOnly, MULTISEND, u));
        calls[k++] = abi.encodeCall(IRoles.setTransactionUnwrapper, (cfg.originToken, VaultRoles.TRANSFER, u));
        return k;
    }

    /// Conversion: per convertible its approval scope and window, then the router's swap scope.
    function _conversionCalls(NetworkConfig memory cfg, bytes[] memory calls, uint256 k)
        internal
        pure
        returns (uint256)
    {
        uint256 n = cfg.convertibles.length;
        address[] memory tokens = new address[](n);
        int24[] memory pools = new int24[](n);
        for (uint256 i; i < n; ++i) {
            tokens[i] = cfg.convertibles[i].token;
            pools[i] = cfg.convertibles[i].pool;
            k = _convertibleCalls(cfg.convertibles[i], cfg.router, calls, k);
        }
        VaultRoles.SwapLayout memory layout = cfg.venue == Venue.UniswapV3SwapRouter02
            ? VaultRoles.uniswapLayout()
            : VaultRoles.slipstreamLayout();
        calls[k++] = abi.encodeCall(IRoles.scopeTarget, (VaultRoles.CONVERSION, cfg.router));
        calls[k++] = abi.encodeCall(
            IRoles.scopeFunction,
            (
                VaultRoles.CONVERSION,
                cfg.router,
                swapSelector(cfg.venue),
                VaultRoles.swapConditions(layout, cfg.originToken, tokens, pools),
                ExecutionOptions.None
            )
        );
        return k;
    }

    function _convertibleCalls(Convertible memory c, address router, bytes[] memory calls, uint256 k)
        internal
        pure
        returns (uint256)
    {
        calls[k++] = abi.encodeCall(IRoles.scopeTarget, (VaultRoles.CONVERSION, c.token));
        calls[k++] = abi.encodeCall(
            IRoles.scopeFunction,
            (
                VaultRoles.CONVERSION,
                c.token,
                VaultRoles.APPROVE,
                VaultRoles.approveConditions(router),
                ExecutionOptions.None
            )
        );
        calls[k++] = abi.encodeCall(
            IRoles.setAllowance,
            (VaultRoles.conversionAllowanceKey(c.token), c.balance, c.maxRefill, c.refill, c.period, 0)
        );
        return k;
    }

    function _assign(address member, bytes32 role) internal pure returns (bytes memory) {
        bytes32[] memory keys = new bytes32[](1);
        keys[0] = role;
        bool[] memory member_ = new bool[](1);
        member_[0] = true;
        return abi.encodeCall(IRoles.assignRoles, (member, keys, member_));
    }

    function swapSelector(Venue venue) public pure returns (bytes4) {
        if (venue == Venue.UniswapV3SwapRouter02) {
            return
                bytes4(
                    keccak256("exactInputSingle((address,address,uint24,address,uint256,uint256,uint160))")
                );
        }
        return bytes4(
            keccak256("exactInputSingle((address,address,int24,address,uint256,uint256,uint256,uint160))")
        );
    }
}
