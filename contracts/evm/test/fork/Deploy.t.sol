// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {Vm} from "forge-std/Vm.sol";
import {Enum} from "@gnosis.pm/safe-contracts/contracts/common/Enum.sol";
import {ExecutionOptions} from "zodiac-roles/Types.sol";

import {DeployVault} from "../../script/DeployVault.s.sol";
import {VerifyVault} from "../../script/VerifyVault.s.sol";
import {VaultGuard} from "../../src/VaultGuard.sol";
import {VaultRoles} from "../../src/VaultRoles.sol";
import {IERC20, IRoles, IRolesEvents, IDelay, ISafeProxyFactory} from "../../src/interfaces/External.sol";
import {VaultFork} from "./VaultFork.sol";

/// A deployer key that abuses its time as the modifiers' owner, as the review's scenario does: before the
/// handover it plants a member of its own, widens the delivery role to the whole origin token, enables a module
/// of its own on the Delay, or queues a transaction in the Delay (and then disables the module, leaving the
/// module list clean).
contract PlantingDeployVault is DeployVault {
    enum Plant {
        Member,
        WidenedRole,
        DelayModule,
        DelayQueue
    }

    Plant internal plant;
    address internal accomplice;

    constructor(Plant plant_, address accomplice_) {
        plant = plant_;
        accomplice = accomplice_;
    }

    function _beforeHandover(IRoles roles, NetworkConfig memory cfg, Deployment memory d) internal override {
        if (plant == Plant.DelayModule) {
            IDelay(d.delay).enableModule(accomplice);
        } else if (plant == Plant.DelayQueue) {
            // The deployer is still the Delay's owner: it enables itself, queues, and disables itself again.
            // Under the broadcast, the deployer is the caller of every one of these.
            (, address self,) = vm.readCallers();
            IDelay delay = IDelay(d.delay);
            delay.enableModule(self);
            delay.execTransactionFromModule(accomplice, 0, hex"deadbeef", Enum.Operation.Call);
            delay.disableModule(address(0x1), self);
        } else if (plant == Plant.Member) {
            bytes32[] memory keys = new bytes32[](1);
            keys[0] = VaultRoles.DELIVERY;
            bool[] memory yes = new bool[](1);
            yes[0] = true;
            roles.assignRoles(accomplice, keys, yes);
        } else {
            roles.allowTarget(VaultRoles.DELIVERY, cfg.originToken, ExecutionOptions.Both);
        }
    }
}

