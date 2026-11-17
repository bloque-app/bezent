// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The status page (F-018, T-018-17; REQ-ST-1, REQ-ST-8): a directed deposit, by its arrival's coordinate on the
// origin network (the transaction and the `Transfer` log's index), and a withdrawal, by its redemption's on-chain
// reference (the redeemer and the reference), followed from public chain data, each state with its coordinates:
//
// - a deposit: the `Transfer`/`Directed` pair and its block's finality on Polygon; the references derived from the
//   coordinate; whether the contract on Kreivo recorded the mint reference (`mint_reference_used`, which only a mint
//   adds, with its `Minted` event); and whether the reserve returned it, by the return's payout reference;
// - a withdrawal: the redemption as the contract records it (what its `Redeemed` event carried, and whether it was
//   refunded); its bridge reference and payout reference; and the payout on Polygon carrying that reference, checked
//   against the redemption's address and amount.
//
// The public status route (T-018-10), when the build names one, is shown beside them as bezent's own report, never as
// an outcome. Reads go through the connected EVM wallet and Kreivo's light client.

import type { Address } from 'viem';
import { getAddress } from 'viem';
import type { ResolvedNetwork } from '../config/networks';
import { UNSET } from '../config/networks';
import type { Kreivo } from '../kreivo/client';
import { backendText } from '../kreivo/bezent';
import { mintReferenceUsed, readRedemption } from '../kreivo/redeem';
import { assertChain, type Session } from '../evm/deposit';
import { formatAmount } from '../lib/amount';
import { decodeSs58, encodeSs58 } from '../lib/ss58';
import { fromHex, type Hex } from '../lib/bytes';
import { depositReference, mintReference, payoutReference, unassociatedRefundPayoutReference, unassociatedRefundReference, withdrawalReference } from '../lib/refs';
import { checkPayout, depositOnPolygon, findPayout, type Finality, type PayoutSearch } from '../status/polygon';
import { depositByCoordinate, withdrawalByRedemption, type RouteResult } from '../status/route';
import { $, errorText, facts, say } from './dom';

export interface StatusHost {
  network(): ResolvedNetwork | undefined;
  kreivo(): Kreivo | undefined;
  session(): Session | undefined;
  route: string | null;
}

/** The link to a status, as a fragment of this page (nothing leaves the browser). */
export const depositLink = (tx: Hex, log: number) => `#status=deposit&tx=${tx}&log=${log}`;
export const withdrawalLink = (redeemer: string, reference: Hex) => `#status=withdrawal&redeemer=${redeemer}&reference=${reference}`;

const HASH = /^0x[0-9a-fA-F]{64}$/;

const finalText = (f: Finality) =>
  !f.canonical
    ? `block ${f.blockNumber} (${f.blockHash}) is no longer on the chain: reorganised away`
    : f.final
      ? `final (block ${f.blockNumber} ≤ finalized head ${f.finalized})`
      : `included, not final yet (block ${f.blockNumber}; finalized head ${f.finalized})`;

function routeRows(r: RouteResult): [string, string][] {
  switch (r.kind) {
    case 'unset':
      return [['bezent’s status route', 'not set in this build: the state above is from chain data alone']];
    case 'not-found':
      return [['bezent reports', 'not found']];
    case 'error':
      return [['bezent’s status route', `unavailable (${r.reason})`]];
    case 'answer': {
      const a = r.answer;
      const rows: [string, string][] = [
        ['bezent reports (not checked by this page)', a.state],
        ['Conditions', a.conditions.length ? a.conditions.join(', ') : 'none'],
      ];
      for (const c of a.coordinates) {
        const where = [c.tx_hash, c.block_number !== undefined ? `block ${c.block_number}` : '', c.block_hash, c.position_kind ? `${c.position_kind} ${c.position_index ?? ''}` : '']
          .filter(Boolean)
          .join(' · ');
        rows.push([`${c.role ?? 'Coordinate'} (${c.network})`, `${where}${c.final === undefined ? '' : c.final ? ', final' : ', not final'}`]);
      }
      if (a.updated_at) rows.push(['Reported at', a.updated_at]);
      return rows;
    }
  }
}

function searchRows(label: string, s: PayoutSearch, check?: { to: Address; amount: bigint }): { rows: [string, string][]; paid: 'yes' | 'no' | 'bad' } {
  if (!s.found) {
    const where = `blocks ${s.searched.from} to ${s.searched.to}`;
    return { rows: [[label, s.complete ? `none found in ${where}` : `none found in ${where}; older blocks were not searched`]], paid: 'no' };
  }
  const p = s.found;
  const rows: [string, string][] = [[label, `${p.txHash}, Reference log ${p.referenceLog}; ${finalText(p.finality)}`]];
  for (const t of p.transfers) rows.push(['Paid', `${t.amount} (minor units) to ${t.to}, Transfer log ${t.logIndex}`]);
  if (!check) return { rows, paid: 'yes' };
  const c = checkPayout(p, check.to, check.amount);
  if (!c.ok) rows.push(['Check', c.reason]);
  return { rows, paid: c.ok ? 'yes' : 'bad' };
}

