// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {Enum} from "@gnosis.pm/safe-contracts/contracts/common/Enum.sol";
import {ConditionFlat, ExecutionOptions} from "zodiac-roles/Types.sol";
import {ITransactionUnwrapper} from "zodiac-roles/adapters/Types.sol";

// The subset of each deployed contract's ABI that bezent calls. Every address is pinned and verified in
// features/F-008-evm-contracts/NOTES.md; the signatures follow the pinned sources in lib/.

/// Safe v1.4.1 (lib/safe-smart-account, tag v1.4.1).
interface ISafe {
    function setup(
        address[] calldata owners,
        uint256 threshold,
        address to,
        bytes calldata data,
        address fallbackHandler,
        address paymentToken,
        uint256 payment,
        address payable paymentReceiver
    ) external;

    function execTransaction(
        address to,
        uint256 value,
        bytes calldata data,
        Enum.Operation operation,
        uint256 safeTxGas,
        uint256 baseGas,
        uint256 gasPrice,
        address gasToken,
        address payable refundReceiver,
        bytes memory signatures
    ) external payable returns (bool success);

    function getTransactionHash(
        address to,
        uint256 value,
        bytes calldata data,
        Enum.Operation operation,
        uint256 safeTxGas,
        uint256 baseGas,
        uint256 gasPrice,
        address gasToken,
        address refundReceiver,
        uint256 _nonce
    ) external view returns (bytes32);

    function nonce() external view returns (uint256);
    function getOwners() external view returns (address[] memory);
    function getThreshold() external view returns (uint256);
    function isModuleEnabled(address module) external view returns (bool);
    function getModulesPaginated(address start, uint256 pageSize)
        external
        view
        returns (address[] memory array, address next);
    function enableModule(address module) external;
    function disableModule(address prevModule, address module) external;
    function setGuard(address guard) external;
    function domainSeparator() external view returns (bytes32);
    function addOwnerWithThreshold(address owner, uint256 threshold) external;
    function getStorageAt(uint256 offset, uint256 length) external view returns (bytes memory);
}

/// SafeProxyFactory v1.4.1.
interface ISafeProxyFactory {
    function createProxyWithNonce(address singleton, bytes memory initializer, uint256 saltNonce)
        external
        returns (address proxy);
    function proxyCreationCode() external view returns (bytes memory);
}

/// Zodiac ModuleProxyFactory v1.2.0.
interface IModuleProxyFactory {
    function deployModule(address masterCopy, bytes memory initializer, uint256 saltNonce)
        external
        returns (address proxy);
}

/// Zodiac Roles Modifier v2.1.1 (lib/zodiac-modifier-roles at 218a5164, the verified mastercopy's source).
interface IRoles {
    function setUp(bytes memory initParams) external;
    function owner() external view returns (address);
    function avatar() external view returns (address);
    function target() external view returns (address);
    function transferOwnership(address newOwner) external;
    function setAvatar(address avatar) external;
    function setTarget(address target) external;

    function assignRoles(address module, bytes32[] calldata roleKeys, bool[] calldata memberOf) external;
    function scopeTarget(bytes32 roleKey, address targetAddress) external;
    function allowFunction(bytes32 roleKey, address targetAddress, bytes4 selector, ExecutionOptions options)
        external;
    function scopeFunction(
        bytes32 roleKey,
        address targetAddress,
        bytes4 selector,
        ConditionFlat[] memory conditions,
        ExecutionOptions options
    ) external;
    function setAllowance(
        bytes32 key,
        uint128 balance,
        uint128 maxRefill,
        uint128 refill,
        uint64 period,
        uint64 timestamp
    ) external;
    function allowances(bytes32 key)
        external
        view
        returns (uint128 refill, uint128 maxRefill, uint64 period, uint128 balance, uint64 timestamp);
    function setTransactionUnwrapper(address to, bytes4 selector, ITransactionUnwrapper adapter) external;
    function unwrappers(bytes32 key) external view returns (ITransactionUnwrapper);

    function execTransactionWithRole(
        address to,
        uint256 value,
        bytes calldata data,
        Enum.Operation operation,
        bytes32 roleKey,
        bool shouldRevert
    ) external returns (bool success);
    function execTransactionFromModule(
        address to,
        uint256 value,
        bytes calldata data,
        Enum.Operation operation
    ) external returns (bool success);

    function enableModule(address module) external;
    function disableModule(address prevModule, address module) external;
    function isModuleEnabled(address module) external view returns (bool);
    function getModulesPaginated(address start, uint256 pageSize)
        external
        view
        returns (address[] memory array, address next);
    function allowTarget(bytes32 roleKey, address targetAddress, ExecutionOptions options) external;
    function revokeTarget(bytes32 roleKey, address targetAddress) external;
    function revokeFunction(bytes32 roleKey, address targetAddress, bytes4 selector) external;
}

/// Zodiac Delay Modifier v1.1.1 (mastercopy 0x8241…dF01; contracts/Delay.sol and zodiac-core 3.x Modifier,
/// Module and SignatureChecker, as published in gnosisguild/zodiac `mastercopies/delay/1.1.1`).
interface IDelay {
    function setUp(bytes memory initParams) external;
    function owner() external view returns (address);
    function avatar() external view returns (address);
    function target() external view returns (address);
    function transferOwnership(address newOwner) external;
    function setAvatar(address avatar) external;
    function setTarget(address target) external;
    function enableModule(address module) external;
    function disableModule(address prevModule, address module) external;
    function isModuleEnabled(address module) external view returns (bool);
    function getModulesPaginated(address start, uint256 pageSize)
        external
        view
        returns (address[] memory array, address next);

