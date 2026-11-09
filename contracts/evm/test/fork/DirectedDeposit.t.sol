// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {Vm} from "forge-std/Test.sol";

import {DeployDirected} from "../../script/DeployDirected.s.sol";
import {DeployVault} from "../../script/DeployVault.s.sol";
import {VerifyVault} from "../../script/VerifyVault.s.sol";
import {DirectedDeposit} from "../../src/DirectedDeposit.sol";
import {ISafe, ISafeProxyFactory} from "../../src/interfaces/External.sol";
import {VaultFork} from "./VaultFork.sol";

/// Native USDC's view of a permit (FiatToken 2.2).
interface IUSDC {
    function name() external view returns (string memory);
    function version() external view returns (string memory);
    function DOMAIN_SEPARATOR() external view returns (bytes32);
    function nonces(address owner) external view returns (uint256);
    function approve(address spender, uint256 amount) external returns (bool);
    function balanceOf(address account) external view returns (uint256);
    function allowance(address owner, address spender) external view returns (uint256);
    function permit(
        address owner,
        address spender,
        uint256 value,
        uint256 deadline,
        uint8 v,
        bytes32 r,
        bytes32 s
    ) external;
}

/// Safe 1.4.1's CompatibilityFallbackHandler, which answers EIP-1271 for a Safe that sets it.
interface ICompatibilityFallbackHandler {
    function getMessageHashForSafe(address safe, bytes memory message) external view returns (bytes32);
}

/// Two deposits in one transaction, from one contract caller.
contract TwoDeposits {
    function run(address usdc, DirectedDeposit directed, uint256 a, bytes32 ra, uint256 b, bytes32 rb)
        external
    {
        IUSDC(usdc).approve(address(directed), a + b);
        directed.deposit(a, ra);
        directed.deposit(b, rb);
    }
}

