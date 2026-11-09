// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {Test, Vm} from "forge-std/Test.sol";
import {DirectedDeposit} from "../src/DirectedDeposit.sol";
import {PermitToken, SignerWallet} from "./PermitToken.sol";

/// T-018-01 (amendment 0029, A1; plan DEC-21, proposed), no fork: limits, the zero recipient, both permit forms
/// with a front-run permit, the allowance check, never a balance, and nothing between `Transfer` and `Directed`.
/// The fork suite (test/fork/DirectedDeposit.t.sol) proves the same against native USDC on Polygon.
contract DirectedDepositTest is Test {
    event Directed(address indexed sender, uint256 amount, bytes32 indexed recipient, address refund);

    bytes32 internal constant TRANSFER = keccak256("Transfer(address,address,uint256)");
    uint256 internal constant MIN = 1e6; // 1 USDC
    uint256 internal constant MAX = 50_000e6; // 50 000 USDC
    bytes32 internal constant RECIPIENT = keccak256("a Kreivo account");

    PermitToken internal token;
    DirectedDeposit internal directed;
    address internal reserve = makeAddr("reserve");
    uint256 internal userKey = 0xA11CE;
    address internal user;
    address internal other = makeAddr("other");
    /// The original depositor the caller names: not the caller (d-refund-dest-swaps).
    address internal refundTo = makeAddr("original depositor");

    function setUp() public {
        token = new PermitToken();
        directed = new DirectedDeposit(address(token), reserve, MIN, MAX);
        user = vm.addr(userKey);
        token.mint(user, 2 * MAX);
    }

    // ------------------------------------------------------------------ construction

    function test_DEC_21_immutables() public view {
        assertEq(address(directed.token()), address(token));
        assertEq(directed.reserve(), reserve);
        assertEq(directed.minimum(), MIN);
        assertEq(directed.maximum(), MAX);
    }

    function test_DEC_21_constructorRefusals() public {
        vm.expectRevert(DirectedDeposit.ZeroAddress.selector);
        new DirectedDeposit(address(0), reserve, MIN, MAX);
        vm.expectRevert(DirectedDeposit.ZeroAddress.selector);
        new DirectedDeposit(address(token), address(0), MIN, MAX);
        vm.expectRevert(DirectedDeposit.BadLimits.selector);
        new DirectedDeposit(address(token), reserve, 0, MAX);
        vm.expectRevert(DirectedDeposit.BadLimits.selector);
        new DirectedDeposit(address(token), reserve, MAX + 1, MAX);
    }

    // ------------------------------------------------------------------ deposit

    /// AC-A6.6, REQ-RN-5: exactly `amount` from the caller to the reserve, `Directed` the log right after the
    /// `Transfer`, and nothing stays in the contract.
    function testFuzz_AC_A6_6_depositMovesExactlyAndPairs(uint256 amount, bytes32 recipient) public {
        amount = bound(amount, MIN, MAX);
        vm.assume(recipient != bytes32(0));
        vm.prank(user);
        token.approve(address(directed), amount);

        uint256 before = token.balanceOf(user);
        vm.recordLogs();
        vm.prank(user);
        directed.deposit(amount, recipient, refundTo);
        Vm.Log[] memory logs = vm.getRecordedLogs();

        assertEq(token.balanceOf(user), before - amount);
        assertEq(token.balanceOf(reserve), amount);
        assertEq(token.balanceOf(address(directed)), 0);
        assertEq(logs.length, 2);
        assertPair(logs, 1, user, amount, recipient, refundTo);
    }

    /// AC-A6.5, REQ-LM-2: below the minimum or above the maximum reverts and moves nothing.
    function testFuzz_AC_A6_5_outOfRangeReverts(uint256 amount) public {
        vm.assume(amount < MIN || amount > MAX);
        vm.prank(user);
        token.approve(address(directed), type(uint256).max);
        vm.prank(user);
        vm.expectRevert(
            amount < MIN ? DirectedDeposit.BelowMinimum.selector : DirectedDeposit.AboveMaximum.selector
        );
        directed.deposit(amount, RECIPIENT, refundTo);
        assertEq(token.balanceOf(reserve), 0);
    }

    function test_AC_A6_5_limitsAreInclusive() public {
        vm.startPrank(user);
        token.approve(address(directed), MIN + MAX);
        directed.deposit(MIN, RECIPIENT, refundTo);
        directed.deposit(MAX, RECIPIENT, refundTo);
        vm.stopPrank();
        assertEq(token.balanceOf(reserve), MIN + MAX);
    }

    /// AC-A6.5: a zero recipient reverts, on every form.
    function test_AC_A6_5_zeroRecipientReverts() public {
        vm.startPrank(user);
        token.approve(address(directed), MIN);
        vm.expectRevert(DirectedDeposit.ZeroRecipient.selector);
        directed.deposit(MIN, bytes32(0), refundTo);
        vm.expectRevert(DirectedDeposit.ZeroRecipient.selector);
        directed.depositWithPermit(MIN, bytes32(0), refundTo, block.timestamp, 0, 0, 0);
        vm.expectRevert(DirectedDeposit.ZeroRecipient.selector);
        directed.depositWithPermit(MIN, bytes32(0), refundTo, block.timestamp, "");
        vm.stopPrank();
    }

    /// d-refund-dest-swaps: a zero refund, the reserve and the contract itself revert, each with its own error, on every
    /// form, and move nothing.
    function test_AC_A6_5_badRefundReverts() public {
        vm.startPrank(user);
        token.approve(address(directed), MIN);
        vm.expectRevert(DirectedDeposit.ZeroRefund.selector);
        directed.deposit(MIN, RECIPIENT, address(0));
        vm.expectRevert(DirectedDeposit.RefundIsReserve.selector);
        directed.deposit(MIN, RECIPIENT, reserve);
        vm.expectRevert(DirectedDeposit.RefundIsSelf.selector);
        directed.deposit(MIN, RECIPIENT, address(directed));

        (uint8 v, bytes32 r, bytes32 s) = sign(userKey, user, MIN, block.timestamp);
        vm.expectRevert(DirectedDeposit.ZeroRefund.selector);
        directed.depositWithPermit(MIN, RECIPIENT, address(0), block.timestamp, v, r, s);
        vm.expectRevert(DirectedDeposit.RefundIsReserve.selector);
        directed.depositWithPermit(MIN, RECIPIENT, reserve, block.timestamp, v, r, s);
        vm.expectRevert(DirectedDeposit.RefundIsSelf.selector);
        directed.depositWithPermit(MIN, RECIPIENT, address(directed), block.timestamp, v, r, s);

        vm.expectRevert(DirectedDeposit.ZeroRefund.selector);
        directed.depositWithPermit(MIN, RECIPIENT, address(0), block.timestamp, abi.encodePacked(r, s, v));
        vm.expectRevert(DirectedDeposit.RefundIsReserve.selector);
        directed.depositWithPermit(MIN, RECIPIENT, reserve, block.timestamp, abi.encodePacked(r, s, v));
        vm.expectRevert(DirectedDeposit.RefundIsSelf.selector);
        directed.depositWithPermit(
            MIN, RECIPIENT, address(directed), block.timestamp, abi.encodePacked(r, s, v)
        );
        vm.stopPrank();
        assertEq(token.balanceOf(reserve), 0);
    }

    /// d-refund-dest-swaps: the event's `refund` is the one the caller named, which need not be the caller; value is
    /// still pulled only from the caller. The refund may be the caller itself.
    function test_DEC_21_refundIsTheNamedAddressNotTheCaller() public {
        vm.prank(user);
        token.approve(address(directed), 2 * MIN);
        vm.recordLogs();
        vm.prank(user);
        directed.deposit(MIN, RECIPIENT, other);
        vm.prank(user);
        directed.deposit(MIN, RECIPIENT, user);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertPair(logs, 1, user, MIN, RECIPIENT, other);
        assertPair(logs, 3, user, MIN, RECIPIENT, user);
        assertEq(token.balanceOf(other), 0);
        assertEq(token.balanceOf(reserve), 2 * MIN);
    }

    function test_DEC_21_allowanceTooLowReverts() public {
        vm.startPrank(user);
        token.approve(address(directed), MIN - 1);
        vm.expectRevert(DirectedDeposit.AllowanceTooLow.selector);
        directed.deposit(MIN, RECIPIENT, refundTo);
        vm.stopPrank();
    }

    function test_DEC_21_falseTransferReverts() public {
        vm.prank(user);
        token.approve(address(directed), MIN);
        token.setFailTransfers(true);
        vm.prank(user);
        vm.expectRevert(DirectedDeposit.TransferFailed.selector);
        directed.deposit(MIN, RECIPIENT, refundTo);
    }

    /// No `receive` and no `fallback`: the contract refuses native value and unknown calls.
    function test_DEC_21_noReceiveNoFallback() public {
        vm.deal(user, 1 ether);
        vm.prank(user);
        (bool ok,) = address(directed).call{value: 1}("");
        assertFalse(ok);
        (ok,) = address(directed).call(abi.encodeWithSignature("sweep()"));
        assertFalse(ok);
    }

    // ------------------------------------------------------------------ permit

    function test_DEC_21_permitDeposits() public {
        (uint8 v, bytes32 r, bytes32 s) = sign(userKey, user, MIN, block.timestamp);
        vm.recordLogs();
        vm.prank(user);
        directed.depositWithPermit(MIN, RECIPIENT, refundTo, block.timestamp, v, r, s);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(logs.length, 3); // Approval, Transfer, Directed
        assertPair(logs, 2, user, MIN, RECIPIENT, refundTo);
        assertEq(token.balanceOf(reserve), MIN);
        assertEq(token.allowance(user, address(directed)), 0);
    }

    /// A permit someone else submits first does not stop the deposit.
    function test_DEC_21_frontRunPermitStillDeposits() public {
        (uint8 v, bytes32 r, bytes32 s) = sign(userKey, user, MIN, block.timestamp);
        vm.prank(other);
        token.permit(user, address(directed), MIN, block.timestamp, v, r, s);
        vm.prank(user);
        directed.depositWithPermit(MIN, RECIPIENT, refundTo, block.timestamp, v, r, s);
        assertEq(token.balanceOf(reserve), MIN);
    }

    /// The pull is from `msg.sender`: someone holding the user's signature cannot deposit the user's tokens.
    function test_DEC_21_stolenSignatureMovesNothing() public {
        (uint8 v, bytes32 r, bytes32 s) = sign(userKey, user, MIN, block.timestamp);
        vm.prank(other);
        vm.expectRevert(DirectedDeposit.AllowanceTooLow.selector);
        directed.depositWithPermit(MIN, RECIPIENT, refundTo, block.timestamp, v, r, s);
        assertEq(token.balanceOf(reserve), 0);
        assertEq(token.allowance(user, address(directed)), 0);
    }

    function test_DEC_21_badPermitWithoutAllowanceReverts() public {
        (uint8 v, bytes32 r, bytes32 s) = sign(0xB0B, user, MIN, block.timestamp);
        vm.prank(user);
        vm.expectRevert(DirectedDeposit.AllowanceTooLow.selector);
        directed.depositWithPermit(MIN, RECIPIENT, refundTo, block.timestamp, v, r, s);
    }

    function test_DEC_21_bytesPermitFromEoa() public {
        (uint8 v, bytes32 r, bytes32 s) = sign(userKey, user, MIN, block.timestamp);
        vm.prank(user);
        directed.depositWithPermit(MIN, RECIPIENT, refundTo, block.timestamp, abi.encodePacked(r, s, v));
        assertEq(token.balanceOf(reserve), MIN);
    }

    /// The bytes form serves an EIP-1271 contract wallet as depositor; the fork suite does it with a Safe.
    function test_DEC_21_bytesPermitFromContractWallet() public {
        SignerWallet wallet = new SignerWallet(user);
        token.mint(address(wallet), MIN);
        (uint8 v, bytes32 r, bytes32 s) = sign(userKey, address(wallet), MIN, block.timestamp);
        bytes memory call = abi.encodeWithSignature(
            "depositWithPermit(uint256,bytes32,address,uint256,bytes)",
            MIN,
            RECIPIENT,
            refundTo,
            block.timestamp,
            abi.encodePacked(r, s, v)
        );
        vm.prank(user);
        wallet.call(address(directed), call);
        assertEq(token.balanceOf(reserve), MIN);
        assertEq(token.balanceOf(address(wallet)), 0);
    }

    // ------------------------------------------------------------------ helpers

    function sign(uint256 key, address owner, uint256 value, uint256 deadline)
        internal
        view
        returns (uint8 v, bytes32 r, bytes32 s)
    {
        return vm.sign(key, token.digest(owner, address(directed), value, token.nonces(owner), deadline));
    }

    /// `logs[i]` is `Directed(sender, amount, recipient, refund)` from the contract, and `logs[i - 1]` the token's
    /// `Transfer(sender, reserve, amount)`.
    function assertPair(
        Vm.Log[] memory logs,
        uint256 i,
        address sender,
        uint256 amount,
        bytes32 recipient,
        address refund
    ) internal view {
        Vm.Log memory t = logs[i - 1];
        assertEq(t.emitter, address(token));
        assertEq(t.topics[0], TRANSFER);
        assertEq(address(uint160(uint256(t.topics[1]))), sender);
        assertEq(address(uint160(uint256(t.topics[2]))), reserve);
        assertEq(abi.decode(t.data, (uint256)), amount);
        Vm.Log memory dlog = logs[i];
        assertEq(dlog.emitter, address(directed));
        assertEq(dlog.topics[0], Directed.selector);
        assertEq(address(uint160(uint256(dlog.topics[1]))), sender);
        assertEq(dlog.topics[2], recipient);
        (uint256 a, address rf) = abi.decode(dlog.data, (uint256, address));
        assertEq(a, amount);
        assertEq(rf, refund);
    }
}
