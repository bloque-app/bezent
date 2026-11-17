// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The build (F-018, T-018-12). Reproducible: no timestamp, no source map, content-hashed file names, and every
// input pinned (the lock file, the deployment's networks, Kreivo's chain specification). CI builds twice and
// compares the hashes (scripts/hash.mjs).
//
// Build-time settings, all optional:
//   BRIDGE_DEPLOYMENT               testbed | test | production (default: test)
//   BRIDGE_TESTBED_DIRECTED_DEPOSIT the test bed's DirectedDeposit address (testbed builds only)
//   BRIDGE_REVISION                 the revision shown in the page (default: git's HEAD, else "unknown")

import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import type { Plugin } from 'vite';
import { defineConfig } from 'vitest/config';
import { csp, headersFile } from './csp';
import type { Deployment } from './src/config/networks';

const KREIVO_CHAIN_SPEC = fileURLToPath(new URL('../../services/kreivo-endpoint/chain-specs/kreivo.json', import.meta.url));

function deployment(): Deployment {
  const d = process.env.BRIDGE_DEPLOYMENT ?? 'test';
  if (d !== 'testbed' && d !== 'test' && d !== 'production') throw new Error(`BRIDGE_DEPLOYMENT=${d}: not testbed, test or production`);
  return d;
}

function testbedAddress(d: Deployment): string {
  const a = process.env.BRIDGE_TESTBED_DIRECTED_DEPOSIT ?? '';
  if (a === '') return '';
  if (d !== 'testbed') throw new Error('BRIDGE_TESTBED_DIRECTED_DEPOSIT is for testbed builds only');
  if (!/^0x[0-9a-fA-F]{40}$/.test(a)) throw new Error(`BRIDGE_TESTBED_DIRECTED_DEPOSIT=${a}: not an address`);
  return a;
}

function revision(): string {
  if (process.env.BRIDGE_REVISION) return process.env.BRIDGE_REVISION;
  try {
    return execFileSync('git', ['rev-parse', '--short=12', 'HEAD'], { encoding: 'utf8' }).trim();
  } catch {
    return 'unknown';
  }
}

/** Ships Kreivo's chain specification as its own file (it is Kreivo's, GPL-3.0-only data: docs/oss/
 * LICENSING.md), writes `_headers`, and puts the CSP in the page. */
function bridge(d: Deployment, spec: string): Plugin {
  return {
    name: 'bezent-bridge',
    transformIndexHtml: {
      order: 'pre',
      handler: (html, ctx) =>
        // The dev server injects its own client and talks to it over a WebSocket; only a build is strict.
        html.replace('%BRIDGE_CSP_META%', ctx.server ? '' : csp(d, { forMeta: true })),
    },
    generateBundle() {
      this.emitFile({ type: 'asset', fileName: 'chain-specs/kreivo.json', source: spec });
      this.emitFile({ type: 'asset', fileName: '_headers', source: headersFile(d) });
    },
    configureServer(server) {
      server.middlewares.use('/chain-specs/kreivo.json', (_req, res) => {
        res.setHeader('content-type', 'application/json');
        res.end(spec);
      });
    },
  };
}

/** The third-party packages bundled into the page and its worker, with their licences, as
 * `THIRD-PARTY-NOTICES.txt` beside the page (minification drops their notices from the code). The worker is
 * built first, so the main build's `generateBundle` sees both. smoldot is GPL-3.0-or-later with the Classpath
 * exception and ships no licence file: the GPL's text is added from LICENSES/ (docs/oss/LICENSING.md §4.4). */
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
  const gpl = readFileSync(fileURLToPath(new URL('../../LICENSES/GPL-3.0-or-later.txt', import.meta.url)), 'utf8');
  const emit: Plugin = {
    ...collect,
    name: 'bezent-bridge-notices',
    generateBundle(options, bundle, isWrite) {
      (collect.generateBundle as (o: unknown, b: unknown, w: unknown) => void).call(this, options, bundle, isWrite);
      const seen = new Map<string, string>();
      for (const dir of dirs) {
        const pkg = JSON.parse(readFileSync(`${dir}/package.json`, 'utf8'));
        const key = `${pkg.name}@${pkg.version}`;
        if (seen.has(key)) continue;
        const file = readdirSync(dir).find((f) => /^(licen[cs]e|copying)(\.|$)/i.test(f));
        let text = file && existsSync(`${dir}/${file}`) ? readFileSync(`${dir}/${file}`, 'utf8').trim() : '';
        if (!text && pkg.license === 'GPL-3.0-or-later WITH Classpath-exception-2.0') {
          text = `Source: ${pkg.homepage ?? ''}, release ${pkg.version}. Unmodified.\nThe GNU GPL version 3 follows; the Classpath exception 2.0 is at https://spdx.org/licenses/Classpath-exception-2.0.html.\n\n${gpl.trim()}`;
        }
        seen.set(key, `${key}\nLicence: ${pkg.license ?? 'unknown'}\n\n${text || '(no licence file in the package)'}\n`);
      }
      const body = [...seen.keys()].sort().map((k) => seen.get(k)).join(`\n${'='.repeat(78)}\n\n`);
      this.emitFile({
        type: 'asset',
        fileName: 'THIRD-PARTY-NOTICES.txt',
        source: `bezent bridge: the third-party packages in this bundle and their licences.\n\n${'='.repeat(78)}\n\n${body}`,
      });
    },
  };
  return { collect, emit };
}

export default defineConfig(() => {
  const d = deployment();
  const spec = readFileSync(KREIVO_CHAIN_SPEC, 'utf8');
  const n = notices();
  return {
    base: './',
    define: {
      __BRIDGE_DEPLOYMENT__: JSON.stringify(d),
      __BRIDGE_TESTBED_DIRECTED_DEPOSIT__: JSON.stringify(testbedAddress(d)),
      __BRIDGE_REVISION__: JSON.stringify(revision()),
      __KREIVO_CHAIN_SPEC_SHA256__: JSON.stringify(createHash('sha256').update(spec).digest('hex')),
    },
    plugins: [bridge(d, spec), n.emit],
    build: {
      target: 'es2022',
      sourcemap: false,
      emptyOutDir: true,
      reportCompressedSize: false,
      // smoldot's worker carries its WebAssembly, and Kusama's specification its checkpoint: both are large.
      chunkSizeWarningLimit: 4096,
      modulePreload: { polyfill: false },
    },
    worker: { format: 'es' as const, plugins: () => [n.collect] },
    test: {
      include: ['test/**/*.test.ts'],
      environment: 'node',
    },
  };
});
