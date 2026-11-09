// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

import {Script} from "forge-std/Script.sol";
import {stdJson} from "forge-std/StdJson.sol";

import {DirectedDeposit} from "../src/DirectedDeposit.sol";

/// @title DeployDirected (F-018, T-018-03; plan DEC-21, proposed)
/// @notice Deploys one network's `DirectedDeposit` in front of its vault, with plain `CREATE` from a one-time
/// deployment key that holds no role afterwards (F-008 NOTES, "Deployment").
///
///   NETWORK=polygon VAULT_SAFE=0x.. \
///   forge script script/DeployDirected.s.sol --rpc-url <network> --sender <deployer> [--broadcast]
///
/// The token and the limits come from config/<NETWORK>.json (`originToken`, `directed`): the contract's minimum and
/// maximum are the route's, and the deployment refuses a maximum above the Kreivo contract's per-transfer maximum
/// (`directed.kreivoPerTransferMax`), which must be read from the deployed Kreivo contract and recorded there before
/// a real deployment. The vault is a parameter: its address is the operator's, published, and not in this
/// repository. script/VerifyVault.s.sol checks the deployment (DIRECTED_DEPOSIT=<address>) before it is declared.
contract DeployDirected is Script {
    using stdJson for string;

    struct Config {
        string network;
        uint256 chainId;
        address token;
        // The route's minimum and per-transfer maximum (REQ-LM-1), which the contract enforces as its own.
        uint256 minimum;
        uint256 maximum;
        // The Kreivo contract's per-transfer maximum for the bridged asset (§7.F): a directed deposit above it
        // would fail its dry run with no named condition.
        uint256 kreivoPerTransferMax;
    }

    struct Params {
        // The vault (DEC-10): the reserve the contract pays into.
        address reserve;
    }

    function run() external returns (DirectedDeposit directed) {
        Config memory cfg = load(vm.envString("NETWORK"));
        Params memory p = Params({reserve: vm.envAddress("VAULT_SAFE")});
        check(cfg, p);
        vm.startBroadcast();
        directed = _deploy(cfg, p);
        vm.stopBroadcast();
    }

    /// The same deployment from `deployer`, for the tests.
    function deployAs(Config memory cfg, Params memory p, address deployer)
        public
        returns (DirectedDeposit directed)
    {
        check(cfg, p);
        vm.startBroadcast(deployer);
        directed = _deploy(cfg, p);
        vm.stopBroadcast();
    }

    function load(string memory network) public view returns (Config memory cfg) {
        string memory json = vm.readFile(string.concat(vm.projectRoot(), "/config/", network, ".json"));
        cfg.network = json.readString(".network");
        cfg.chainId = json.readUint(".chainId");
        cfg.token = json.readAddress(".originToken.address");
        cfg.minimum = json.readUint(".directed.minimum");
        cfg.maximum = json.readUint(".directed.maximum");
        cfg.kreivoPerTransferMax = json.readUint(".directed.kreivoPerTransferMax");
    }

    /// Reverts unless the deployment is one the plan allows (F-018 plan, "Deployment").
    function check(Config memory cfg, Params memory p) public view {
        require(block.chainid == cfg.chainId, "DeployDirected: wrong chain");
        require(cfg.token.code.length > 0, "DeployDirected: token has no code");
        require(cfg.minimum > 0 && cfg.minimum <= cfg.maximum, "DeployDirected: bad limits");
        require(
            cfg.maximum <= cfg.kreivoPerTransferMax,
            "DeployDirected: maximum above the Kreivo per-transfer maximum"
        );
        require(p.reserve.code.length > 0, "DeployDirected: reserve is not a contract");
    }

    function _deploy(Config memory cfg, Params memory p) internal returns (DirectedDeposit) {
        return new DirectedDeposit(cfg.token, p.reserve, cfg.minimum, cfg.maximum);
    }
}
