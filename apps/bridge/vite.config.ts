// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The build (F-018, T-018-12). Reproducible: no timestamp, no source map, content-hashed file names, and every
// input pinned (the lock file, the Node version in package.json's `engines`, the deployment's networks, the
// chain specifications). CI builds twice and compares the hashes (scripts/hash.mjs); README.md says how to
// reproduce a published hash.
//
// Build-time settings:
//   BRIDGE_DEPLOYMENT               testbed | test | production; required by `vite build` (the dev server and the
//                                   tests default to test)
//   BRIDGE_TESTBED_DIRECTED_DEPOSIT the test bed's DirectedDeposit address (testbed builds only)
//   BRIDGE_TESTBED_RESERVE          its reserve() (testbed builds only)
//   BRIDGE_TESTBED_CODE_HASH        keccak256 of its runtime code (testbed builds only)
//   BRIDGE_TESTBED_BEZENT           the bezent contract the bed's Kreivo deployed, 32 bytes as hex (testbed only)
//   BRIDGE_TESTBED_EMITTER          the bed's vault reference emitter (testbed builds only)
//   BRIDGE_TESTBED_PAYOUTS_FROM     the block the status page searches payouts from (testbed builds only)
//   BRIDGE_STATUS_URL               the public status route (T-018-10), https; unset (the default), the status
//                                   page reads chain data alone
//   BRIDGE_REVISION                 the revision shown in the page (default: git's full HEAD, else "unknown")

import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import type { Plugin } from 'vite';
import { defineConfig } from 'vitest/config';
import { csp, headersFile } from './csp';
import type { Deployment } from './src/config/networks';

const here = (p: string) => fileURLToPath(new URL(p, import.meta.url));
const KREIVO_CHAIN_SPEC = here('../../services/kreivo-endpoint/chain-specs/kreivo.json');
/** Where `polkadot-api/chains/kusama` comes from (it re-exports `@polkadot-api/known-chains/kusama`). */
const KNOWN_CHAINS = here('./node_modules/@polkadot-api/known-chains');
const sha256 = (s: string) => createHash('sha256').update(s).digest('hex');

function deployment(command: 'build' | 'serve'): Deployment {
  const d = process.env.BRIDGE_DEPLOYMENT;
  if (d === undefined || d === '') {
    // A build names what it is for: a default would let a production build be made by accident, or a test one
    // be published as production.
    if (command === 'build') throw new Error('BRIDGE_DEPLOYMENT is not set: name testbed, test or production for the build');
    return 'test';
  }
  if (d !== 'testbed' && d !== 'test' && d !== 'production') throw new Error(`BRIDGE_DEPLOYMENT=${d}: not testbed, test or production`);
  return d;
}

function testbedPins(d: Deployment): Record<'directedDeposit' | 'reserve' | 'codeHash' | 'bezent' | 'emitter' | 'payoutsFrom', string> {
  const read = (name: string, shape: RegExp, what: string) => {
    const v = process.env[name] ?? '';
    if (v === '') return '';
    if (d !== 'testbed') throw new Error(`${name} is for testbed builds only`);
    if (!shape.test(v)) throw new Error(`${name}=${v}: not ${what}`);
    return v;
  };
  return {
    directedDeposit: read('BRIDGE_TESTBED_DIRECTED_DEPOSIT', /^0x[0-9a-fA-F]{40}$/, 'an address'),
    reserve: read('BRIDGE_TESTBED_RESERVE', /^0x[0-9a-fA-F]{40}$/, 'an address'),
    codeHash: read('BRIDGE_TESTBED_CODE_HASH', /^0x[0-9a-f]{64}$/, '32 bytes of lowercase hex'),
    bezent: read('BRIDGE_TESTBED_BEZENT', /^0x[0-9a-f]{64}$/, '32 bytes of lowercase hex'),
    emitter: read('BRIDGE_TESTBED_EMITTER', /^0x[0-9a-fA-F]{40}$/, 'an address'),
    payoutsFrom: read('BRIDGE_TESTBED_PAYOUTS_FROM', /^(0|[1-9][0-9]{0,15})$/, 'a block number'),
  };
}

/** The public status route's base URL (T-018-10), or '' when unset: https only, or a loopback http URL for a test
 * bed build, with no credentials, query or fragment. It is fixed in the build, so the bundle's hash covers it, and
 * the CSP lets the page reach that origin alone. */
function statusRoute(d: Deployment): string {
  const v = process.env.BRIDGE_STATUS_URL ?? '';
  if (v === '') return '';
  let u: URL;
  try {
    u = new URL(v);
  } catch {
    throw new Error(`BRIDGE_STATUS_URL=${v}: not a URL`);
  }
  const loopback = /^(127\.0\.0\.1|localhost|\[::1\])$/.test(u.hostname);
  if (!(u.protocol === 'https:' || (d === 'testbed' && u.protocol === 'http:' && loopback))) {
    throw new Error(`BRIDGE_STATUS_URL=${v}: https only (a test bed build may name a loopback http URL)`);
  }
  if (u.username || u.password || u.search || u.hash) throw new Error(`BRIDGE_STATUS_URL=${v}: no credentials, query or fragment`);
  return u.href.replace(/\/$/, '');
}

