// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// T-018-12: the CSP allows no third-party script, no inline script and no eval, and only the test bed's build
// may reach a loopback node.

import { describe, expect, it } from 'vitest';
import { csp, headersFile } from '../csp';

const directive = (policy: string, name: string) =>
  policy.split('; ').find((d) => d === name || d.startsWith(`${name} `));

describe('csp', () => {
  for (const d of ['test', 'production'] as const) {
    it(`is strict in the ${d} build`, () => {
      const p = csp(d);
      expect(directive(p, 'default-src')).toBe("default-src 'none'");
      expect(directive(p, 'script-src')).toBe("script-src 'self' 'wasm-unsafe-eval'");
      expect(directive(p, 'connect-src')).toBe("connect-src 'self' wss:");
      expect(directive(p, 'frame-ancestors')).toBe("frame-ancestors 'none'");
      expect(p).not.toMatch(/unsafe-inline|'unsafe-eval'|https?:|\*/);
    });
  }

  it('lets the test bed build reach the local collator only', () => {
    expect(directive(csp('testbed'), 'connect-src')).toBe("connect-src 'self' wss: ws://127.0.0.1:9944 ws://localhost:9944");
  });

  it('leaves frame-ancestors out of the meta element, where browsers ignore it', () => {
    expect(directive(csp('production', { forMeta: true }), 'frame-ancestors')).toBeUndefined();
  });

  it('applies the headers to every path', () => {
    const h = headersFile('production');
    expect(h.startsWith('/*\n')).toBe(true);
    expect(h).toContain(`Content-Security-Policy: ${csp('production')}`);
    expect(h).toContain('X-Frame-Options: DENY');
  });
});
