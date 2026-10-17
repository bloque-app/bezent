// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Usage: node dryrun.mjs ws|light
import { createClient, Binary } from "polkadot-api";
import { getWsProvider } from "polkadot-api/ws";
import { getSmProvider } from "polkadot-api/sm-provider";
import { start } from "polkadot-api/smoldot";
import { kusama as ksm } from "@polkadot-api/known-chains";
import fs from "node:fs";

const mode = process.argv[2];
const ORIGIN = "5EYCAe5ijiYfyeZ2JJCGq56LmPyNRAKzpG4QkoQkkQNB5e6Z"; // Treasury, used only as a dry-run origin
const t0 = Date.now(); const ts = () => ((Date.now() - t0) / 1000).toFixed(1) + "s";

let provider, smoldot;
if (mode === "ws") provider = getWsProvider(process.env.KREIVO_WS ?? "wss://kreivo.io");
else {
  smoldot = start({ maxLogLevel: 3 });
  const relay = await smoldot.addChain({ chainSpec: ksm });
  const spec = JSON.parse(fs.readFileSync("kreivo.json", "utf8"));
  spec.bootNodes = [18, 19, 20, 21].map(n => `/ip4/104.206.91.${n}/tcp/30333/p2p/12D3KooWP1mhED5zhk5ghkdYH1TSEVTVX2s2hSADz67oLfiYeeXF`);
  const inner = getSmProvider(() => smoldot.addChain({ chainSpec: JSON.stringify(spec), potentialRelayChains: [relay] }));
  provider = (onMsg) => { const c = inner((m) => { const j = typeof m === "string" ? JSON.parse(m) : m; const ev = j.params?.result?.event; if (false) console.log(`[light] <- ${j.id ? "resp " + j.id + (j.error ? " ERROR " + JSON.stringify(j.error) : "") : (ev ?? j.method)}`); onMsg(m); }); return { ...c, send: (m) => { const j = typeof m === "string" ? JSON.parse(m) : m; if (false) console.log(`[light] -> ${j.id} ${j.method} ${j.method === "chainHead_v1_call" ? j.params[2] : ""}`); c.send(m); } }; };
}
const client = createClient(provider);
const api = client.getUnsafeApi();
const fin = await client.getFinalizedBlock();
console.log(`[${mode}] ${ts()} finalized #${fin.number}`);
const at = { at: fin.hash };

async function run(name, f) {
  const s = Date.now();
  try { const r = await f(); console.log(`[${mode}] ${name}: OK (${Date.now() - s} ms) ${r}`); }
  catch (e) { console.log(`[${mode}] ${name}: FAIL (${Date.now() - s} ms) ${String(e?.message ?? e).slice(0, 300)}`); }
}
const big = (x) => JSON.stringify(x, (_, v) => typeof v === "bigint" ? v.toString() : (v instanceof Uint8Array ? "0x" + Buffer.from(v).toString("hex") : v));

await run("T1 read-only runtime call (AccountNonceApi)", async () => "nonce=" + await api.apis.AccountNonceApi.account_nonce(ORIGIN, at));
await run("T2 DryRunApi.dry_run_call(System.remark_with_event)", async () => {
  const call = api.tx.System.remark_with_event({ remark: Binary.fromText("probe") }).decodedCall;
  const r = await api.apis.DryRunApi.dry_run_call({ type: "system", value: { type: "Signed", value: ORIGIN } }, call, 5, at);
  return `success=${r.success} events=${r.value?.emitted_events?.length}`;
});
for (const [label, file] of [["no topic", "evt0.wasm"], ["one topic", "evt1.wasm"]]) {
  await run(`T3 ContractsApi.instantiate, contract emits an event with ${label}`, async () => {
    const code = Binary.fromHex("0x" + fs.readFileSync(file).toString("hex"));
    const r = await api.apis.ContractsApi.instantiate(ORIGIN, 0n, undefined, undefined, { type: "Upload", value: code }, Binary.fromHex("0x"), Binary.fromText("salt-" + Date.now()), at);
    return `result=${big(r.result).slice(0, 120)} gas=${big(r.gas_required)} events=${r.events?.length ?? "n/a"}`;
  });
}
client.destroy(); smoldot?.terminate(); process.exit(0);
