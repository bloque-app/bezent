#!/usr/bin/env node
// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// F-013 (T-013-02): read, from Kreivo mainnet and read-only, what the genesis override copies:
// name, symbol, decimals, sufficiency, minimum balance and roles of Here(100) to Here(105). The
// values are written by hand into `e2e/network/src/lib.rs`; run this to see whether mainnet moved.
//
//   KREIVO_MAINNET_WS=wss://kreivo.io node mainnet-metadata.mjs
//
// Read on 2026-10-13: Here(101) to Here(103) already carry bUSDC, bEURC and bBTC; MXNB and BRLB,
// which sat at Here(101) and Here(102) before that, are not yet recreated at Here(104) and
// Here(105) (PLAN.md DEC-7), so their metadata was read at an earlier block.

import { Binary, createClient } from 'polkadot-api';
import { getWsProvider } from 'polkadot-api/ws';

const url = process.env.KREIVO_MAINNET_WS ?? 'wss://kreivo.io';
const client = createClient(getWsProvider(url));
const api = client.getUnsafeApi();
const text = (b) => Binary.toText(b);

try {
  const v = await api.constants.System.Version();
  console.log(`Kreivo mainnet at ${url}: ${v.spec_name} ${v.spec_version}`);
  for (const id of [100, 101, 102, 103, 104, 105]) {
    const key = { type: 'Here', value: id };
    const [asset, meta] = await Promise.all([
      api.query.Assets.Asset.getValue(key),
      api.query.Assets.Metadata.getValue(key),
    ]);
    console.log(
      `Here(${id})`,
      JSON.stringify({
        exists: Boolean(asset),
        is_sufficient: asset?.is_sufficient,
        min_balance: asset?.min_balance?.toString(),
        status: asset?.status?.type,
        owner: asset?.owner,
        issuer: asset?.issuer,
        admin: asset?.admin,
        name: meta ? text(meta.name) : null,
        symbol: meta ? text(meta.symbol) : null,
        decimals: meta?.decimals,
      }),
    );
  }
} finally {
  client.destroy();
}
