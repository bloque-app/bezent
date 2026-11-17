// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
pragma solidity 0.8.28;

interface IERC1271 {
    function isValidSignature(bytes32 hash, bytes memory signature) external view returns (bytes4);
}

/// A test ERC-20 shaped like FiatToken 2.2 where `DirectedDeposit` touches it: EIP-2612 `permit`, the
/// bytes-signature `permit` (EOA or EIP-1271), and a `transferFrom` that emits only `Transfer`, as USDC's does, so
/// the unit tests' adjacency checks mean what the fork tests' do. `failTransfers` makes `transferFrom` return false.
contract PermitToken {
    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    bytes32 public constant PERMIT_TYPEHASH =
        keccak256("Permit(address owner,address spender,uint256 value,uint256 nonce,uint256 deadline)");

    string public constant name = "USD Coin";
    string public constant version = "2";
    uint8 public constant decimals = 6;

    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    mapping(address => uint256) public nonces;
    bool public failTransfers;

    function DOMAIN_SEPARATOR() public view returns (bytes32) {
        return keccak256(
            abi.encode(
                keccak256(
                    "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"
                ),
                keccak256(bytes(name)),
                keccak256(bytes(version)),
                block.chainid,
                address(this)
            )
        );
    }

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
        emit Transfer(address(0), to, amount);
    }

    function setFailTransfers(bool fail) external {
        failTransfers = fail;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        emit Approval(msg.sender, spender, amount);
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        _move(msg.sender, to, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        if (failTransfers) return false;
        require(allowance[from][msg.sender] >= amount, "allowance");
        allowance[from][msg.sender] -= amount;
        _move(from, to, amount);
        return true;
    }

    function digest(address owner, address spender, uint256 value, uint256 nonce, uint256 deadline)
        public
        view
        returns (bytes32)
    {
        return keccak256(
            abi.encodePacked(
                "\x19\x01",
                DOMAIN_SEPARATOR(),
                keccak256(abi.encode(PERMIT_TYPEHASH, owner, spender, value, nonce, deadline))
            )
        );
    }

    function permit(
        address owner,
        address spender,
        uint256 value,
        uint256 deadline,
        uint8 v,
        bytes32 r,
        bytes32 s
    ) external {
        permit(owner, spender, value, deadline, abi.encodePacked(r, s, v));
    }

    function permit(address owner, address spender, uint256 value, uint256 deadline, bytes memory signature)
        public
    {
        require(block.timestamp <= deadline, "expired");
        bytes32 h = digest(owner, spender, value, nonces[owner], deadline);
        if (owner.code.length > 0) {
            require(IERC1271(owner).isValidSignature(h, signature) == 0x1626ba7e, "invalid signature");
        } else {
            require(signature.length == 65, "invalid signature");
            bytes32 r;
            bytes32 s;
            uint8 v;
            assembly {
                r := mload(add(signature, 32))
                s := mload(add(signature, 64))
                v := byte(0, mload(add(signature, 96)))
            }
            address signer = ecrecover(h, v, r, s);
            require(signer != address(0) && signer == owner, "invalid signature");
        }
        ++nonces[owner];
        allowance[owner][spender] = value;
        emit Approval(owner, spender, value);
    }

    function _move(address from, address to, uint256 amount) internal {
        require(balanceOf[from] >= amount, "balance");
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        emit Transfer(from, to, amount);
    }
}

/// A contract wallet that answers EIP-1271 for one signer's 65-byte signature over the hash itself.
contract SignerWallet {
    address public immutable signer;

    constructor(address signer_) {
        signer = signer_;
    }

    function isValidSignature(bytes32 hash, bytes memory signature) external view returns (bytes4) {
        if (signature.length != 65) return 0xffffffff;
        bytes32 r;
        bytes32 s;
        uint8 v;
        assembly {
            r := mload(add(signature, 32))
            s := mload(add(signature, 64))
            v := byte(0, mload(add(signature, 96)))
        }
        return ecrecover(hash, v, r, s) == signer ? bytes4(0x1626ba7e) : bytes4(0xffffffff);
    }

    function call(address to, bytes calldata data) external returns (bytes memory) {
        require(msg.sender == signer, "not signer");
        (bool ok, bytes memory ret) = to.call(data);
        if (!ok) {
            assembly {
                revert(add(ret, 32), mload(ret))
            }
        }
        return ret;
    }
}
