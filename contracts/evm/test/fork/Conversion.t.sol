// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {Enum} from "@gnosis.pm/safe-contracts/contracts/common/Enum.sol";

import {DeployVault} from "../../script/DeployVault.s.sol";
import {VaultRoles} from "../../src/VaultRoles.sol";
import {
    IERC20,
    IUniswapSwapRouter02,
    ISlipstreamSwapRouter,
    IUniswapV3Factory,
    ISlipstreamFactory,
    IConcentratedPool
} from "../../src/interfaces/External.sol";
import {Batches} from "../Batches.sol";
import {VaultFork} from "./VaultFork.sol";

/// Router calldata per venue.
library Venues {
    /// Uniswap v3 SwapRouter02 through the pool of fee tier `fee`.
    function uniswap(address tokenIn, address tokenOut, int24 fee, address recipient, uint256 amountIn)
        internal
        pure
        returns (bytes memory)
    {
        return abi.encodeCall(
            IUniswapSwapRouter02.exactInputSingle,
            (IUniswapSwapRouter02.ExactInputSingleParams(
                    tokenIn, tokenOut, uint24(fee), recipient, amountIn, 0, 0
                ))
        );
    }

    /// Aerodrome Slipstream through the pool of tick spacing `tickSpacing`.
    function slipstream(
        address tokenIn,
        address tokenOut,
        int24 tickSpacing,
        address recipient,
        uint256 amountIn
    ) internal view returns (bytes memory) {
        return abi.encodeCall(
            ISlipstreamSwapRouter.exactInputSingle,
            (ISlipstreamSwapRouter.ExactInputSingleParams(
                    tokenIn, tokenOut, tickSpacing, recipient, block.timestamp + 600, amountIn, 0, 0
                ))
        );
    }
}

/// T-008-05: the conversion role (REQ-CV-7). Exact-input single swaps of an admitted convertible into the
/// origin token, output to the vault, per-window allowance on the input; nothing else.
abstract contract ConversionFork is VaultFork {
    address internal convertible;
    uint256 internal unit;

    function setUp() public virtual override {
        super.setUp();
        convertible = cfg.convertibles[0].token;
        unit = 10 ** IERC20(convertible).decimals();
        fund(convertible, uint256(cfg.convertibles[0].maxRefill) * 3);
        fund(cfg.originToken, 1_000_000 * 10 ** cfg.originDecimals);
    }

    /// Router calldata for an exact-input single swap on this network's venue, through the pool whose fee
    /// tier or tick spacing is `pool`.
    function swapDataVia(address tokenIn, address tokenOut, int24 pool, address recipient, uint256 amountIn)
        internal
        view
        virtual
        returns (bytes memory);

    /// Router calldata through the declared pool of `tokenIn` (the first convertible's when it has none).
    function swapData(address tokenIn, address tokenOut, address recipient, uint256 amountIn)
        internal
        view
        returns (bytes memory)
    {
        return swapDataVia(tokenIn, tokenOut, declaredPool(tokenIn), recipient, amountIn);
    }

    function declaredPool(address token) internal view returns (int24) {
        for (uint256 i; i < cfg.convertibles.length; ++i) {
            if (cfg.convertibles[i].token == token) return cfg.convertibles[i].pool;
        }
        return cfg.convertibles[0].pool;
    }

    function swapVia(address tokenIn, int24 pool, uint256 amountIn) internal returns (bool, bytes memory) {
        return roleCall(
            conversion,
            VaultRoles.CONVERSION,
            cfg.router,
            0,
            swapDataVia(tokenIn, cfg.originToken, pool, d.safe, amountIn),
            Enum.Operation.Call
        );
    }

    function approve(address token, address spender, uint256 amount) internal returns (bool, bytes memory) {
        return roleCall(
            conversion,
            VaultRoles.CONVERSION,
            token,
            0,
            abi.encodeCall(IERC20.approve, (spender, amount)),
            Enum.Operation.Call
        );
    }

    function swap(address tokenIn, address tokenOut, address recipient, uint256 amountIn)
        internal
        returns (bool, bytes memory)
    {
        return roleCall(
            conversion,
            VaultRoles.CONVERSION,
            cfg.router,
            0,
            swapData(tokenIn, tokenOut, recipient, amountIn),
            Enum.Operation.Call
        );
    }
}

