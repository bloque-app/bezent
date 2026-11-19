// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {Test, Vm} from "forge-std/Test.sol";
import {StdInvariant} from "forge-std/StdInvariant.sol";
import {DirectedDeposit} from "../src/DirectedDeposit.sol";
import {PermitToken} from "./PermitToken.sol";

/// T-018-01, adversarial (review of PR #121): the endpoint's pairing rule (amendment 0029, A4) as a pure function
/// over logs, and the cases that would break it or make it lie.
library Pairing {
    bytes32 internal constant TRANSFER = keccak256("Transfer(address,address,uint256)");
    bytes32 internal constant DIRECTED = keccak256("Directed(address,uint256,bytes32,address)");

    struct Result {
        uint256 paired;
        uint256 unpaired;
        uint256 pairedAmount;
    }

    /// A4: a `Directed` from `directed` counts only when the log right before it is `token`'s `Transfer` with
    /// `from` = sender, `to` = vault, `value` = amount > 0.
    function classify(Vm.Log[] memory logs, address token, address directed, address vault)
        internal
        pure
        returns (Result memory r)
    {
        for (uint256 i; i < logs.length; ++i) {
            Vm.Log memory dl = logs[i];
            if (dl.emitter != directed || dl.topics[0] != DIRECTED) continue;
            address sender = address(uint160(uint256(dl.topics[1])));
            (uint256 amount,) = abi.decode(dl.data, (uint256, address));
            bool ok;
            if (i > 0) {
                Vm.Log memory t = logs[i - 1];
                ok = t.emitter == token && t.topics.length == 3 && t.topics[0] == TRANSFER
                    && address(uint160(uint256(t.topics[1]))) == sender
                    && address(uint160(uint256(t.topics[2]))) == vault
                    && abi.decode(t.data, (uint256)) == amount && amount > 0;
            }
            if (ok) {
                r.paired++;
                r.pairedAmount += amount;
            } else {
                r.unpaired++;
            }
        }
    }
}

/// A model of an upgraded USDC: same interface, with one behaviour Circle could add. `feeBps` takes a fee on
/// `transferFrom` (fee leg emitted after the main leg); `extraEvent` emits one more log after `Transfer`;
/// `hook` calls back the `from` address after moving value (an ERC-777-style hook).
contract UpgradedToken {
    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);
    event AllowanceUsed(address indexed owner, address indexed spender, uint256 value);

    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    uint256 public feeBps;
    bool public extraEvent;
    bool public hook;
    address public feeCollector = address(0xFEE);

    function set(uint256 feeBps_, bool extraEvent_, bool hook_) external {
        feeBps = feeBps_;
        extraEvent = extraEvent_;
        hook = hook_;
    }

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        emit Approval(msg.sender, spender, amount);
        return true;
    }

    function permit(address, address, uint256, uint256, uint8, bytes32, bytes32) external pure {
        revert("no permit");
    }

    function permit(address, address, uint256, uint256, bytes calldata) external pure {
        revert("no permit");
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        _move(msg.sender, to, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        allowance[from][msg.sender] -= amount;
        uint256 fee = amount * feeBps / 10_000;
        _move(from, to, amount - fee);
        if (fee > 0) _move(from, feeCollector, fee);
        if (extraEvent) emit AllowanceUsed(from, msg.sender, amount);
        if (hook && from.code.length > 0) IHooked(from).tokensSent(to, amount);
        return true;
    }

    function _move(address from, address to, uint256 amount) internal {
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        emit Transfer(from, to, amount);
    }
}

interface IHooked {
    function tokensSent(address to, uint256 amount) external;
}

/// A depositor contract that does a second, nested deposit from the token's hook.
contract ReentrantDepositor is IHooked {
    DirectedDeposit internal immutable directed;
    UpgradedToken internal immutable token;
    bool internal entered;

    constructor(DirectedDeposit directed_, UpgradedToken token_) {
        directed = directed_;
        token = token_;
    }

    function go(uint256 amount) external {
        token.approve(address(directed), 2 * amount);
        directed.deposit(amount, keccak256("outer"), address(0xBEEF));
    }

    function tokensSent(address, uint256 amount) external {
        if (entered) return;
        entered = true;
        directed.deposit(amount, keccak256("inner"), address(0xBEEF));
    }
}

