// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

interface ISafeModuleManager {
    function enableModule(address module) external;
}

/// @title VaultModuleSetup
/// @notice Delegatecalled once by a new Safe from `setup(…, to, data, …)` so the vault is born with its Roles
/// modifier enabled: there is no moment at which the deployer, or anyone but the owners, controls the Safe.
/// @dev In the delegatecall, `address(this)` is the Safe, and the call below is an ordinary call from the
/// Safe to itself, which satisfies Safe's `authorized` modifier. Nothing else is done, and no state is kept.
contract VaultModuleSetup {
    function enableModule(address module) external {
        ISafeModuleManager(address(this)).enableModule(module);
    }
}
