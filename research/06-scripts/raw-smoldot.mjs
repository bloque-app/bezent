// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Replays PAPI's captured runtime calls straight into smoldot's chainHead_v1 API, logging every event.
import { start } from "polkadot-api/smoldot";
import { kusama } from "@polkadot-api/known-chains";
import fs from "node:fs";
const calls = JSON.parse(fs.readFileSync("calls.json", "utf8"));
const t0 = Date.now(); const ts = () => ((Date.now() - t0) / 1000).toFixed(1) + "s";
const sm = start({ maxLogLevel: 3 });
const db = fs.existsSync("ksm-db.json") ? fs.readFileSync("ksm-db.json", "utf8") : undefined;
const relay = await sm.addChain({ chainSpec: kusama, databaseContent: db });
const spec = JSON.parse(fs.readFileSync("kreivo.json", "utf8"));
spec.bootNodes = [18, 19, 20, 21].map(n => `/ip4/104.206.91.${n}/tcp/30333/p2p/12D3KooWP1mhED5zhk5ghkdYH1TSEVTVX2s2hSADz67oLfiYeeXF`);
const para = await sm.addChain({ chainSpec: JSON.stringify(spec), potentialRelayChains: [relay] });
let id = 0; const pending = new Map(); const listeners = [];
const send = (method, params) => new Promise(r => { const i = ++id; pending.set(i, r); para.sendJsonRpc(JSON.stringify({ jsonrpc: "2.0", id: i, method, params })); });
(async () => { for (;;) { const m = JSON.parse(await para.nextJsonRpcResponse()); if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); } else for (const l of listeners) l(m); } })();
const next = (pred) => new Promise(r => { const l = (m) => { if (pred(m)) { listeners.splice(listeners.indexOf(l), 1); r(m); } }; listeners.push(l); });
const sub = (await send("chainHead_v1_follow", [true])).result;
const init = await next(m => m.params?.subscription === sub && m.params.result.event === "initialized");
const hash = init.params.result.finalizedBlockHashes.at(-1);
console.log(`[raw] ${ts()} initialized at ${hash}`);
const outs = [];
for (const c of calls) {
  const s = Date.now();
  const r = await send("chainHead_v1_call", [sub, hash, c.fn, c.params]);
  if (r.error || r.result?.result !== "started") { console.log(`[raw] ${c.fn}: not started ${JSON.stringify(r).slice(0, 300)}`); continue; }
  const op = r.result.operationId;
  const ev = await Promise.race([
    next(m => m.params?.subscription === sub && m.params.result.operationId === op),
    new Promise(res => setTimeout(() => res({ timeout: true }), 120000)),
  ]);
  const e = ev.params?.result;
  outs.push({ fn: c.fn, params: c.params, output: e?.output ?? null, event: e?.event ?? "timeout" });
  console.log(`[raw] ${c.fn}: ${ev.timeout ? "TIMEOUT 120s" : e.event} (${Date.now() - s} ms) ${e?.error ?? (e?.output ? "output " + ((e.output.length - 2) / 2) + " bytes" : "")}`);
}
fs.writeFileSync("smoldot-outputs.json", JSON.stringify({ hash, outs }));
sm.terminate(); process.exit(0);