/// One sender, one transaction: a plain transfer to the vault and a directed deposit, in either order.
contract MixedSender {
    function plainThenDirected(
        PermitToken token,
        DirectedDeposit directed,
        address vault,
        uint256 a,
        uint256 b
    ) external {
        token.transfer(vault, a);
        token.approve(address(directed), b);
        directed.deposit(b, keccak256("r"), address(0xBEEF));
    }

    function directedThenPlain(
        PermitToken token,
        DirectedDeposit directed,
        address vault,
        uint256 a,
        uint256 b
    ) external {
        token.approve(address(directed), b);
        directed.deposit(b, keccak256("r"), address(0xBEEF));
        token.transfer(vault, a);
    }
}

contract DirectedDepositAdversarialTest is Test {
    uint256 internal constant MIN = 1e6;
    uint256 internal constant MAX = 50_000e6;
    address internal vault = makeAddr("vault");
    address internal user = makeAddr("user");

    // ------------------------------------------------------------------ the vault as caller

    /// AC-A6.6, INV-10: the reserve cannot call the directing code. A token that accepts `from == to` (USDC does)
    /// would give a perfect A4 pair while the reserve's balance does not move, so the endpoint would mint against no
    /// new reserve. Every form reverts with `SenderIsReserve` before any permit, and nothing moves.
    function test_AC_A6_6_reserveAsCallerReverts() public {
        PermitToken token = new PermitToken();
        DirectedDeposit directed = new DirectedDeposit(address(token), vault, MIN, MAX);
        token.mint(vault, 10_000e6);

        vm.startPrank(vault);
        token.approve(address(directed), 10_000e6);
        vm.recordLogs();
        vm.expectRevert(DirectedDeposit.SenderIsReserve.selector);
        directed.deposit(10_000e6, keccak256("attacker on Kreivo"), user);
        vm.expectRevert(DirectedDeposit.SenderIsReserve.selector);
        directed.depositWithPermit(10_000e6, keccak256("attacker on Kreivo"), user, block.timestamp, 0, 0, 0);
        vm.expectRevert(DirectedDeposit.SenderIsReserve.selector);
        directed.depositWithPermit(10_000e6, keccak256("attacker on Kreivo"), user, block.timestamp, "");
        vm.stopPrank();

        assertEq(vm.getRecordedLogs().length, 0, "no log at all");
        assertEq(token.balanceOf(vault), 10_000e6);
        assertEq(token.allowance(vault, address(directed)), 10_000e6, "the allowance is untouched");
    }

    // ------------------------------------------------------------------ an upgraded token

    /// AC-A6.6, INV-2: a fee on transfer (an upgraded token) would deliver `amount - fee` with no A4 pair, so the
    /// value would sit in the reserve, never minted and never refundable. The reserve's balance must rise by exactly
    /// `amount`, so the call reverts with `ReserveDeltaMismatch` and nothing moves.
    function test_AC_A6_6_feeOnTransferReverts() public {
        UpgradedToken token = new UpgradedToken();
        DirectedDeposit directed = new DirectedDeposit(address(token), vault, MIN, MAX);
        token.set(10, false, false); // 0.1 %
        token.mint(user, 100e6);
        vm.startPrank(user);
        token.approve(address(directed), 100e6);
        vm.expectRevert(DirectedDeposit.ReserveDeltaMismatch.selector);
        directed.deposit(100e6, keccak256("r"), user);
        vm.stopPrank();
        assertEq(token.balanceOf(user), 100e6, "nothing left the caller");
        assertEq(token.balanceOf(vault), 0, "nothing reached the reserve");
        assertEq(token.balanceOf(token.feeCollector()), 0);
    }

    /// AC-A6.6, documentation of what the reserve-balance check cannot catch: a token that delivers exactly `amount`
    /// but emits another log after its `Transfer` inside `transferFrom` (an upgrade adding an event) breaks adjacency.
    /// The deposit succeeds, the value is in the reserve, and the endpoint records the `Directed` as unpaired, which
    /// pages (amendment 0029, A4). Expected to pass as written: a change of the token is a change of the route.
    function test_AC_A6_6_extraEventAfterTransferIsUnpaired() public {
        UpgradedToken token = new UpgradedToken();
        DirectedDeposit directed = new DirectedDeposit(address(token), vault, MIN, MAX);
        token.set(0, true, false);
        token.mint(user, 100e6);
        vm.startPrank(user);
        token.approve(address(directed), 100e6);
        vm.recordLogs();
        directed.deposit(100e6, keccak256("r"), user);
        vm.stopPrank();
        Pairing.Result memory r =
            Pairing.classify(vm.getRecordedLogs(), address(token), address(directed), vault);
        assertEq(r.unpaired, 1);
        assertEq(token.balanceOf(vault), 100e6);
    }

    /// AC-A6.6, INV-2: a hook inside `transferFrom` (an upgraded token) lets the caller re-enter. Without the
    /// reserve-balance check the inner deposit would pair and the outer `Directed`, following the inner one, would be
    /// unpaired, its value stranded. The check is the outer call's too: its reserve delta includes the inner
    /// deposit's amount, so the outer call reverts with `ReserveDeltaMismatch`, taking the inner one with it, and
    /// nothing moves. The review expected this case to pass the check; it does not, which is the better outcome. The
    /// contract has no state to corrupt, so reentrancy only ever mattered for pairing.
    function test_AC_A6_6_reentrantHookReverts() public {
        UpgradedToken token = new UpgradedToken();
        DirectedDeposit directed = new DirectedDeposit(address(token), vault, MIN, MAX);
        token.set(0, false, true);
        ReentrantDepositor dep = new ReentrantDepositor(directed, token);
        token.mint(address(dep), 2e6);
        vm.expectRevert(DirectedDeposit.ReserveDeltaMismatch.selector);
        dep.go(1e6);
        assertEq(token.balanceOf(vault), 0);
        assertEq(token.balanceOf(address(dep)), 2e6);
    }

    // ------------------------------------------------------------------ ordering with USDC-shaped token

    /// AC-A6.6, INV-10: a plain transfer to the vault and a directed deposit from the same sender in one
    /// transaction, in either order: adjacency pairs the directed one only, with its own amount, and the plain one
    /// stays a plain arrival.
    function testFuzz_AC_A6_6_plainAndDirectedSameSenderSameTx(uint256 a, uint256 b, bool plainFirst) public {
        a = bound(a, 1, MAX);
        b = bound(b, MIN, MAX);
        PermitToken token = new PermitToken();
        DirectedDeposit directed = new DirectedDeposit(address(token), vault, MIN, MAX);
        MixedSender s = new MixedSender();
        token.mint(address(s), a + b);
        vm.recordLogs();
        if (plainFirst) s.plainThenDirected(token, directed, vault, a, b);
        else s.directedThenPlain(token, directed, vault, a, b);
        Pairing.Result memory r =
            Pairing.classify(vm.getRecordedLogs(), address(token), address(directed), vault);
        assertEq(r.paired, 1);
        assertEq(r.unpaired, 0);
        assertEq(r.pairedAmount, b);
        assertEq(token.balanceOf(vault), a + b);
    }

    /// The bytes-signature form also survives a front-run permit (DirectedDeposit.t.sol tests v, r, s).
    function test_DEC_21_bytesPermitFrontRunStillDeposits() public {
        PermitToken token = new PermitToken();
        DirectedDeposit directed = new DirectedDeposit(address(token), vault, MIN, MAX);
        (address owner, uint256 key) = makeAddrAndKey("owner");
        token.mint(owner, MIN);
        uint256 deadline = block.timestamp + 1;
        (uint8 v, bytes32 r, bytes32 s) =
            vm.sign(key, token.digest(owner, address(directed), MIN, token.nonces(owner), deadline));
        bytes memory sig = abi.encodePacked(r, s, v);
        vm.prank(user);
        token.permit(owner, address(directed), MIN, deadline, sig);
        vm.prank(owner);
        directed.depositWithPermit(MIN, keccak256("r"), owner, deadline, sig);
        assertEq(token.balanceOf(vault), MIN);
    }
}