/// T-018-02 and T-018-03 on a Polygon fork at the pinned block (e2e/evm/forks.env), against native USDC and a vault
/// that DeployVault deployed: permit with the domain read from the token, a front-run permit, the bytes-signature
/// permit from an EOA and from a Safe, two deposits in one transaction, the limits, the deployment's refusals and
/// the verifier. The depositors' keys are fresh (`makeAddrAndKey`): the test mnemonic's addresses past VaultFork's
/// carry EIP-7702 delegations on Polygon at the pinned block, so USDC would check their permits with EIP-1271.
contract DirectedDepositForkTest is VaultFork {
    event Directed(address indexed sender, uint256 amount, bytes32 indexed recipient);

    bytes32 internal constant TRANSFER = keccak256("Transfer(address,address,uint256)");
    bytes32 internal constant APPROVAL = keccak256("Approval(address,address,uint256)");
    bytes32 internal constant PERMIT_TYPEHASH =
        keccak256("Permit(address owner,address spender,uint256 value,uint256 nonce,uint256 deadline)");
    // features/F-018-directed-deposits/NOTES.md, read at Polygon's finalized head.
    bytes32 internal constant USDC_DOMAIN =
        0xcaa2ce1a5703ccbe253a34eb3166df60a705c561b44b192061e28f2a985be2ca;
    // Safe 1.4.1's CompatibilityFallbackHandler on Polygon (safe-deployments).
    address internal constant FALLBACK_HANDLER = 0xfd0732Dc9E303f09fCEf3a7388Ad10A83459Ec99;
    bytes32 internal constant RECIPIENT = keccak256("a Kreivo account");

    DeployDirected internal deployScript;
    DeployDirected.Config internal dcfg;
    DirectedDeposit internal directed;
    IUSDC internal usdc;
    uint256 internal userKey;
    address internal user;
    address internal frontRunner;

    function network() internal pure override returns (string memory) {
        return "polygon";
    }

    function setUp() public override {
        super.setUp();
        deployScript = new DeployDirected();
        dcfg = deployScript.load("polygon");
        directed = deployScript.deployAs(dcfg, params(), deployer);
        usdc = IUSDC(cfg.originToken);
        (user, userKey) = makeAddrAndKey("bezent.directed.user");
        frontRunner = makeAddr("bezent.directed.front-runner");
        deal(cfg.originToken, user, 100_000e6);
    }

    function params() internal view virtual returns (DeployDirected.Params memory) {
        return DeployDirected.Params({reserve: d.safe});
    }

    // ------------------------------------------------------------------ the token

    /// The permit domain is read from the token and is the one NOTES records: `USD Coin`, `2`, chain 137.
    function test_DEC_21_usdcPermitDomain() public view {
        assertEq(usdc.name(), "USD Coin");
        assertEq(usdc.version(), "2");
        assertEq(block.chainid, 137);
        assertEq(usdc.DOMAIN_SEPARATOR(), USDC_DOMAIN);
        assertEq(domainFromToken(), USDC_DOMAIN);
    }

    // ------------------------------------------------------------------ deposits

    /// AC-A6.6, REQ-RN-5: approve, then deposit: exactly `amount` from the caller to the vault, with `Directed` the
    /// log right after USDC's `Transfer`, and no balance left in the contract.
    function test_AC_A6_6_approveThenDeposit() public {
        uint256 amount = 1e6;
        vm.prank(user);
        usdc.approve(address(directed), amount);
        uint256 before = usdc.balanceOf(user);
        vm.recordLogs();
        vm.prank(user);
        directed.deposit(amount, RECIPIENT);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(logs.length, 2);
        assertPair(logs, 1, user, amount, RECIPIENT);
        assertEq(usdc.balanceOf(user), before - amount);
        assertEq(usdc.balanceOf(d.safe), amount);
        assertNoBalance();
    }

    /// One signature: the permit's `Approval`, then the `Transfer` and `Directed` pair. FiatToken's `transferFrom`
    /// emits no `Approval`, so nothing comes between the two.
    function test_AC_A6_6_permitDeposit() public {
        uint256 amount = 50_000e6;
        (uint8 v, bytes32 r, bytes32 s) = sign(userKey, user, amount, block.timestamp + 1 hours);
        vm.recordLogs();
        vm.prank(user);
        directed.depositWithPermit(amount, RECIPIENT, block.timestamp + 1 hours, v, r, s);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(logs.length, 3);
        assertEq(logs[0].emitter, cfg.originToken);
        assertEq(logs[0].topics[0], APPROVAL);
        assertPair(logs, 2, user, amount, RECIPIENT);
        assertEq(usdc.balanceOf(d.safe), amount);
        assertEq(usdc.allowance(user, address(directed)), 0);
        assertNoBalance();
    }

    /// A front-run permit costs nothing: the deposit's own `permit` reverts inside `try`, and the allowance the
    /// front-runner set is used.
    function test_DEC_21_frontRunPermitStillDeposits() public {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) = sign(userKey, user, 1e6, deadline);
        vm.prank(frontRunner);
        usdc.permit(user, address(directed), 1e6, deadline, v, r, s);
        vm.prank(user);
        directed.depositWithPermit(1e6, RECIPIENT, deadline, v, r, s);
        assertEq(usdc.balanceOf(d.safe), 1e6);
        assertNoBalance();
    }

    /// The pull is always from `msg.sender`: the user's signature in someone else's call moves nothing.
    function test_DEC_21_stolenSignatureMovesNothing() public {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) = sign(userKey, user, 1e6, deadline);
        deal(cfg.originToken, frontRunner, 1e6);
        vm.prank(frontRunner);
        vm.expectRevert(DirectedDeposit.AllowanceTooLow.selector);
        directed.depositWithPermit(1e6, RECIPIENT, deadline, v, r, s);
        assertEq(usdc.balanceOf(d.safe), 0);
        assertEq(usdc.nonces(user), 0);
    }

    function test_DEC_21_badPermitReverts() public {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) = sign(uint256(keccak256("not the user")), user, 1e6, deadline);
        vm.prank(user);
        vm.expectRevert(DirectedDeposit.AllowanceTooLow.selector);
        directed.depositWithPermit(1e6, RECIPIENT, deadline, v, r, s);
    }

    function test_DEC_21_bytesPermitFromEoa() public {
        uint256 deadline = block.timestamp + 1 hours;
        (uint8 v, bytes32 r, bytes32 s) = sign(userKey, user, 1e6, deadline);
        vm.prank(user);
        directed.depositWithPermit(1e6, RECIPIENT, deadline, abi.encodePacked(r, s, v));
        assertEq(usdc.balanceOf(d.safe), 1e6);
        assertNoBalance();
    }

    /// A Safe as depositor (EIP-1271 through Safe's CompatibilityFallbackHandler): its owner signs the Safe message
    /// of the permit digest, and the Safe calls `depositWithPermit`. `sender` is the Safe.
    function test_DEC_21_bytesPermitFromSafe() public {
        (address safeOwner, uint256 ownerKey) = makeAddrAndKey("bezent.directed.safe-owner");
        address[] memory safeOwners = new address[](1);
        safeOwners[0] = safeOwner;
        address wallet = ISafeProxyFactory(cfg.safeProxyFactory)
            .createProxyWithNonce(
                cfg.safeSingleton,
                abi.encodeCall(
                    ISafe.setup,
                    (safeOwners, 1, address(0), "", FALLBACK_HANDLER, address(0), 0, payable(address(0)))
                ),
                uint256(keccak256("bezent.directed.test.depositor"))
            );
        deal(cfg.originToken, wallet, 1e6);
        uint256 deadline = block.timestamp + 1 hours;
        bytes32 permitDigest = digest(wallet, 1e6, deadline);
        bytes32 message = ICompatibilityFallbackHandler(FALLBACK_HANDLER)
            .getMessageHashForSafe(wallet, abi.encode(permitDigest));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(ownerKey, message);

        vm.recordLogs();
        vm.prank(wallet);
        directed.depositWithPermit(1e6, RECIPIENT, deadline, abi.encodePacked(r, s, v));
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertPair(logs, logs.length - 1, wallet, 1e6, RECIPIENT);
        assertEq(usdc.balanceOf(d.safe), 1e6);
        assertEq(usdc.balanceOf(wallet), 0);
        assertNoBalance();
    }

    /// A batch of two deposits in one transaction gives two adjacent pairs (amendment 0029, A4: adjacency, not
    /// order in the transaction).
    function test_AC_A6_6_twoDepositsInOneTransaction() public {
        TwoDeposits batch = new TwoDeposits();
        deal(cfg.originToken, address(batch), 3e6);
        bytes32 other = keccak256("another Kreivo account");
        vm.recordLogs();
        batch.run(cfg.originToken, directed, 1e6, RECIPIENT, 2e6, other);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(logs.length, 5); // Approval, Transfer, Directed, Transfer, Directed
        assertPair(logs, 2, address(batch), 1e6, RECIPIENT);
        assertPair(logs, 4, address(batch), 2e6, other);
        assertEq(usdc.balanceOf(d.safe), 3e6);
        assertNoBalance();
    }

    /// AC-A6.5: below the minimum, above the maximum and a zero recipient revert, and nothing moves.
    function test_AC_A6_5_refusalsMoveNothing() public {
        vm.startPrank(user);
        usdc.approve(address(directed), type(uint256).max);
        vm.expectRevert(DirectedDeposit.BelowMinimum.selector);
        directed.deposit(1e6 - 1, RECIPIENT);
        vm.expectRevert(DirectedDeposit.AboveMaximum.selector);
        directed.deposit(50_000e6 + 1, RECIPIENT);
        vm.expectRevert(DirectedDeposit.ZeroRecipient.selector);
        directed.deposit(1e6, bytes32(0));
        vm.stopPrank();
        assertEq(usdc.balanceOf(d.safe), 0);
        assertNoBalance();
    }

    // ------------------------------------------------------------------ deployment and verifier

    /// T-018-03: the deployment's refusals.
    function test_REQ_AS_11_deployRefusals() public {
        DeployDirected.Config memory c = dcfg;
        c.kreivoPerTransferMax = c.maximum - 1;
        vm.expectRevert("DeployDirected: maximum above the Kreivo per-transfer maximum");
        deployScript.check(c, params());

        c = dcfg;
        c.minimum = 0;
        vm.expectRevert("DeployDirected: bad limits");
        deployScript.check(c, params());

        DeployDirected.Params memory p = params();
        p.reserve = user;
        vm.expectRevert("DeployDirected: reserve is not a contract");
        deployScript.check(dcfg, p);
    }

    /// T-018-03: the verifier accepts the deployment, and refuses a changed immutable or a maximum above the Kreivo
    /// per-transfer maximum.
    function test_REQ_AS_11_verifierChecksDirected() public {
        VerifyVault verifier = new VerifyVault();
        verifier.checkDirected(dcfg, cfg, d, address(directed));

        DeployDirected.Config memory c = dcfg;
        c.minimum = 2e6;
        DirectedDeposit changed = deployScript.deployAs(c, params(), deployer);
        vm.expectRevert("VerifyVault: directed minimum");
        verifier.checkDirected(dcfg, cfg, d, address(changed));

        c = dcfg;
        c.kreivoPerTransferMax = c.maximum - 1;
        vm.expectRevert("VerifyVault: directed maximum above the Kreivo per-transfer maximum");
        verifier.checkDirected(c, cfg, d, address(directed));

        DeployVault.Deployment memory other = d;
        other.safe = user;
        vm.expectRevert("VerifyVault: directed reserve");
        verifier.checkDirected(dcfg, cfg, other, address(directed));
    }

    // ------------------------------------------------------------------ helpers

    function domainFromToken() internal view returns (bytes32) {
        return keccak256(
            abi.encode(
                keccak256(
                    "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"
                ),
                keccak256(bytes(usdc.name())),
                keccak256(bytes(usdc.version())),
                block.chainid,
                cfg.originToken
            )
        );
    }

    function digest(address owner, uint256 value, uint256 deadline) internal view returns (bytes32) {
        return keccak256(
            abi.encodePacked(
                "\x19\x01",
                domainFromToken(),
                keccak256(
                    abi.encode(PERMIT_TYPEHASH, owner, address(directed), value, usdc.nonces(owner), deadline)
                )
            )
        );
    }

    function sign(uint256 key, address owner, uint256 value, uint256 deadline)
        internal
        view
        returns (uint8, bytes32, bytes32)
    {
        return vm.sign(key, digest(owner, value, deadline));
    }

    function assertNoBalance() internal view {
        assertEq(usdc.balanceOf(address(directed)), 0);
    }

    /// `logs[i]` is `Directed(sender, amount, recipient)` from the contract, and `logs[i - 1]` USDC's
    /// `Transfer(sender, vault, amount)`.
    function assertPair(Vm.Log[] memory logs, uint256 i, address sender, uint256 amount, bytes32 recipient)
        internal
        view
    {
        Vm.Log memory t = logs[i - 1];
        assertEq(t.emitter, cfg.originToken);
        assertEq(t.topics[0], TRANSFER);
        assertEq(address(uint160(uint256(t.topics[1]))), sender);
        assertEq(address(uint160(uint256(t.topics[2]))), d.safe);
        assertEq(abi.decode(t.data, (uint256)), amount);
        Vm.Log memory dl = logs[i];
        assertEq(dl.emitter, address(directed));
        assertEq(dl.topics[0], Directed.selector);
        assertEq(address(uint160(uint256(dl.topics[1]))), sender);
        assertEq(dl.topics[2], recipient);
        assertEq(abi.decode(dl.data, (uint256)), amount);
    }
}