function revision(): string {
  if (process.env.BRIDGE_REVISION) return process.env.BRIDGE_REVISION;
  try {
    return execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
  } catch {
    return 'unknown';
  }
}

/** The bundle's hash depends on the Node that built it only through its tools, but a published hash is
 * reproduced with the pinned version (package.json `engines.node`); say so when another one builds. */
function checkNode(command: 'build' | 'serve') {
  if (command !== 'build') return;
  const want = JSON.parse(readFileSync(here('./package.json'), 'utf8')).engines?.node as string | undefined;
  if (want && process.version !== `v${want}`) {
    console.warn(`warning: Node ${process.version} builds this bundle; the published hashes are made with Node v${want}.`);
  }
}

/** Ships the chain specifications as their own files (Kreivo's is GPL-3.0-only data, Kusama's comes from
 * @polkadot-api/known-chains: docs/oss/LICENSING.md §4.4), writes `_headers`, and puts the CSP in the page. */
function bridge(d: Deployment, specs: Record<'kreivo' | 'kusama', string>, route: string): Plugin {
  return {
    name: 'bezent-bridge',
    transformIndexHtml: {
      order: 'pre',
      handler: (html, ctx) =>
        // The dev server injects its own client and talks to it over a WebSocket; only a build is strict.
        html.replace('%BRIDGE_CSP_META%', ctx.server ? '' : csp(d, { forMeta: true, statusRoute: route })),
    },
    generateBundle() {
      for (const [name, spec] of Object.entries(specs)) {
        this.emitFile({ type: 'asset', fileName: `chain-specs/${name}.json`, source: spec });
      }
      this.emitFile({ type: 'asset', fileName: '_headers', source: headersFile(d, route) });
    },
    configureServer(server) {
      for (const [name, spec] of Object.entries(specs)) {
        server.middlewares.use(`/chain-specs/${name}.json`, (_req, res) => {
          res.setHeader('content-type', 'application/json');
          res.end(spec);
        });
      }
    },
  };
}

/** viem's CCIP-Read (EIP-3668) would let a contract's revert send the page to fetch a URL the contract names.
 * The clients set `ccipRead: false`; this also leaves the code out of the bundle, replacing viem's module with
 * src/evm/no-ccip.ts. */
function noCcip(): Plugin {
  const stub = here('./src/evm/no-ccip.ts');
  return {
    name: 'bezent-bridge-no-ccip',
    enforce: 'pre',
    async resolveId(source, importer, options) {
      if (!/(^|\/)ccip\.js$/.test(source) || !importer?.replace(/\\/g, '/').includes('/node_modules/viem/')) return null;
      const r = await this.resolve(source, importer, { ...options, skipSelf: true });
      return r && /\/node_modules\/viem\/_esm\/utils\/ccip\.js$/.test(r.id.replace(/\\/g, '/')) ? stub : null;
    },
  };
}

const RULE = '='.repeat(78);

/** The third-party packages bundled into the page and its worker, with their licences, and the files shipped
 * beside the page, as `THIRD-PARTY-NOTICES.txt` (minification drops the packages' notices from the code). The
 * worker is built first, so the main build's `generateBundle` sees both. A package that ships no licence file
 * takes its notice from notices/ (smoldot, scale-ts); a bundled package with neither fails the build. */
