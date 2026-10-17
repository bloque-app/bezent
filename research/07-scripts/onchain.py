# Copyright (c) 2026 Pagos Cube LLC
# SPDX-License-Identifier: Apache-2.0 OR MIT
import json, subprocess
def call(rpc, to, data):
    body=json.dumps({"jsonrpc":"2.0","id":1,"method":"eth_call","params":[{"to":to,"data":data},"latest"]})
    out=subprocess.run(["curl","-s","-m","20","-H","Content-Type: application/json","-d",body,rpc],capture_output=True,text=True).stdout
    try: return json.loads(out).get("result")
    except Exception: return "ERR "+out[:120]
def a(x): return x.lower().replace("0x","").rjust(64,"0")
def u(n): return hex(n)[2:].rjust(64,"0")
def dec_str(h):
    if not h or h=="0x" or h.startswith("ERR"): return h
    b=bytes.fromhex(h[2:]); 
    if len(b)>=96: l=int.from_bytes(b[32:64],"big"); return b[64:64+l].decode(errors="replace")
    return b.rstrip(b"\0").decode(errors="replace")
GETPOOL="0x1698ee82"; BAL="0x70a08231"; SYM="0x95d89b41"; SUP="0x18160ddd"; DEC="0x313ce567"
NETS={
 "polygon":{"rpc":"https://polygon-bor-rpc.publicnode.com","factory":"0x1F98431c8aD98523631AE4a59f267346ea31F984",
            "USDC":"0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359","USDT":"0xc2132D05D31c914a87C6611C10748AEb04B58e8F"},
 "base":{"rpc":"https://base-rpc.publicnode.com","factory":"0x33128a8fC17869897dcE68Ed026d694621f6FDfD",
         "USDC":"0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913","USDT":"0xfde4C96c8593536E31F229EA8f37b2ADa2699bb2"},
}
for n,c in NETS.items():
    print(f"== {n}")
    for t in ("USDC","USDT"):
        s=dec_str(call(c["rpc"],c[t],SYM)); sup=call(c["rpc"],c[t],SUP); d=call(c["rpc"],c[t],DEC)
        dd=int(d,16) if d and d.startswith("0x") and len(d)>2 else 6
        print(f"  {t} {c[t]} symbol={s} decimals={dd} supply={int(sup,16)/10**dd:,.0f}" if sup and sup.startswith("0x") and len(sup)>2 else f"  {t} {sup}")
    for fee in (100,500,3000):
        p=call(c["rpc"],c["factory"],GETPOOL+a(c["USDC"])+a(c["USDT"])+u(fee))
        pool="0x"+p[-40:] if p and p.startswith("0x") else p
        if int(pool,16)==0: print(f"  uniswap v3 fee {fee/1e6:.2%}: no pool"); continue
        b1=int(call(c["rpc"],c["USDC"],BAL+a(pool)),16)/1e6; b2=int(call(c["rpc"],c["USDT"],BAL+a(pool)),16)/1e6
        print(f"  uniswap v3 fee {fee/1e6:.2%}: pool {pool} holds USDC {b1:,.0f} + USDT {b2:,.0f}")