/// AC-A6.6 (INV-2, INV-10), invariant: whatever sequence of deposits (approve, permit, either form, batched or not),
/// plain transfers to the vault and refused calls, (1) the contract never holds the token, (2) every `Directed` pairs
/// under A4, and (3) the paired amounts sum to exactly the vault's growth minus the plain transfers.
contract DirectedHandler is Test {
    PermitToken public token;
    DirectedDeposit public directed;
    address public vault;
    uint256 public plain;
    uint256 public paired;
    uint256 public pairedAmount;
    uint256 public unpaired;
    uint256[] internal keys;

    constructor(PermitToken token_, DirectedDeposit directed_, address vault_) {
        token = token_;
        directed = directed_;
        vault = vault_;
        for (uint256 i = 1; i <= 4; ++i) {
            keys.push(uint256(keccak256(abi.encode("actor", i))));
            token.mint(vm.addr(keys[i - 1]), 1_000_000e6);
        }
    }

    function _record() internal {
        Pairing.Result memory r =
            Pairing.classify(vm.getRecordedLogs(), address(token), address(directed), vault);
        paired += r.paired;
        unpaired += r.unpaired;
        pairedAmount += r.pairedAmount;
    }

    function deposit(uint256 who, uint256 amount, bytes32 recipient, address refund) external {
        address a = vm.addr(keys[who % keys.length]);
        amount = bound(amount, 0, 60_000e6);
        vm.prank(a);
        token.approve(address(directed), amount);
        vm.recordLogs();
        vm.prank(a);
        try directed.deposit(amount, recipient, refund) {} catch {}
        _record();
    }

    function depositWithPermit(uint256 who, uint256 amount, bytes32 recipient, address refund, bool bytesForm)
        external
    {
        uint256 k = keys[who % keys.length];
        amount = bound(amount, 0, 60_000e6);
        (bytes memory sig, uint8 v, bytes32 r, bytes32 s) = _sign(k, amount);
        vm.recordLogs();
        vm.prank(vm.addr(k));
        if (bytesForm) {
            try directed.depositWithPermit(amount, recipient, refund, block.timestamp + 1, sig) {} catch {}
        } else {
            try directed.depositWithPermit(amount, recipient, refund, block.timestamp + 1, v, r, s) {}
                catch {}
        }
        _record();
    }

    function _sign(uint256 k, uint256 amount) internal view returns (bytes memory, uint8, bytes32, bytes32) {
        address a = vm.addr(k);
        (uint8 v, bytes32 r, bytes32 s) =
            vm.sign(k, token.digest(a, address(directed), amount, token.nonces(a), block.timestamp + 1));
        return (abi.encodePacked(r, s, v), v, r, s);
    }

    function plainTransfer(uint256 who, uint256 amount) external {
        address a = vm.addr(keys[who % keys.length]);
        amount = bound(amount, 0, token.balanceOf(a));
        vm.prank(a);
        token.transfer(vault, amount);
        plain += amount;
    }
}

contract DirectedDepositInvariantTest is StdInvariant, Test {
    PermitToken internal token;
    DirectedDeposit internal directed;
    DirectedHandler internal handler;
    address internal vault = makeAddr("vault");

    function setUp() public {
        token = new PermitToken();
        directed = new DirectedDeposit(address(token), vault, 1e6, 50_000e6);
        handler = new DirectedHandler(token, directed, vault);
        targetContract(address(handler));
    }

    /// The run did deposit (the handler swallows reverts, so check it is not vacuous).
    function afterInvariant() public view {
        assertGt(handler.paired(), 0);
    }

    function invariant_AC_A6_6_neverHoldsABalance() public view {
        assertEq(token.balanceOf(address(directed)), 0);
    }

    function invariant_AC_A6_6_everyDirectedPairs() public view {
        assertEq(handler.unpaired(), 0);
    }

    function invariant_AC_A6_6_pairedEqualsVaultGrowth() public view {
        assertEq(handler.pairedAmount() + handler.plain(), token.balanceOf(vault));
    }
}
