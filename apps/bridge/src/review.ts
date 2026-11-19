// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The locked review (F-018, T-018-14). At the Deposit click the page takes a snapshot of what the user reviewed,
// {recipient, amount, refund}, locks every input until the deposit ends, checks the recipient again on the
// snapshot (never on the live form), deposits the snapshot, and after the receipt compares the `Directed` log
// the chain recorded with the snapshot, failing loudly on any difference. Kept apart from main.ts, which owns
// the DOM, so the tests can drive it.

import type { Address } from 'viem';
import type { Assessment } from './lib/recipient';
import type { ContractFacts, DepositRequest, DepositResult, Progress, Session } from './evm/deposit';
import type { DirectedArrival } from './evm/coordinate';
import { formatAmount } from './lib/amount';
import { encodeSs58 } from './lib/ss58';
import { fromHex, type Hex } from './lib/bytes';

/** What the user reviewed and the deposit sends, frozen at the click. */
export interface Snapshot {
  readonly recipient: { readonly account: Hex; readonly address: string };
  readonly amount: bigint;
  readonly refund: Address;
}

/** Takes the snapshot, the recipient's address encoded again from its account as the canonical SS58. Returns
 * `undefined` when something is missing. */
export function takeSnapshot(
  live: { recipient?: { account: Hex }; amount?: bigint; session?: Pick<Session, 'account'> },
  ss58Prefix: number,
): Snapshot | undefined {
  const { recipient: r, amount, session: s } = live;
  if (!r || amount === undefined || !s) return undefined;
  return Object.freeze({
    recipient: Object.freeze({ account: r.account, address: encodeSs58(fromHex(r.account), ss58Prefix) }),
    amount,
    refund: s.account,
  });
}

/** The controls the lock disables, by id: everything on the page that changes what is deposited. */
export const LOCKED_IDS = ['network', 'recipient', 'amount', 'from-kreivo-wallet'] as const;
/** Containers whose buttons the lock disables too (wallets, Kreivo accounts), whenever they are drawn. */
export const LOCKED_CONTAINERS = ['wallets', 'kreivo-accounts'] as const;

/** Anything the lock disables: inputs, selects and buttons. */
export interface Lockable {
  disabled: boolean;
}

/** Disables every control while a deposit is in flight, and enables them again after. */
export function lockControls(controls: Iterable<Lockable>, locked: boolean): void {
  for (const c of controls) c.disabled = locked;
}

/** The decoded `Directed` log, as the page shows it. */
export interface Recorded {
  recipient: string;
  recipientAccount: Hex;
  amount: bigint;
  refund: Address;
}

export type Comparison =
  | { ok: true; arrival: DirectedArrival; recorded: Recorded }
  | { ok: false; reason: string; recorded: Recorded | undefined };

/** Compares what the chain recorded with the snapshot. The deposit's arrival must be in the receipt, and its
 * recipient, amount and refund must be the snapshot's; any difference is a failure, named. */
export function compareArrival(res: Pick<DepositResult, 'arrival' | 'arrivals' | 'directedLogs'>, snap: Snapshot, ss58Prefix: number): Comparison {
  const recorded = (a: DirectedArrival): Recorded => ({
    recipient: encodeSs58(fromHex(a.recipient), ss58Prefix),
    recipientAccount: a.recipient,
    amount: a.amount,
    refund: a.refund,
  });
  const ofSender = res.arrivals.filter((a) => a.sender === snap.refund);
  const seen = res.arrival ?? ofSender[0] ?? res.arrivals[0];
  if (!seen) {
    return { ok: false, reason: 'The receipt holds no Directed log paired with its Transfer for this contract.', recorded: undefined };
  }
  const r = recorded(seen);
  const diffs: string[] = [];
  if (seen.recipient !== snap.recipient.account) diffs.push(`recipient ${r.recipient}, not ${snap.recipient.address}`);
  if (seen.amount !== snap.amount) diffs.push(`amount ${seen.amount}, not ${snap.amount} (minor units)`);
  if (seen.refund !== snap.refund) diffs.push(`refund ${seen.refund}, not ${snap.refund}`);
  if (seen.sender !== snap.refund) diffs.push(`sender ${seen.sender}, not ${snap.refund}`);
  // A deposit emits one Directed log. More than one, paired or not, is not this deposit alone, whatever the first
  // says: fail loudly rather than pick one.
  const count = Math.max(res.directedLogs, res.arrivals.length);
  if (count > 1) diffs.push(`the receipt holds ${count} Directed logs of this contract, not one`);
  if (diffs.length || !res.arrival) {
    return { ok: false, reason: `The Directed log differs from what you reviewed: ${diffs.join('; ') || 'it is not this deposit'}.`, recorded: r };
  }
  return { ok: true, arrival: res.arrival, recorded: r };
}

export interface RunDeps {
  /** Checks the recipient on Kreivo for exactly this account and amount. */
  recheck: (account: Hex, amount: bigint) => Promise<Assessment | undefined>;
  deposit: (s: Session, c: ContractFacts, req: DepositRequest, progress: Progress) => Promise<DepositResult>;
}

export type Outcome = { res: DepositResult; comparison: Comparison };

/** Runs the deposit of a snapshot: the recipient is checked again on the snapshot's account and amount, the
 * deposit sends the snapshot, and the receipt's `Directed` log is compared with it. Throws before signing
 * when the re-check fails. */
export async function runDeposit(
  snap: Snapshot,
  s: Session,
  c: ContractFacts,
  ss58Prefix: number,
  deps: RunDeps,
  progress: Progress,
): Promise<Outcome> {
  if (s.account !== snap.refund) throw new Error('The wallet’s account changed after the review; nothing was signed.');
  progress('Checking the recipient on Kreivo again.');
  const a = await deps.recheck(snap.recipient.account, snap.amount);
  if (!a?.ok) throw new Error(`The recipient cannot receive the deposit${a && !a.ok ? `: ${a.reason}` : ''}; nothing was signed.`);
  const res = await deps.deposit(s, c, { amount: snap.amount, recipient: snap.recipient.account }, progress);
  return { res, comparison: compareArrival(res, snap, ss58Prefix) };
}

/** The review's rows for a snapshot. */
export function reviewRows(snap: Snapshot, c: Pick<ContractFacts, 'decimals' | 'symbol'>): [string, string][] {
  return [
    ['From', snap.refund],
    ['Refund to', `${snap.refund} (the depositor)`],
    ['Recipient (Kreivo)', snap.recipient.address],
    ['Recipient (32 bytes)', snap.recipient.account],
    ['Amount', `${formatAmount(snap.amount, c.decimals)} ${c.symbol}`],
  ];
}