    function setTxCooldown(uint256 txCooldown) external;
    function setTxExpiration(uint256 txExpiration) external;
    function setTxNonce(uint256 txNonce) external;
    function txCooldown() external view returns (uint256);
    function txExpiration() external view returns (uint256);
    function txNonce() external view returns (uint256);
    function queueNonce() external view returns (uint256);
    function txCreatedAt(uint256 nonce) external view returns (uint256);
    function txHash(uint256 nonce) external view returns (bytes32);

    /// Queues a transaction (the avatar's interface, so the Delay sits between a module and the avatar).
    function execTransactionFromModule(
        address to,
        uint256 value,
        bytes calldata data,
        Enum.Operation operation
    ) external returns (bool success);
    function execTransactionFromModuleReturnData(
        address to,
        uint256 value,
        bytes calldata data,
        Enum.Operation operation
    ) external returns (bool success, bytes memory returnData);
    /// Executes the oldest queued transaction once its cooldown has passed and before it expires. Anyone may call.
    function executeNextTx(address to, uint256 value, bytes calldata data, Enum.Operation operation) external;
    function skipExpired() external;
    function moduleTxHash(bytes calldata data, bytes32 salt) external view returns (bytes32);
}

/// Every event the Roles Modifier v2.1.1 mastercopy can emit (Roles.sol, PermissionBuilder.sol,
/// Periphery.sol and AllowanceTracker.sol at 218a5164; zodiac 3.4.1 Modifier, Module and ExecutionTracker;
/// OpenZeppelin upgradeable 5 Ownable and Initializable). None has an indexed argument but those noted, so
/// for the permission events the log data is exactly the arguments of the call that emitted it.
interface IRolesEvents {
    // Permissions: compared one for one with the configuration by VerifyVault.
    event AssignRoles(address module, bytes32[] roleKeys, bool[] memberOf);
    event SetDefaultRole(address module, bytes32 defaultRoleKey);
    event AllowTarget(bytes32 roleKey, address targetAddress, ExecutionOptions options);
    event RevokeTarget(bytes32 roleKey, address targetAddress);
    event ScopeTarget(bytes32 roleKey, address targetAddress);
    event AllowFunction(bytes32 roleKey, address targetAddress, bytes4 selector, ExecutionOptions options);
    event RevokeFunction(bytes32 roleKey, address targetAddress, bytes4 selector);
    event ScopeFunction(
        bytes32 roleKey,
        address targetAddress,
        bytes4 selector,
        ConditionFlat[] conditions,
        ExecutionOptions options
    );
    event SetAllowance(
        bytes32 allowanceKey,
        uint128 balance,
        uint128 maxRefill,
        uint128 refill,
        uint64 period,
        uint64 timestamp
    );
    event SetUnwrapAdapter(address to, bytes4 selector, ITransactionUnwrapper adapter);
    event EnabledModule(address module);
    event DisabledModule(address module);

    // Setup and ownership: VerifyVault checks the resulting state (owner, avatar, target) instead.
    event RolesModSetup(
        address indexed initiator, address indexed owner, address indexed avatar, address target
    );
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);
    event AvatarSet(address indexed previousAvatar, address indexed newAvatar);
    event TargetSet(address indexed previousTarget, address indexed newTarget);
    event Initialized(uint64 version);

    // Operation: they change no permission.
    event ConsumeAllowance(bytes32 allowanceKey, uint128 consumed, uint128 newBalance);
    event ExecutionFromModuleSuccess(address indexed module);
    event ExecutionFromModuleFailure(address indexed module);
    event HashExecuted(bytes32);
    event HashInvalidated(bytes32);
}

interface IERC20 {
    function transfer(address to, uint256 amount) external returns (bool);
    function transferFrom(address from, address to, uint256 amount) external returns (bool);
    function approve(address spender, uint256 amount) external returns (bool);
    function balanceOf(address account) external view returns (uint256);
    function allowance(address owner, address spender) external view returns (uint256);
    function decimals() external view returns (uint8);
    function symbol() external view returns (string memory);
}

/// Uniswap v3 SwapRouter02 (IV3SwapRouter): exact-input single hop, no deadline field.
interface IUniswapSwapRouter02 {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    struct ExactInputParams {
        bytes path;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
    }

    function exactInputSingle(ExactInputSingleParams calldata params)
        external
        payable
        returns (uint256 amountOut);
    function exactInput(ExactInputParams calldata params) external payable returns (uint256 amountOut);
    function factory() external view returns (address);
}

/// UniswapV3Factory: a pool per (token pair, fee tier).
interface IUniswapV3Factory {
    function getPool(address tokenA, address tokenB, uint24 fee) external view returns (address pool);
    function feeAmountTickSpacing(uint24 fee) external view returns (int24);
}

/// The in-range liquidity of a Uniswap v3 pool.
interface IConcentratedPool {
    function liquidity() external view returns (uint128);
}