/// The REQ-CV-7 suite, run on each venue.
abstract contract ConversionTest is ConversionFork {
    /// The pool parameters the venue's factory knows: every fee tier or tick spacing a pool can have.
    function poolParams() internal view virtual returns (int24[] memory);

    /// The venue's pool of (tokenIn, origin token) with fee tier or tick spacing `pool`, or zero.
    function poolAt(address tokenIn, int24 pool) internal view virtual returns (address);

    /// REQ-VN-4: the review's scenario. A full window of the convertible (50 000 USDT0 on Polygon) through any
    /// pool but the declared one, such as the 1 % USDT0/USDC pool that held 1.3e6 of liquidity at the fork
    /// block and would have returned 0.13 USDC for it, is refused by the role. Nothing leaves the vault.
    function test_REQ_VN_4_undeclaredPoolIsRefused() public {
        uint256 amountIn = cfg.convertibles[0].balance; // the whole window: 50 000 units
        (bool ok, bytes memory ret) = approve(convertible, cfg.router, type(uint256).max);
        assertTrue(ok, "approve");
        uint256 inBefore = balance(convertible, d.safe);
        uint256 outBefore = balance(cfg.originToken, d.safe);

        int24[] memory params = poolParams();
        int24 declared = declaredPool(convertible);
        uint256 refused;
        for (uint256 i; i < params.length; ++i) {
            if (params[i] == declared) continue;
            (ok, ret) = swapVia(convertible, params[i], amountIn);
            assertViolation(ok, ret, PARAMETER_NOT_ALLOWED);
            ++refused;
        }
        // And a parameter no pool has yet: a pool created a block earlier at an unused tier or spacing.
        (ok, ret) = swapVia(convertible, 7, amountIn);
        assertViolation(ok, ret, PARAMETER_NOT_ALLOWED);

        assertGt(refused, 2, "the other pools were tried");
        assertEq(balance(convertible, d.safe), inBefore, "nothing spent");
        assertEq(balance(cfg.originToken, d.safe), outBefore, "nothing received");
        // The declared pool still converts.
        (ok, ret) = swapVia(convertible, declared, 1_000 * unit);
        assertTrue(ok, "declared pool");
    }

    /// REQ-VN-4: the declared pool is the one the venue's factory names for the configured parameter, and, at
    /// the fork block, the deepest pool of the pair (largest in-range liquidity of all its pools).
    function test_REQ_VN_4_declaredPoolIsTheDeepest() public view {
        DeployVault.Convertible memory c = cfg.convertibles[0];
        assertEq(poolAt(c.token, c.pool), c.poolAddress, "declared pool address");
        uint128 declared = IConcentratedPool(c.poolAddress).liquidity();
        assertGt(declared, 0, "declared pool has liquidity");
        int24[] memory params = poolParams();
        for (uint256 i; i < params.length; ++i) {
            address pool = poolAt(c.token, params[i]);
            if (pool == address(0) || pool == c.poolAddress) continue;
            assertLt(IConcentratedPool(pool).liquidity(), declared, "a deeper pool than the declared one");
        }
    }

    /// REQ-CV-7, REQ-CV-2: a conversion spends the convertible and its output lands in the vault.
    function test_REQ_CV_7_convertsIntoTheVault() public {
        uint256 amountIn = 1_000 * unit;
        uint256 inBefore = balance(convertible, d.safe);
        uint256 outBefore = balance(cfg.originToken, d.safe);

        (bool ok, bytes memory ret) = approve(convertible, cfg.router, amountIn);
        assertTrue(ok, "approve");
        (ok, ret) = swap(convertible, cfg.originToken, d.safe, amountIn);
        assertTrue(ok, "swap");

        assertEq(inBefore - balance(convertible, d.safe), amountIn, "input spent");
        assertGt(balance(cfg.originToken, d.safe), outBefore, "output in the vault");
    }

    /// REQ-CV-7 (a): the role never spends the origin token, by approval, swap, transfer or delivery batch.
    function test_REQ_CV_7_cannotSpendTheOriginToken() public {
        (bool ok, bytes memory ret) = approve(cfg.originToken, cfg.router, 1);
        assertViolation(ok, ret, TARGET_NOT_ALLOWED); // not a target of the role

        (ok, ret) = swap(cfg.originToken, convertible, d.safe, unit);
        assertViolation(ok, ret, PARAMETER_NOT_ALLOWED);
        (ok, ret) = swap(cfg.originToken, cfg.originToken, d.safe, unit);
        assertViolation(ok, ret, PARAMETER_NOT_ALLOWED);

        (ok, ret) = roleCall(
            conversion,
            VaultRoles.CONVERSION,
            cfg.originToken,
            0,
            Batches.transferData(outsider, 1),
            Enum.Operation.Call
        );
        assertRevertSelector(ok, ret, MALFORMED_MULTI_ENTRYPOINT);

        (ok, ret) = roleCall(
            conversion,
            VaultRoles.CONVERSION,
            cfg.multiSendCallOnly,
            0,
            Batches.delivery(cfg.originToken, outsider, 1, d.emitter, keccak256("x")),
            Enum.Operation.DelegateCall
        );
        assertViolation(ok, ret, TARGET_NOT_ALLOWED);
    }

    /// REQ-CV-7 (b): the output goes to the vault only; any other recipient, or the router's placeholders
    /// (address(1) "msg.sender", address(2) "this"), is refused.
    function test_REQ_CV_7_outputOnlyToTheVault() public {
        (bool ok, bytes memory ret) = approve(convertible, cfg.router, 100 * unit);
        assertTrue(ok);
        address[4] memory others = [outsider, address(1), address(2), cfg.router];
        for (uint256 i; i < others.length; ++i) {
            (ok, ret) = swap(convertible, cfg.originToken, others[i], unit);
            assertViolation(ok, ret, PARAMETER_NOT_ALLOWED);
        }
        // tokenOut other than the origin token.
        (ok, ret) = swap(convertible, convertible, d.safe, unit);
        assertViolation(ok, ret, PARAMETER_NOT_ALLOWED);
    }

    /// REQ-CV-7 (c): the input is bounded per window, and refills per period.
    function test_REQ_CV_7_boundedPerWindow() public {
        DeployVault.Convertible memory c = cfg.convertibles[0];
        (bool ok, bytes memory ret) = approve(convertible, cfg.router, type(uint256).max);
        assertTrue(ok);

        (ok, ret) = swap(convertible, cfg.originToken, d.safe, uint256(c.balance) + 1);
        assertViolation(ok, ret, ALLOWANCE_EXCEEDED);
        (ok, ret) = swap(convertible, cfg.originToken, d.safe, 10 * unit);
        assertTrue(ok, "within window");
        (ok, ret) = swap(convertible, cfg.originToken, d.safe, uint256(c.balance) - 10 * unit + 1);
        assertViolation(ok, ret, ALLOWANCE_EXCEEDED);

        vm.warp(block.timestamp + c.period);
        (ok, ret) = swap(convertible, cfg.originToken, d.safe, 20 * unit);
        assertTrue(ok, "after refill");
    }

    /// REQ-CV-7: approvals go to the router only, and the role reaches the router's single-hop exact-input
    /// swap only.
    function test_REQ_CV_7_onlyTheRoutersExactInputSingle() public {
        (bool ok, bytes memory ret) = approve(convertible, outsider, 1);
        assertViolation(ok, ret, PARAMETER_NOT_ALLOWED);

        // A plain transfer of the convertible out of the vault.
        (ok, ret) = roleCall(
            conversion,
            VaultRoles.CONVERSION,
            convertible,
            0,
            Batches.transferData(outsider, 1),
            Enum.Operation.Call
        );
        assertViolation(ok, ret, FUNCTION_NOT_ALLOWED);

        // Multi-hop, multicall and sweeping functions on the router.
        bytes[3] memory calls = [
            abi.encodeWithSelector(IUniswapSwapRouter02.exactInput.selector, bytes(""), d.safe, unit, 0),
            abi.encodeWithSelector(bytes4(keccak256("multicall(bytes[])")), new bytes[](0)),
            abi.encodeWithSelector(
                bytes4(keccak256("sweepToken(address,uint256,address)")), convertible, 0, outsider
            )
        ];
        for (uint256 i; i < calls.length; ++i) {
            (ok, ret) =
                roleCall(conversion, VaultRoles.CONVERSION, cfg.router, 0, calls[i], Enum.Operation.Call);
            assertViolation(ok, ret, FUNCTION_NOT_ALLOWED);
        }

        // No value, no delegatecall to the router.
        vm.deal(d.safe, 1 ether);
        (ok, ret) = roleCall(
            conversion,
            VaultRoles.CONVERSION,
            cfg.router,
            1,
            swapData(convertible, cfg.originToken, d.safe, unit),
            Enum.Operation.Call
        );
        assertViolation(ok, ret, SEND_NOT_ALLOWED);
        (ok, ret) = roleCall(
            conversion,
            VaultRoles.CONVERSION,
            cfg.router,
            0,
            swapData(convertible, cfg.originToken, d.safe, unit),
            Enum.Operation.DelegateCall
        );
        assertViolation(ok, ret, DELEGATECALL_NOT_ALLOWED);
    }
}

