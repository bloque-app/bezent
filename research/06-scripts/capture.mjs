// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Run the dry-run tests over RPC, recording every chainHead_v1_call PAPI sends (function + params).
import { createClient, Binary } from "polkadot-api";
import { getWsProvider } from "polkadot-api/ws";
import fs from "node:fs";
const calls = [];
const base = getWsProvider(process.env.KREIVO_WS ?? "wss://kreivo.io");
const provider = (onMsg) => { const c = base(onMsg); return { ...c, send: (m) => { try { const j = typeof m === "string" ? JSON.parse(m) : m; if (/call/i.test(j.method ?? "")) calls.push({ method: j.method, fn: j.method === "state_call" ? j.params[0] : j.params[2], params: j.method === "state_call" ? j.params[1] : j.params[3] }); } catch {} c.send(m); } }; };
const client = createClient(provider);
const api = client.getUnsafeApi();
const fin = await client.getFinalizedBlock();
const ORIGIN = "5EYCAe5ijiYfyeZ2JJCGq56LmPyNRAKzpG4QkoQkkQNB5e6Z";
const at = { at: fin.hash };
await api.apis.AccountNonceApi.account_nonce(ORIGIN, at);
const call = api.tx.System.remark_with_event({ remark: Binary.fromText("probe") }).decodedCall;
await api.apis.DryRunApi.dry_run_call({ type: "system", value: { type: "Signed", value: ORIGIN } }, call, 5, at);
for (const f of ["evt0.wasm", "evt1.wasm"]) {
  const code = Binary.fromHex("0x" + fs.readFileSync(f).toString("hex"));
  await api.apis.ContractsApi.instantiate(ORIGIN, 0n, undefined, undefined, { type: "Upload", value: code }, Binary.fromHex("0x"), Binary.fromText("salt-" + f), at);
}
fs.writeFileSync("calls.json", JSON.stringify(calls, null, 1));
console.log(calls.map(c => `${c.method} ${c.fn} (${(c.params.length - 2) / 2} bytes)`).join("\n"));
client.destroy(); process.exit(0);
