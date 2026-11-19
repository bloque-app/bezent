// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The page's Content Security Policy and response headers (F-018 plan part 4, "Defence": a strict CSP, no
// third-party script). Built from the deployment, so the test bed's build alone may reach a loopback node.
//
// - Scripts and the light client's worker come only from this origin; `'wasm-unsafe-eval'` lets smoldot
//   compile its WebAssembly, and allows no `eval` of JavaScript.
// - `connect-src` is this origin and `wss:`: smoldot talks to Kreivo's and Kusama's peers over secure
//   WebSockets, wherever the peer-to-peer network puts them, so no host list can be fixed in advance. Every
//   EVM read and write goes through the wallet extension, which the page's CSP does not govern, so no EVM RPC
//   host is listed either.
// - The public status route (T-018-10), when a build names one (`BRIDGE_STATUS_URL`), adds its origin to
//   `connect-src`; unset, as by default, nothing is added.
// - Images: this origin and `data:` (EIP-6963 wallet icons are data URIs). No frame, form, object or base.

import type { Deployment } from './src/config/networks';

export function csp(deployment: Deployment, opts: { forMeta?: boolean; statusRoute?: string } = {}): string {
  const connect = ["'self'", 'wss:'];
  if (deployment === 'testbed') connect.push('ws://127.0.0.1:9944', 'ws://localhost:9944');
  // The public status route (T-018-10), when the build names one: its origin alone.
  if (opts.statusRoute) connect.push(new URL(opts.statusRoute).origin);
  const directives: [string, ...string[]][] = [
    ['default-src', "'none'"],
    ['script-src', "'self'", "'wasm-unsafe-eval'"],
    ['worker-src', "'self'"],
    ['connect-src', ...connect],
    ['img-src', "'self'", 'data:'],
    ['style-src', "'self'"],
    ['base-uri', "'none'"],
    ['form-action', "'none'"],
    ['object-src', "'none'"],
  ];
  // A meta element cannot carry frame-ancestors; the response header does.
  if (!opts.forMeta) directives.push(['frame-ancestors', "'none'"]);
  if (deployment !== 'testbed') directives.push(['upgrade-insecure-requests']);
  return directives.map((d) => d.join(' ')).join('; ');
}

/** Cloudflare's `_headers` file for the static assets: every path gets the CSP and the hardening headers. */
export function headersFile(deployment: Deployment, statusRoute = ''): string {
  const h = [
    `Content-Security-Policy: ${csp(deployment, { statusRoute })}`,
    'X-Content-Type-Options: nosniff',
    'X-Frame-Options: DENY',
    'Referrer-Policy: no-referrer',
    'Cross-Origin-Opener-Policy: same-origin',
    'Permissions-Policy: camera=(), microphone=(), geolocation=(), payment=(), usb=(), serial=(), hid=(), bluetooth=()',
    'Strict-Transport-Security: max-age=31536000; includeSubDomains',
  ];
  return ['/*', ...h.map((x) => `  ${x}`), ''].join('\n');
}