/// Polygon: Uniswap v3 SwapRouter02, USDT0 → USDC through the declared 0.01% pool.
contract PolygonConversionTest is ConversionTest {
    function network() internal pure override returns (string memory) {
        return "polygon";
    }

    function swapDataVia(address tokenIn, address tokenOut, int24 pool, address recipient, uint256 amountIn)
        internal
        pure
        override
        returns (bytes memory)
    {
        return Venues.uniswap(tokenIn, tokenOut, pool, recipient, amountIn);
    }

    /// Every fee tier the factory has enabled at the fork block, among the tiers Uniswap deployments use.
    function poolParams() internal view override returns (int24[] memory p) {
        uint24[9] memory tiers = [uint24(100), 200, 300, 400, 500, 2500, 3000, 5000, 10000];
        IUniswapV3Factory factory = IUniswapV3Factory(IUniswapSwapRouter02(cfg.router).factory());
        p = new int24[](tiers.length);
        uint256 n;
        for (uint256 i; i < tiers.length; ++i) {
            if (factory.feeAmountTickSpacing(tiers[i]) != 0) p[n++] = int24(tiers[i]);
        }
        assembly {
            mstore(p, n)
        }
    }

    function poolAt(address tokenIn, int24 fee) internal view override returns (address) {
        address factory = IUniswapSwapRouter02(cfg.router).factory();
        // forge-lint: disable-next-line(unsafe-typecast)
        return IUniswapV3Factory(factory).getPool(tokenIn, cfg.originToken, uint24(fee));
    }
}