/// T-008-03, T-008-07: the deployment script on each fork. Owners and threshold as configured, the Roles and
/// Delay modifiers the only modules and owned by the Safe, the guard set, the deployer left with nothing, and
/// every pinned address carrying the code its project published (NOTES.md).
abstract contract DeployTest is VaultFork {
    // Runtime code hashes, from safe-global/safe-deployments (v1.4.1 codeHash) and from the zodiac
    // repository's verified mastercopy artefacts; equal on Polygon, Base and BNB Chain at the pinned blocks.
    bytes32 internal constant SAFE_L2_141 =
        0xb1f926978a0f44a2c0ec8fe822418ae969bd8c3f18d61e5103100339894f81ff;
    bytes32 internal constant SAFE_PROXY_FACTORY_141 =
        0x50c3cdc4074750a7a974204a716c999edd37482f907608d960b2b025ee0b3317;
    bytes32 internal constant MULTISEND_CALL_ONLY_141 =
        0xecd5bd14a08c5d2122379900b2f272bdf107a7e92423c10dd5fe3254386c9939;
    bytes32 internal constant ROLES_211 = 0x471d8b3b419f1eb955230c0326c8812176df49bf3c7b414a563fda5a3c6c10b6;
    // keccak of the runtime bytecode in gnosisguild/zodiac mastercopies/delay/1.1.1/Delay/bytecode.json, equal to
    // the code on Polygon, Base and BNB Chain at the pinned blocks.
    bytes32 internal constant DELAY_111 = 0xdd928eddfff7b68bd8fff28178c29e3388c1d098051d9262f151c4b534c577b8;
    bytes32 internal constant MODULE_PROXY_FACTORY_120 =
        0x01623cbcf010a1c326230f1b2d5f48a66b440232ee49096102bc84967dc5f21e;

    // Safe's guard slot: keccak256("guard_manager.guard.address").
    uint256 internal constant GUARD_SLOT = 0x4a204f620c8c5ccdca3fd54d003badd85ba500436a431f0cbda4f558c93c34c8;
    // Safe's fallback handler slot: keccak256("fallback_manager.handler.address").
    uint256 internal constant FALLBACK_SLOT =
        0x6c9a6c4a39284e37ed1cf53d337577d14212a4870fb976a4366c693b939918d5;

    /// DEC-10: every external address in config/<network>.json has the published code.
    function test_DEC_10_pinnedAddressesCarryVerifiedCode() public view {
        assertEq(cfg.safeSingleton.codehash, SAFE_L2_141, "SafeL2 1.4.1");
        assertEq(cfg.safeProxyFactory.codehash, SAFE_PROXY_FACTORY_141, "SafeProxyFactory 1.4.1");
        assertEq(cfg.multiSendCallOnly.codehash, MULTISEND_CALL_ONLY_141, "MultiSendCallOnly 1.4.1");
        assertEq(cfg.rolesMastercopy.codehash, ROLES_211, "Roles 2.1.1");
        assertEq(cfg.delayMastercopy.codehash, DELAY_111, "Delay 1.1.1");
        assertEq(cfg.moduleProxyFactory.codehash, MODULE_PROXY_FACTORY_120, "ModuleProxyFactory 1.2.0");
        assertEq(IERC20(cfg.originToken).decimals(), cfg.originDecimals, "origin decimals");
        assertEq(IERC20(cfg.originToken).symbol(), cfg.originSymbol, "origin symbol");
        for (uint256 i; i < cfg.convertibles.length; ++i) {
            assertEq(
                IERC20(cfg.convertibles[i].token).symbol(), cfg.convertibles[i].symbol, "convertible symbol"
            );
        }
    }

    /// REQ-KY-5: owners and threshold exactly as configured; several signers.
    function test_REQ_KY_5_ownersAndThresholdAsConfigured() public view {
        address[] memory got = safe.getOwners();
        assertEq(got.length, owners.length);
        for (uint256 i; i < owners.length; ++i) {
            bool found;
            for (uint256 j; j < got.length; ++j) {
                if (got[j] == owners[i]) found = true;
            }
            assertTrue(found, "owner missing");
        }
        assertEq(safe.getThreshold(), 2);
    }

    /// REQ-RN-4, REQ-GV-3, REQ-GV-5: the vault's modules are the Roles modifier (outflows) and the Delay
    /// (administration), both owned by the Safe; the guard is set; there is no fallback handler.
    function test_REQ_RN_4_rolesAndDelayAreTheOnlyModulesAndOwnedByTheSafe() public view {
        (address[] memory modules,) = safe.getModulesPaginated(address(0x1), 10);
        assertEq(modules.length, 2, "two modules");
        assertTrue(safe.isModuleEnabled(d.roles), "Roles");
        assertTrue(safe.isModuleEnabled(d.delay), "Delay");
        assertEq(roles.owner(), d.safe);
        assertEq(roles.avatar(), d.safe);
        assertEq(roles.target(), d.safe);
        assertEq(delayMod.owner(), d.safe);
        assertEq(delayMod.avatar(), d.safe);
        assertEq(delayMod.target(), d.safe);
        (address[] memory delayModules,) = delayMod.getModulesPaginated(address(0x1), 10);
        assertEq(delayModules.length, 1, "the Safe alone queues");
        assertEq(delayModules[0], d.safe);
        assertEq(address(uint160(uint256(bytes32(safe.getStorageAt(GUARD_SLOT, 1))))), d.guard, "guard");
        assertEq(uint256(bytes32(safe.getStorageAt(FALLBACK_SLOT, 1))), 0, "no fallback handler");
        assertEq(address(roles.unwrappers(_unwrapperKey(cfg.multiSendCallOnly, 0x8d80ff0a))), d.unwrapper);
        assertEq(address(roles.unwrappers(_unwrapperKey(cfg.originToken, VaultRoles.TRANSFER))), d.unwrapper);
    }

    /// REQ-KY-4: each role key is a member of its own role only; the deployer keeps no power.
    function test_REQ_KY_4_membersAndNoDeployerPower() public view {
        assertTrue(roles.isModuleEnabled(delivery), "delivery member");
        assertEq(roles.isModuleEnabled(conversion), cfg.venue != DeployVault.Venue.None, "conversion member");
        assertFalse(roles.isModuleEnabled(deployer));
        assertFalse(safe.isModuleEnabled(deployer));
        assertFalse(safe.isModuleEnabled(delivery));
        assertFalse(safe.isModuleEnabled(conversion));
        address[] memory got = safe.getOwners();
        for (uint256 j; j < got.length; ++j) {
            assertTrue(got[j] != deployer && got[j] != delivery && got[j] != conversion, "role key as owner");
        }
    }

    /// AC-I1.1: the delivery allowance is set as configured, refilling per period.
    function test_AC_I1_1_allowancesAsConfigured() public view {
        (uint128 refill, uint128 maxRefill, uint64 period, uint128 bal,) =
            roles.allowances(VaultRoles.deliveryAllowanceKey(cfg.originToken));
        assertEq(refill, cfg.deliveryRefill);
        assertEq(maxRefill, cfg.deliveryMaxRefill);
        assertEq(period, cfg.deliveryPeriod);
        assertEq(bal, cfg.deliveryBalance);
        assertGt(period, 0, "refills per period");
        for (uint256 i; i < cfg.convertibles.length; ++i) {
            (refill, maxRefill, period, bal,) =
                roles.allowances(VaultRoles.conversionAllowanceKey(cfg.convertibles[i].token));
            assertEq(bal, cfg.convertibles[i].balance);
            assertEq(period, cfg.convertibles[i].period);
        }
    }

    /// REQ-KY-4: the post-deployment verifier accepts a vault deployed exactly as configured.
    function test_REQ_KY_4_verifierAcceptsTheDeployment() public {
        (DeployVault.Deployment memory e, DeployVault.Governance memory g, VerifyVault.Log[] memory logs) =
            _recordedDeployment(script);
        new VerifyVault().check(script, cfg, g, e, logs);
    }

    /// REQ-KY-4, REQ-GV-3: the deployer never owns an enabled modifier. Ownership of the Roles modifier passes
    /// to the Safe's address before the Safe exists, and the Safe enables the modifier only when it is created.
    function test_REQ_KY_4_deployerNeverOwnsAnEnabledModifier() public {
        DeployVault.Governance memory g = governance();
        g.saltNonce += 1;
        vm.recordLogs();
        DeployVault.Deployment memory e = script.deployAs(cfg, g, deployer);
        Vm.Log[] memory logs = vm.getRecordedLogs();

        bytes32 enabled = keccak256("EnabledModule(address)");
        uint256 handover = type(uint256).max;
        uint256 delayHandover = type(uint256).max;
        uint256 enabling = type(uint256).max;
        uint256 lastRoles;
        uint256 lastDelay;
        for (uint256 i; i < logs.length; ++i) {
            Vm.Log memory l = logs[i];
            if (l.emitter == e.roles) lastRoles = i;
            if (l.emitter == e.delay) lastDelay = i;
            if (
                l.emitter == e.delay && l.topics[0] == IRolesEvents.OwnershipTransferred.selector
                    && address(uint160(uint256(l.topics[2]))) == e.safe
            ) {
                assertEq(
                    address(uint160(uint256(l.topics[1]))), deployer, "Delay handed over by the deployer"
                );
                delayHandover = i;
            }
            if (
                l.emitter == e.roles && l.topics[0] == IRolesEvents.OwnershipTransferred.selector
                    && address(uint160(uint256(l.topics[2]))) == e.safe
            ) {
                assertEq(address(uint160(uint256(l.topics[1]))), deployer, "handed over by the deployer");
                handover = i;
            }
            if (
                l.emitter == e.safe && l.topics[0] == enabled
                    && address(uint160(uint256(l.topics[1]))) == e.roles
            ) {
                enabling = i;
            }
        }
        assertLt(handover, enabling, "the handover precedes the enabling");
        assertLt(delayHandover, enabling, "the Delay's handover precedes the enabling");
        assertLt(lastRoles, enabling, "nothing changes the modifier once the Safe has it");
        assertLt(lastDelay, enabling, "nothing changes the Delay once the Safe has it");
        assertEq(roles.owner(), d.safe);
        assertEq(delayMod.owner(), d.safe);
    }

    /// REQ-GV-3: the Delay's cooldown and expiration are the configured ones, which are the recommended ones.
    function test_REQ_GV_3_delayAsConfigured() public view {
        assertEq(delayMod.txCooldown(), exampleValue("VAULT_DELAY_COOLDOWN"));
        assertEq(delayMod.txExpiration(), exampleValue("VAULT_DELAY_EXPIRATION"));
        assertEq(delayMod.txCooldown(), 1 days, "recommended cooldown");
        assertEq(delayMod.txExpiration(), 7 days, "recommended expiration");
        assertEq(delayMod.queueNonce(), 0);
    }

    /// REQ-GV-3: the script refuses a delay that is none.
    function test_REQ_GV_3_scriptRefusesNoCooldown() public {
        DeployVault.Governance memory g = governance();
        g.saltNonce += 1;
        g.cooldown = 0;
        vm.expectRevert(bytes("DeployVault: no cooldown"));
        script.deployAs(cfg, g, deployer);
    }

    /// REQ-GV-3: the script refuses a queue that never clears, and one the Delay itself would refuse.
    function test_REQ_GV_3_scriptRefusesNoExpiration() public {
        DeployVault.Governance memory g = governance();
        g.saltNonce += 1;
        g.expiration = 0;
        vm.expectRevert(bytes("DeployVault: expiration below 60 s"));
        script.deployAs(cfg, g, deployer);
    }

    function test_REQ_GV_3_scriptRefusesExpirationBelowTheDelaysMinimum() public {
        DeployVault.Governance memory g = governance();
        g.saltNonce += 1;
        g.expiration = 59;
        vm.expectRevert(bytes("DeployVault: expiration below 60 s"));
        script.deployAs(cfg, g, deployer);
    }

    /// REQ-GV-3: a Delay module the deployer enabled before the handover survives it, and the verifier fails.
    function test_REQ_GV_3_verifierDetectsAPlantedDelayModule() public {
        PlantingDeployVault evil = new PlantingDeployVault(PlantingDeployVault.Plant.DelayModule, outsider);
        (DeployVault.Deployment memory e, DeployVault.Governance memory g, VerifyVault.Log[] memory logs) =
            _recordedDeployment(evil);
        assertTrue(IDelay(e.delay).isModuleEnabled(outsider), "the plant survived the handover");
        VerifyVault verifier = new VerifyVault();
        vm.expectRevert(bytes("VerifyVault: Delay modules are not the Safe alone"));
        verifier.check(script, cfg, g, e, logs);
    }

    /// REQ-GV-3: a transaction the deployer queued while it owned the Delay, with the module list left clean,
    /// is still in the queue after the handover, and the verifier fails.
    function test_REQ_GV_3_verifierDetectsAQueuedTransaction() public {
        PlantingDeployVault evil = new PlantingDeployVault(PlantingDeployVault.Plant.DelayQueue, outsider);
        (DeployVault.Deployment memory e, DeployVault.Governance memory g, VerifyVault.Log[] memory logs) =
            _recordedDeployment(evil);
        assertEq(IDelay(e.delay).queueNonce(), 1, "the queued transaction survived the handover");
        assertFalse(IDelay(e.delay).isModuleEnabled(address(evil)), "its module is gone");
        VerifyVault verifier = new VerifyVault();
        vm.expectRevert(bytes("VerifyVault: Delay has queued transactions"));
        verifier.check(script, cfg, g, e, logs);
    }

    /// REQ-GV-3, REQ-GV-5: the verifier checks the Delay's settings, the guard, the modules and the handler.
    function test_REQ_GV_5_verifierChecksDelayGuardModulesAndHandler() public {
        (DeployVault.Deployment memory e, DeployVault.Governance memory g, VerifyVault.Log[] memory logs) =
            _recordedDeployment(script);
        VerifyVault verifier = new VerifyVault();

        DeployVault.Governance memory other = governance();
        other.cooldown = g.cooldown - 1;
        vm.expectRevert(bytes("VerifyVault: Delay cooldown"));
        verifier.check(script, cfg, other, e, logs);
        other = governance();
        other.expiration = g.expiration + 1;
        vm.expectRevert(bytes("VerifyVault: Delay expiration"));
        verifier.check(script, cfg, other, e, logs);

        // A guard that is not this repository's (here: one for another pair of modifiers).
        DeployVault.Deployment memory wrongGuard = DeployVault.Deployment(
            e.safe,
            e.roles,
            e.delay,
            address(new VaultGuard(address(0xBEEF), address(0xCAFE))),
            e.emitter,
            e.unwrapper,
            e.moduleSetup
        );
        vm.expectRevert(bytes("VerifyVault: Safe guard"));
        verifier.check(script, cfg, g, wrongGuard, logs);
        // The right guard address, but the Safe has none.
        vm.store(e.safe, bytes32(GUARD_SLOT), bytes32(0));
        vm.expectRevert(bytes("VerifyVault: Safe guard"));
        verifier.check(script, cfg, g, e, logs);
        vm.store(e.safe, bytes32(GUARD_SLOT), bytes32(uint256(uint160(e.guard))));
        // A fallback handler, which would let the owners sign for the Safe.
        vm.store(e.safe, bytes32(FALLBACK_SLOT), bytes32(uint256(uint160(address(0xF00D)))));
        vm.expectRevert(bytes("VerifyVault: Safe fallback handler"));
        verifier.check(script, cfg, g, e, logs);
        vm.store(e.safe, bytes32(FALLBACK_SLOT), bytes32(0));
        verifier.check(script, cfg, g, e, logs);
    }

    /// REQ-KY-4: a member planted by the deployer before the handover survives it, and the verifier fails.
    function test_REQ_KY_4_verifierDetectsAPlantedMember() public {
        PlantingDeployVault evil = new PlantingDeployVault(PlantingDeployVault.Plant.Member, outsider);
        (DeployVault.Deployment memory e, DeployVault.Governance memory g, VerifyVault.Log[] memory logs) =
            _recordedDeployment(evil);
        assertTrue(IRoles(e.roles).isModuleEnabled(outsider), "the plant survived the handover");
        VerifyVault verifier = new VerifyVault();
        vm.expectRevert(bytes("VerifyVault: permission not in the configuration"));
        verifier.check(script, cfg, g, e, logs);
    }

    /// REQ-KY-4: a widened role (the delivery role allowed the whole origin token) is invisible in state
    /// reads, since no member changes; the replayed events show it and the verifier fails.
    function test_REQ_KY_4_verifierDetectsAWidenedRole() public {
        PlantingDeployVault evil = new PlantingDeployVault(PlantingDeployVault.Plant.WidenedRole, outsider);
        (DeployVault.Deployment memory e, DeployVault.Governance memory g, VerifyVault.Log[] memory logs) =
            _recordedDeployment(evil);
        VerifyVault verifier = new VerifyVault();
        vm.expectRevert(bytes("VerifyVault: permission not in the configuration"));
        verifier.check(script, cfg, g, e, logs);
    }

    /// The verifier refuses logs that do not reach back to the modifier's creation, and a configured
    /// permission that is missing.
    function test_REQ_KY_4_verifierRefusesPartialHistory() public {
        (DeployVault.Deployment memory e, DeployVault.Governance memory g, VerifyVault.Log[] memory logs) =
            _recordedDeployment(script);
        VerifyVault verifier = new VerifyVault();

        // Drop everything up to and including the setup event.
        uint256 setupAt;
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].topics[0] == IRolesEvents.RolesModSetup.selector) setupAt = i;
        }
        VerifyVault.Log[] memory late = new VerifyVault.Log[](logs.length - setupAt - 1);
        for (uint256 i; i < late.length; ++i) {
            late[i] = logs[setupAt + 1 + i];
        }
        vm.expectRevert(bytes("VerifyVault: logs do not start at the modifier's creation"));
        verifier.check(script, cfg, g, e, late);

        // Drop the last permission event (the last member's assignment).
        VerifyVault.Log[] memory short = new VerifyVault.Log[](logs.length - 1);
        uint256 k;
        bool dropped;
        for (uint256 i = logs.length; i > 0; --i) {
            if (!dropped && logs[i - 1].topics[0] == IRolesEvents.AssignRoles.selector) {
                dropped = true;
                continue;
            }
            short[short.length - 1 - k++] = logs[i - 1];
        }
        vm.expectRevert(bytes("VerifyVault: configured permission missing"));
        verifier.check(script, cfg, g, e, short);
    }

    /// The script's prediction of the Safe's address uses the deployed factory's own proxy code.
    function test_REQ_KY_4_safeAddressIsPredicted() public view {
        assertGt(ISafeProxyFactory(cfg.safeProxyFactory).proxyCreationCode().length, 0);
        assertEq(roles.owner(), d.safe);
        assertGt(d.safe.code.length, 0);
    }

    /// REQ-KY-4: the script refuses a deployer that is an owner.
    function test_REQ_KY_4_scriptRefusesDeployerAsOwner() public {
        DeployVault.Governance memory g = governance();
        g.saltNonce += 1;
        vm.expectRevert(bytes("DeployVault: deployer is an owner"));
        script.deployAs(cfg, g, owners[0]);
    }

    /// REQ-KY-4: the script refuses a deployer that is a role member.
    function test_REQ_KY_4_scriptRefusesDeployerAsMember() public {
        DeployVault.Governance memory g = governance();
        g.saltNonce += 1;
        vm.expectRevert(bytes("DeployVault: deployer is the delivery member"));
        script.deployAs(cfg, g, delivery);
    }

    /// One deployment with a fresh salt, by `deployer` through `via`, with the Roles modifier's logs.
    function _recordedDeployment(DeployVault via)
        internal
        returns (
            DeployVault.Deployment memory e,
            DeployVault.Governance memory g,
            VerifyVault.Log[] memory logs
        )
    {
        g = governance();
        g.saltNonce += 2;
        vm.recordLogs();
        e = via.deployAs(cfg, g, deployer);
        Vm.Log[] memory all = vm.getRecordedLogs();
        uint256 n;
        for (uint256 i; i < all.length; ++i) {
            if (all[i].emitter == e.roles) ++n;
        }
        logs = new VerifyVault.Log[](n);
        n = 0;
        for (uint256 i; i < all.length; ++i) {
            if (all[i].emitter == e.roles) logs[n++] = VerifyVault.Log(all[i].topics, all[i].data);
        }
    }

    /// REQ-KY-5: the script refuses a single-signer vault.
    function test_REQ_KY_5_scriptRefusesOneSigner() public {
        DeployVault.Governance memory g = governance();
        g.threshold = 1;
        g.saltNonce += 1;
        vm.expectRevert(bytes("DeployVault: threshold below 2"));
        script.deployAs(cfg, g, deployer);
    }

    /// REQ-KY-4: the script refuses a role key that is also an owner.
    function test_REQ_KY_4_scriptRefusesRoleKeyAsOwner() public {
        DeployVault.Governance memory g = governance();
        g.deliveryMember = owners[0];
        g.saltNonce += 1;
        vm.expectRevert(bytes("DeployVault: delivery member is an owner"));
        script.deployAs(cfg, g, deployer);
    }

    function _unwrapperKey(address to, bytes4 selector) internal pure returns (bytes32) {
        return bytes32(bytes20(to)) | (bytes32(selector) >> 160);
    }
}

contract PolygonDeployTest is DeployTest {
    function network() internal pure override returns (string memory) {
        return "polygon";
    }

    /// REQ-CV-7 (a): the script refuses the origin token as a convertible.
    function test_REQ_CV_7_scriptRefusesOriginAsConvertible() public {
        DeployVault.NetworkConfig memory c = cfg;
        c.convertibles[0].token = cfg.originToken;
        DeployVault.Governance memory g = governance();
        g.saltNonce += 1;
        vm.expectRevert(bytes("DeployVault: origin token as convertible"));
        script.deployAs(c, g, deployer);
    }
}

contract BaseDeployTest is DeployTest {
    function network() internal pure override returns (string memory) {
        return "base";
    }
}

contract BscDeployTest is DeployTest {
    function network() internal pure override returns (string memory) {
        return "bsc";
    }
}