function notices(): { collect: Plugin; emit: Plugin } {
  const dirs = new Set<string>();
  const collect: Plugin = {
    name: 'bezent-bridge-collect-packages',
    generateBundle(_, bundle) {
      for (const out of Object.values(bundle)) {
        if (out.type !== 'chunk') continue;
        for (const id of out.moduleIds) {
          const m = /^(.*\/node_modules\/(?:@[^/]+\/)?[^/]+)\//.exec(id.replace(/\\/g, '/'));
          if (m) dirs.add(m[1]!);
        }
      }
    },
  };
  const text = (p: string) => readFileSync(here(p), 'utf8').trim();
  const gpl = text('../../LICENSES/GPL-3.0-or-later.txt');
  const classpath = text('../../LICENSES/Classpath-exception-2.0.txt');
  const own = (name: string) => {
    const p = here(`./notices/${name.replace(/^@/, '').replace(/\//g, '__')}.txt`);
    return existsSync(p) ? readFileSync(p, 'utf8').trim() : '';
  };
  const emit: Plugin = {
    ...collect,
    name: 'bezent-bridge-notices',
    generateBundle(options, bundle, isWrite) {
      (collect.generateBundle as (o: unknown, b: unknown, w: unknown) => void).call(this, options, bundle, isWrite);
      const seen = new Map<string, string>();
      // Kusama's chain specification ships as its own file, so known-chains is no longer in a chunk: its notice
      // goes in all the same.
      dirs.add(KNOWN_CHAINS);
      let knownChains = '';
      for (const dir of dirs) {
        const pkg = JSON.parse(readFileSync(`${dir}/package.json`, 'utf8'));
        const key = `${pkg.name}@${pkg.version}`;
        if (seen.has(key)) continue;
        if (pkg.name === '@polkadot-api/known-chains') knownChains = key;
        const file = readdirSync(dir).find((f) => /^(licen[cs]e|copying)(\.|$)/i.test(f));
        const body = (file ? readFileSync(`${dir}/${file}`, 'utf8').trim() : '') || own(pkg.name);
        if (!body) throw new Error(`${key} (${pkg.license ?? 'no licence'}) is bundled with no licence notice: add apps/bridge/notices/`);
        seen.set(key, `${key}\nLicence: ${pkg.license ?? 'unknown'}\n\n${body}\n`);
      }
      const packages = [...seen.keys()].sort().map((k) => seen.get(k)).join(`\n${RULE}\n\n`);
      const files = [
        `chain-specs/kreivo.json\nLicence: GPL-3.0-only\n\nKreivo's chain specification, Copyright (c) Virto Network, licensed under the GNU General Public\nLicense version 3 only (its text is at the end of this file). Copied verbatim from Kreivo's publication\n(services/kreivo-endpoint/chain-specs/kreivo.json in bezent's repository, which is its source); a separate\nfile the page fetches and checks against its SHA-256, aggregated beside the app (GPL-3.0 section 5), not\ncombined with it.\n`,
        `chain-specs/kusama.json\nLicence: as ${knownChains} (MIT, its notice is above)\n\nKusama's chain specification (its genesis, boot nodes and a light-sync checkpoint), verbatim as the package\n${knownChains} ships it, through polkadot-api/chains/kusama. It is the network's data, and it ships as a\nseparate file the page fetches and checks against its SHA-256, not inside the app's code: whatever terms it\ncarries upstream (Polkadot's own chain specifications are GPL-3.0-only), it is aggregated beside the app\n(GPL-3.0 section 5) and does not extend them to the app.\n`,
      ].join(`\n${RULE}\n\n`);
      this.emitFile({
        type: 'asset',
        fileName: 'THIRD-PARTY-NOTICES.txt',
        source: [
          'bezent bridge: the third-party packages in this bundle, the files shipped beside the page, and their licences.',
          `${RULE}\nPACKAGES\n${RULE}\n\n${packages}`,
          `${RULE}\nFILES BESIDE THE PAGE\n${RULE}\n\n${files}`,
          `${RULE}\nTHE GNU GENERAL PUBLIC LICENSE, VERSION 3\n(smoldot: GPL-3.0-or-later WITH Classpath-exception-2.0; chain-specs/kreivo.json: GPL-3.0-only)\n${RULE}\n\n${gpl}\n`,
          `${RULE}\nTHE CLASSPATH EXCEPTION 2.0 (smoldot)\n${RULE}\n\n${classpath}\n`,
        ].join('\n\n'),
      });
    },
  };
  return { collect, emit };
}

export default defineConfig(async ({ command }) => {
  const d = deployment(command);
  checkNode(command);
  const { chainSpec: kusama } = await import('polkadot-api/chains/kusama');
  const specs = { kreivo: readFileSync(KREIVO_CHAIN_SPEC, 'utf8'), kusama };
  const n = notices();
  const route = statusRoute(d);
  return {
    base: './',
    define: {
      __BRIDGE_DEPLOYMENT__: JSON.stringify(d),
      __BRIDGE_TESTBED__: JSON.stringify(testbedPins(d)),
      __BRIDGE_REVISION__: JSON.stringify(revision()),
      __BRIDGE_STATUS_URL__: JSON.stringify(route),
      __KREIVO_CHAIN_SPEC_SHA256__: JSON.stringify(sha256(specs.kreivo)),
      __KUSAMA_CHAIN_SPEC_SHA256__: JSON.stringify(sha256(specs.kusama)),
    },
    plugins: [noCcip(), bridge(d, specs, route), n.emit],
    build: {
      target: 'es2022',
      sourcemap: false,
      emptyOutDir: true,
      reportCompressedSize: false,
      // smoldot's worker carries its WebAssembly: it is large.
      chunkSizeWarningLimit: 4096,
      modulePreload: { polyfill: false },
    },
    worker: { format: 'es' as const, plugins: () => [n.collect] },
    test: {
      include: ['test/**/*.test.ts'],
      // Node by default; the page's own tests (test/page.test.ts) ask for happy-dom.
      environment: 'node',
    },
  };
});
