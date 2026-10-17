// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
import { createClient } from "polkadot-api";
import { getWsProvider } from "polkadot-api/ws";
const client = createClient(getWsProvider(process.env.KREIVO_WS ?? "wss://kreivo.io"));
const api = client.getUnsafeApi();
const all = await api.query.Assets.Asset.getEntries();
const md = await api.query.Assets.Metadata.getEntries();
const name = new Map(md.map(e => [JSON.stringify(e.keyArgs[0]), e.value]));
const fmt = (k) => k.type === "Here" ? `Here(${k.value})` : `${k.type}(${JSON.stringify(k.value, (_, v) => typeof v === "bigint" ? v.toString() : v)})`;
for (const e of all.filter(e => e.keyArgs[0].type === "Here").sort((x, y) => x.keyArgs[0].value - y.keyArgs[0].value)) {
  const m = name.get(JSON.stringify(e.keyArgs[0]));
  const sym = m ? Buffer.from(m.symbol.asBytes ? m.symbol.asBytes() : m.symbol).toString() : "?";
  console.log(fmt(e.keyArgs[0]), "symbol", sym, "decimals", m?.decimals, "sufficient", e.value.is_sufficient, "status", e.value.status.type ?? e.value.status, "supply", e.value.supply.toString());
}
console.log("Here(103) exists:", all.some(e => e.keyArgs[0].type === "Here" && e.keyArgs[0].value === 103));
client.destroy(); process.exit(0);
