# Copyright (c) 2026 Pagos Cube LLC
# SPDX-License-Identifier: Apache-2.0 OR MIT
from onchain import call, a, u, dec_str, NETS
import json, subprocess
QUOTER={"polygon":"0x61fFE014bA17989E743c5F6cB21bF9697530B21e","base":"0x3d4e44Eb1374240CE5F1B871ab261CD16335B76a"}
QSEL="0xc6a5026a"  # quoteExactInputSingle((address,address,uint256,uint24,uint160))
def gasprice(rpc):
    body=json.dumps({"jsonrpc":"2.0","id":1,"method":"eth_gasPrice","params":[]})
    return int(json.loads(subprocess.run(["curl","-s","-m","20","-H","Content-Type: application/json","-d",body,rpc],capture_output=True,text=True).stdout)["result"],16)
for n,c in NETS.items():
    gp=gasprice(c["rpc"]); print(f"== {n}: gas price {gp/1e9:.4f} gwei")
    for amt in (1,10,100,1000,10000,100000):
        r=call(c["rpc"],QUOTER[n],QSEL+a(c["USDT"])+a(c["USDC"])+u(amt*10**6)+u(100)+u(0))
        if not r or not r.startswith("0x") or len(r)<10: print(f"  {amt:>7} USDT -> err {r}"); continue
        out=int(r[2:66],16)/1e6; gas=int(r[2+64*3:2+64*4],16)
        print(f"  {amt:>7} USDT -> {out:,.4f} USDC  (cost {100*(amt-out)/amt:.3f}%; ~{gas:,} gas for the swap = {gas*gp/1e18:.8f} native)")
# Base USDT provenance: OptimismMintableERC20 exposes remoteToken()/l1Token() and bridge()
rpc=NETS["base"]["rpc"]; t=NETS["base"]["USDT"]
print("base USDT name:", dec_str(call(rpc,t,"0x06fdde03")))
for sel,nm in (("0xd6c0b2c4","remoteToken()"),("0xc01e1bd6","l1Token()"),("0xe78cea92","bridge()"),("0x893d20e8","getOwner()")):
    r=call(rpc,t,sel); print(f"  {nm}:", ("0x"+r[-40:]) if r and r.startswith("0x") and len(r)>=66 else r)
# Aerodrome Slipstream (CL) on Base: factory.getPool(tokenA, tokenB, int24 tickSpacing)
AERO="0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A"
for ts in (1,50,100):
    p=call(rpc,AERO,"0x28af8d0b"+a(NETS["base"]["USDC"])+a(t)+u(ts))
    pool="0x"+p[-40:] if p and p.startswith("0x") and len(p)>=66 else None
    if pool and int(pool,16):
        b1=int(call(rpc,NETS["base"]["USDC"],"0x70a08231"+a(pool)),16)/1e6; b2=int(call(rpc,t,"0x70a08231"+a(pool)),16)/1e6
        print(f"aerodrome CL tickSpacing {ts}: {pool} USDC {b1:,.0f} + USDT {b2:,.0f}")
    else: print(f"aerodrome CL tickSpacing {ts}: none ({p})")