export function initStatus(host: StatusHost) {
  const out = $('status-result');
  const note = $('status-note');

  function polygon(): { n: ResolvedNetwork; s: Session } | string {
    const n = host.network();
    if (!n) return 'Pick a network the app does not refuse first.';
    const s = host.session();
    if (!s || s.network !== n) return `Connect an EVM wallet on ${n.label}: the page reads Polygon through it.`;
    return { n, s };
  }

  async function deposit(tx: string, logText: string) {
    const rows: [string, string][] = [];
    const show = () => facts(out, rows);
    if (!HASH.test(tx.trim())) return say(note, 'A transaction hash is 0x and 64 hex digits.', 'error');
    const log = Number(logText);
    if (!Number.isInteger(log) || log < 0 || log > 0xffff_ffff) return say(note, 'The log index is the Transfer log’s index in its block: a whole number.', 'error');
    const p = polygon();
    if (typeof p === 'string') return say(note, p, 'error');
    const { n, s } = p;
    say(note, `Reading ${n.label}…`);
    out.replaceChildren();
    try {
      await assertChain(s);
      const d = await depositOnPolygon(s.public, n, tx.trim().toLowerCase() as Hex, log);
      if (!d.found) {
        rows.push(['State', 'not a directed deposit'], ['Why', d.reason]);
        return show();
      }
      const a = d.arrival;
      const c = a.coordinate;
      const prefix = n.kreivo.ss58Prefix;
      rows.push(
        ['State', ''],
        ['Arrival (coordinate)', `${c.network} · block ${c.blockNumber} (${c.blockHash}) · log ${c.positionIndex}`],
        ['Polygon', finalText(d.finality)],
        ['Transfer → Directed', `${d.txHash}: Transfer log ${c.positionIndex}, Directed log ${d.directedLog}`],
        ['Sender', a.sender],
        ['Recipient (Kreivo)', encodeSs58(fromHex(a.recipient), prefix)],
        ['Amount', `${a.amount} (minor units)`],
        ['Refund to', a.refund],
        ['Bridge reference', depositReference(c)],
        ['Mint reference', mintReference(c)],
      );
      if (d.others) rows.push(['Note', `the transaction holds ${d.others} other directed arrival(s); each has its own coordinate`]);
      show();
      let state = d.finality.canonical ? (d.finality.final ? 'final on Polygon, not minted yet' : 'on Polygon, not final yet') : 'reorganised away on Polygon';
      // Kreivo: did the contract record the mint reference?
      const k = host.kreivo();
      const contract = n.kreivo.bezent;
      if (!k || contract === UNSET) rows.push(['Kreivo', !k ? `${n.kreivo.label} is not connected` : 'the bezent contract is not set in this build']);
      else {
        try {
          const minted = await mintReferenceUsed(k, n.kreivo, contract as Hex, mintReference(c));
          rows.push(['Kreivo', minted ? 'minted: the contract recorded this mint reference (its Minted event carries it)' : 'not minted: the contract has not recorded this mint reference']);
          if (minted) state = 'minted on Kreivo';
        } catch (e) {
          rows.push(['Kreivo', `could not be read: ${errorText(e)}`]);
        }
      }
      show();
      // A deposit that can never be minted is returned to its refund address, with the return's payout reference.
      if (n.emitter === UNSET || n.payoutsFrom === UNSET) rows.push(['Return', 'the vault’s reference emitter is not set in this build, so returns are not searched']);
      else {
        const ref = unassociatedRefundPayoutReference(c);
        rows.push(['Return reference', `${unassociatedRefundReference(c)}, payout reference ${ref}`]);
        try {
          const from = c.blockNumber > (n.payoutsFrom as bigint) ? c.blockNumber : (n.payoutsFrom as bigint);
          const sr = searchRows('Returned', await findPayout(s.public, n, n.emitter as Address, from, ref), { to: a.refund, amount: a.amount });
          rows.push(...sr.rows);
          if (sr.paid !== 'no') state = sr.paid === 'yes' ? 'returned to the refund address' : 'a return carries its reference but differs (see the check)';
        } catch (e) {
          rows.push(['Returned', `could not be searched: ${errorText(e)}`]);
        }
      }
      rows[0] = ['State (from chain data)', state];
      rows.push(...routeRows(await depositByCoordinate(host.route, c)));
      show();
      say(note, '');
    } catch (e) {
      if (rows.length) {
        rows[0] = ['State (from chain data)', 'not established: a read failed'];
        show();
      }
      say(note, errorText(e), 'error');
    }
  }

  async function withdrawal(redeemerText: string, referenceText: string) {
    const rows: [string, string][] = [];
    const show = () => facts(out, rows);
    const n = host.network();
    if (!n) return say(note, 'Pick a network the app does not refuse first.', 'error');
    const d = decodeSs58(redeemerText.trim());
    if (!d || d.prefix !== n.kreivo.ss58Prefix) return say(note, `The redeemer is a Kreivo address (SS58, prefix ${n.kreivo.ss58Prefix}).`, 'error');
    if (!HASH.test(referenceText.trim())) return say(note, 'The reference is 0x and 64 hex digits, as the redemption chose it.', 'error');
    const reference = referenceText.trim().toLowerCase() as Hex;
    const redeemer = encodeSs58(fromHex(d.account), n.kreivo.ss58Prefix);
    const bridge = withdrawalReference(d.account, reference);
    const payoutRef = payoutReference(bridge);
    out.replaceChildren();
    rows.push(['State', ''], ['On-chain reference', `${redeemer} + ${reference}`], ['Bridge reference', bridge], ['Payout reference', payoutRef]);
    show();
    const k = host.kreivo();
    if (!k || n.kreivo.bezent === UNSET) {
      say(note, !k ? `${n.kreivo.label} is not connected yet.` : 'The bezent contract is not set in this build.', 'error');
      return;
    }
    say(note, `Reading ${n.kreivo.label}…`);
    let state = 'no such redemption';
    try {
      const red = await readRedemption(k, n.kreivo, n.kreivo.bezent as Hex, d.account, reference);
      if (!red) {
        rows.push(['Kreivo', 'the contract records no redemption with this redeemer and reference']);
      } else {
        const { symbol, decimals } = n.kreivo.asset;
        const to = red.payout.kind === 'address' ? getAddress(red.payout.address) : undefined;
        rows.push(
          ['Kreivo', `redeemed: the contract records it (its Redeemed event carried the same), final`],
          ['Amount', `${formatAmount(red.amount, decimals)} ${symbol} (Here(${red.asset}))`],
          ['Backend', backendText(red.backend)],
          ['Paid to', to ?? `commitment ${red.payout.kind === 'commitment' ? red.payout.commitment : ''}`],
          ['Refunded', red.refunded ? 'yes: a refund mint returned it on Kreivo, after its payout was proven not executed' : 'no'],
        );
        state = red.refunded ? 'refunded on Kreivo' : 'redeemed on Kreivo; payout not found yet';
        show();
        const p = polygon();
        if (red.backend.kind !== 'evm' || red.backend.chainId !== BigInt(n.chainId) || !to) rows.push(['Payout', `on ${backendText(red.backend)}, which this page does not read`]);
        else if (typeof p === 'string') rows.push(['Payout', p]);
        else if (n.emitter === UNSET || n.payoutsFrom === UNSET) rows.push(['Payout', 'the vault’s reference emitter is not set in this build, so payouts are not searched']);
        else {
          try {
            await assertChain(p.s);
            const sr = searchRows('Payout', await findPayout(p.s.public, n, n.emitter as Address, n.payoutsFrom as bigint, payoutRef), { to, amount: red.amount });
            rows.push(...sr.rows);
            if (sr.paid === 'yes') {
              // A refund mint follows only a payout proven not executed: both at once is an incident, not a state.
              state = red.refunded ? 'refunded on Kreivo, and a payout carries its reference: both should never happen; report it' : 'paid out on Polygon';
            }
            if (sr.paid === 'bad') state = 'a payout carries its reference but differs (see the check)';
          } catch (e) {
            rows.push(['Payout', `could not be searched: ${errorText(e)}`]);
          }
        }
      }
      rows[0] = ['State (from chain data)', state];
      rows.push(...routeRows(await withdrawalByRedemption(host.route, redeemer, reference)));
      show();
      say(note, '');
    } catch (e) {
      rows[0] = ['State (from chain data)', 'not established: a read failed'];
      show();
      say(note, errorText(e), 'error');
    }
  }

  $('status-deposit').addEventListener('click', () => void deposit($<HTMLInputElement>('status-tx').value, $<HTMLInputElement>('status-log').value));
  $('status-withdrawal').addEventListener('click', () =>
    void withdrawal($<HTMLInputElement>('status-redeemer').value, $<HTMLInputElement>('status-reference').value),
  );

  /** Follows a status link (`#status=…`): fills the form and looks it up. */
  function follow(hash: string) {
    const q = new URLSearchParams(hash.replace(/^#/, ''));
    const what = q.get('status');
    if (what === 'deposit') {
      $<HTMLInputElement>('status-tx').value = q.get('tx') ?? '';
      $<HTMLInputElement>('status-log').value = q.get('log') ?? '';
      void deposit(q.get('tx') ?? '', q.get('log') ?? '');
    } else if (what === 'withdrawal') {
      $<HTMLInputElement>('status-redeemer').value = q.get('redeemer') ?? '';
      $<HTMLInputElement>('status-reference').value = q.get('reference') ?? '';
      void withdrawal(q.get('redeemer') ?? '', q.get('reference') ?? '');
    } else return;
    $('status').scrollIntoView?.();
  }

  return { deposit, withdrawal, follow };
}