/// Base: Aerodrome Slipstream, USDC → EURC through the declared tick-spacing-50 pool.
contract BaseConversionTest is ConversionTest {
    function network() internal pure override returns (string memory) {
        return "base";
    }

    function swapDataVia(address tokenIn, address tokenOut, int24 pool, address recipient, uint256 amountIn)
        internal
        view
        override
        returns (bytes memory)
    {
        return Venues.slipstream(tokenIn, tokenOut, pool, recipient, amountIn);
    }

    /// Every tick spacing the factory lists at the fork block (`tickSpacings()`).
    function poolParams() internal view override returns (int24[] memory) {
        return ISlipstreamFactory(ISlipstreamSwapRouter(cfg.router).factory()).tickSpacings();
    }

    function poolAt(address tokenIn, int24 tickSpacing) internal view override returns (address) {
        address factory = ISlipstreamSwapRouter(cfg.router).factory();
        return ISlipstreamFactory(factory).getPool(tokenIn, cfg.originToken, tickSpacing);
    }
}

/// A minimal token for the several-convertibles case: only authorisation is under test, so its swaps fail
/// in the router (no pool), after the role has admitted them.
contract MockConvertible {
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    function decimals() external pure returns (uint8) {
        return 6;
    }

    function symbol() external pure returns (string memory) {
        return "MOCK";
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }
}

/// Polygon with two admitted convertibles: one branch per token (an Or), each with its own window.
contract PolygonTwoConvertiblesTest is ConversionFork {
    bytes4 internal constant MODULE_TRANSACTION_FAILED = bytes4(keccak256("ModuleTransactionFailed()"));
    address internal second;

    function configure() internal override {
        second = address(new MockConvertible());
        cfg.convertibles
            .push(
                DeployVault.Convertible({
                    symbol: "MOCK",
                    token: second,
                    balance: 5e6,
                    maxRefill: 5e6,
                    refill: 5e6,
                    period: 1 days,
                    pool: 500,
                    poolAddress: address(0)
                })
            );
    }

    /// REQ-CV-7: every admitted convertible has its own branch and window; a miss is an Or violation.
    function test_REQ_CV_7_eachConvertibleItsOwnBranchAndWindow() public {
        (bool ok, bytes memory ret) = approve(second, cfg.router, 1e6);
        assertTrue(ok, "approve second");
        // Admitted (the router then fails: no pool), so the role passed it.
        (ok, ret) = swap(second, cfg.originToken, d.safe, 1e6);
        assertRevertSelector(ok, ret, MODULE_TRANSACTION_FAILED);
        // Over the second token's own window, though within the first's.
        (ok, ret) = swap(second, cfg.originToken, d.safe, 5e6 + 1);
        assertViolation(ok, ret, OR_VIOLATION);
        // Through a pool other than its own declared one (500), including the first convertible's (100).
        (ok, ret) = swapVia(second, 100, 1e6);
        assertViolation(ok, ret, OR_VIOLATION);
        (ok, ret) = swapVia(convertible, 500, 1e6);
        assertViolation(ok, ret, OR_VIOLATION);
        // Output elsewhere, on either branch.
        (ok, ret) = swap(second, cfg.originToken, outsider, 1);
        assertViolation(ok, ret, OR_VIOLATION);
        (ok, ret) = swap(convertible, cfg.originToken, outsider, 1);
        assertViolation(ok, ret, OR_VIOLATION);
        // The first convertible still converts.
        (ok, ret) = approve(convertible, cfg.router, 100 * unit);
        assertTrue(ok);
        (ok, ret) = swap(convertible, cfg.originToken, d.safe, 100 * unit);
        assertTrue(ok, "first");
    }

    function network() internal pure override returns (string memory) {
        return "polygon";
    }

    function swapDataVia(address tokenIn, address tokenOut, int24 pool, address recipient, uint256 amountIn)
        internal
        pure
        override
        returns (bytes memory)
    {
        return Venues.uniswap(tokenIn, tokenOut, pool, recipient, amountIn);
    }
}
