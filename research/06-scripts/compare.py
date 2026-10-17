# Copyright (c) 2026 Pagos Cube LLC
# SPDX-License-Identifier: Apache-2.0 OR MIT
import json, os, subprocess
d = json.load(open("smoldot-outputs.json"))
for o in d["outs"]:
    body = json.dumps({"id": 1, "jsonrpc": "2.0", "method": "state_call", "params": [o["fn"], o["params"], d["hash"]]})
    out = subprocess.run(["curl", "-s", "-m", "30", "-H", "Content-Type: application/json", "-d", body, os.environ.get("KREIVO_HTTP", "https://kreivo.io")], capture_output=True, text=True).stdout
    r = json.loads(out); rpc = r.get("result")
    print(f'{o["fn"]:32} smoldot={o["event"]:18} identical_to_full_node={rpc == o["output"]}  ({(len(rpc or "")-2)//2} bytes){"  RPC error: "+str(r.get("error")) if rpc is None else ""}')
