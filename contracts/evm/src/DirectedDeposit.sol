// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

/// The calls `DirectedDeposit` makes on the origin token: native USDC (FiatToken 2.2) on Polygon, which has both
/// permit forms (features/F-018-directed-deposits/NOTES.md).
interface IPermitToken {
    /// EIP-2612.
    function permit(
        address owner,
        address spender,
        uint256 value,
        uint256 deadline,
        uint8 v,
        bytes32 r,
        bytes32 s
    ) external;

    /// FiatToken 2.2's bytes-signature form: an EOA's 65-byte signature, or an EIP-1271 contract wallet's.
    function permit(address owner, address spender, uint256 value, uint256 deadline, bytes calldata signature)
        external;

    function allowance(address owner, address spender) external view returns (uint256);
    function transferFrom(address from, address to, uint256 amount) external returns (bool);
}

/// @title DirectedDeposit (F-018, T-018-01; amendment 0029, A1; plan DEC-21, proposed)
/// @notice The directing code for one reserve: in one call it moves `amount` of the origin token from the caller
/// straight into the reserve (the vault Safe of DEC-10) and emits `Directed`, which names the recipient on the
/// issuing network. The `Transfer` log is always the one right before `Directed`, so the EVM endpoint pairs them
/// into one directed arrival (amendment 0029, A4; REQ-RN-5).
/// @dev Never holds value: the token goes from the caller to the reserve in one `transferFrom`. No owner, no
/// upgrade, no proxy, no `receive`, no `fallback` and no sweep; token, reserve and limits are immutable, and any
/// change is a new contract at a new address. Every amount is pulled from `msg.sender`: no function takes an owner,
/// so a permit someone else submits first only sets an allowance this contract then uses. Every function takes the
/// `refund` address, the original depositor, which is where a return goes (the owner's ruling `d-refund-dest-swaps`,
/// 2026-11-09). The funder chooses it, so bezent never chooses a refund destination (REQ-RF-2); it is never the zero
/// address, the reserve or this contract.
/// Tokens sent to this contract directly are lost (amendment 0029, "Stray tokens").
contract DirectedDeposit {
    /// The admitted origin token.
    IPermitToken public immutable token;
    /// The reserve address: the vault.
    address public immutable reserve;
    /// The route's minimum and per-transfer maximum, in the token's minor units (REQ-LM-1, REQ-LM-2).
    uint256 public immutable minimum;
    uint256 public immutable maximum;

    /// `sender` moved `amount` into the reserve for `recipient`, a 32-byte account on the issuing network. `refund` is
    /// where a return of it goes: the original depositor, named by the caller (not necessarily `sender`).
    event Directed(address indexed sender, uint256 amount, bytes32 indexed recipient, address refund);

    error ZeroAddress();
    error BadLimits();
    error BelowMinimum();
    error AboveMaximum();
    error ZeroRecipient();
    error ZeroRefund();
    error RefundIsReserve();
    error RefundIsSelf();
    error AllowanceTooLow();
    error TransferFailed();

    constructor(address token_, address reserve_, uint256 minimum_, uint256 maximum_) {
        if (token_ == address(0) || reserve_ == address(0)) revert ZeroAddress();
        if (minimum_ == 0 || minimum_ > maximum_) revert BadLimits();
        token = IPermitToken(token_);
        reserve = reserve_;
        minimum = minimum_;
        maximum = maximum_;
    }

    /// Deposits with an allowance the caller already gave this contract (`approve`, then this).
    function deposit(uint256 amount, bytes32 recipient, address refund) external {
        _check(amount, recipient, refund);
        _pull(amount, recipient, refund);
    }

    /// Deposits with an EIP-2612 permit for this contract, in the same transaction.
    function depositWithPermit(
        uint256 amount,
        bytes32 recipient,
        address refund,
        uint256 deadline,
        uint8 v,
        bytes32 r,
        bytes32 s
    ) external {
        _check(amount, recipient, refund);
        // A permit someone submitted first makes this one revert; the allowance it set is what counts.
        try token.permit(msg.sender, address(this), amount, deadline, v, r, s) {} catch {}
        _pull(amount, recipient, refund);
    }

    /// Deposits with a bytes-signature permit, which an EIP-1271 contract wallet (a Safe) can give.
    function depositWithPermit(
        uint256 amount,
        bytes32 recipient,
        address refund,
        uint256 deadline,
        bytes calldata signature
    ) external {
        _check(amount, recipient, refund);
        try token.permit(msg.sender, address(this), amount, deadline, signature) {} catch {}
        _pull(amount, recipient, refund);
    }

    function _check(uint256 amount, bytes32 recipient, address refund) internal view {
        if (amount < minimum) revert BelowMinimum();
        if (amount > maximum) revert AboveMaximum();
        if (recipient == bytes32(0)) revert ZeroRecipient();
        if (refund == address(0)) revert ZeroRefund();
        if (refund == reserve) revert RefundIsReserve();
        if (refund == address(this)) revert RefundIsSelf();
    }

    /// The transfer, then the event, with nothing between them.
    function _pull(uint256 amount, bytes32 recipient, address refund) internal {
        if (token.allowance(msg.sender, address(this)) < amount) revert AllowanceTooLow();
        if (!token.transferFrom(msg.sender, reserve, amount)) revert TransferFailed();
        emit Directed(msg.sender, amount, recipient, refund);
    }
}
