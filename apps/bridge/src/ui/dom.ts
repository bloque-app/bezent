// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The page's few DOM helpers, shared by the deposit (main.ts), the redeem (ui/redeem.ts) and the status page
// (ui/status.ts). Text only: nothing the chain or a wallet says is ever written as HTML.

export const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

/** Fills a `<dl>` with label/value rows. */
export function facts(dl: HTMLElement, rows: [string, string][]) {
  dl.replaceChildren(
    ...rows.flatMap(([k, v]) => {
      const dt = document.createElement('dt');
      dt.textContent = k;
      const dd = document.createElement('dd');
      dd.textContent = v;
      return [dt, dd];
    }),
  );
}

export type Kind = 'muted' | 'error' | 'ok';

export function say(el: HTMLElement, text: string, kind: Kind = 'muted') {
  el.textContent = text;
  el.className = kind;
}

/** A paragraph of muted text, as a list's placeholder. */
export function note(text: string): HTMLElement {
  const p = document.createElement('p');
  p.className = 'muted';
  p.textContent = text;
  return p;
}

export const errorText = (e: unknown) => (e as { shortMessage?: string }).shortMessage ?? (e instanceof Error ? e.message : String(e));

/** A progress list: each step is a list item, its kind shown by its class. */
export function progressList(list: HTMLElement) {
  list.replaceChildren();
  return (t: string, kind: Kind = 'muted') => {
    const li = document.createElement('li');
    say(li, t, kind);
    list.append(li);
  };
}
