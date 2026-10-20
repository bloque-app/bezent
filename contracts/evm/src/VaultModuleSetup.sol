// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

interface ISafeSetup {
    function enableModule(address module) external;
    function setGuard(address guard) external;
}

/// @title VaultModuleSetup
/// @notice Delegatecalled once by a new Safe from `setup(…, to, data, …)` so the vault is born complete: the Roles
/// modifier and the Delay modifier enabled as its modules and the guard set. There is no moment at which the
/// deployer, or anyone but the owners, controls the Safe, and none at which the Safe runs without its guard.
/// @dev In the delegatecall, `address(this)` is the Safe, and the calls below are ordinary calls from the
/// Safe to itself, which satisfy Safe's `authorized` modifier. Nothing else is done, and no state is kept.
contract VaultModuleSetup {
    function setUpVault(address roles, address delay, address guard) external {
        ISafeSetup(address(this)).enableModule(roles);
        ISafeSetup(address(this)).enableModule(delay);
        ISafeSetup(address(this)).setGuard(guard);
    }
}
