// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Kreivo's signed extensions, and the bridge between polkadot-api and a Kreivo browser-extension wallet (Talisman,
// SubWallet, Polkadot.js) that signs them (F-018, T-018-15; amendment 0029, Open 9; plan part 4, "Redeem").
//
// Kreivo's runtime (spec 136, transaction version 14; read from its metadata) signs ten extensions, in this order:
//
//   PassAuthenticate, CheckNonZeroSender, CheckSpecVersion, CheckTxVersion, CheckGenesis, CheckMortality,
//   CheckNonce, CheckWeight, ChargeAssetTxPayment, DynamicMaxBlockWeight
//
// Two of them are Kreivo's own and unknown to polkadot.js, whose wallets build the signed payload from a JSON
// description with their own type registry:
//
// - `PassAuthenticate` (Virto's pass pallet): `Option<{ device_id, credential }>`, one byte, `0x00` (None), when the
//   account signs for itself, as every extension wallet's account does. A wallet that does not know it treats it as
//   "no effect" and leaves that byte out of what it signs, so its signature is over another payload and Kreivo
//   rejects the transaction.
// - `DynamicMaxBlockWeight`: a wrapper that adds no data.
//
// So, as the requirement of Open 9 says, the app registers both in the polkadot.js initialisation metadata it gives
// the wallet before anything is signed (`metadata.provide`, the `MetadataDef`'s `userExtensions`): the shape
// polkadot.js's `ApiPromise.create({ signedExtensions })` takes. And polkadot-api's own polkadot.js bridge refuses
// any extension it does not map that carries data (`PJS does not support this signed-extension: PassAuthenticate`),
// so the app signs through its own bridge (`pjsSigner`), which maps the two and refuses anything else it does not
// know: the list is read from the runtime's metadata at every signature, never assumed.
//
// The app never asks the wallet for a signed transaction: it puts the extrinsic together itself from the wallet's
// signature and the bytes it built, so a wallet that signed something else produces an extrinsic Kreivo rejects,
// never one that does something else. And before that, it checks the wallet's signature over the exact signing
// payload against the account's key (Ed25519, Sr25519 or Ecdsa), so a wallet that signed another payload is
// refused in the page, and nothing is broadcast.

import { decAnyMetadata, unifyMetadata } from '@polkadot-api/substrate-bindings';
import { blake2b } from '@noble/hashes/blake2b';
import { ed25519 } from '@noble/curves/ed25519';
import { secp256k1 } from '@noble/curves/secp256k1';
import { verify as sr25519Verify } from '@scure/sr25519';
import type { PolkadotSigner } from 'polkadot-api/signer';
import { concat, fromHex, toHex, type Hex } from '../lib/bytes';
import { compact, Reader } from '../lib/scale';

/** polkadot.js's `ExtDef`: per extension, the fields its explicit (`extrinsic`) and implicit (`payload`) data add
 * to the payload, as type names polkadot.js's registry resolves. */
export type ExtDef = Record<string, { extrinsic: Record<string, string>; payload: Record<string, string> }>;

/**
 * Kreivo's custom signed extensions, registered with the wallet. The app only ever signs `PassAuthenticate` as
 * `None`, and the JSON it gives the wallet never carries the field, so the wallet encodes it as `None` too: the
 * option's inner type is never encoded, and `u8` stands in for the pass-authentication struct, which polkadot.js
 * does not know by name.
 */
export const KREIVO_USER_EXTENSIONS: ExtDef = {
  PassAuthenticate: { extrinsic: { passAuthenticate: 'Option<u8>' }, payload: {} },
  DynamicMaxBlockWeight: { extrinsic: {}, payload: {} },
};

/** The bytes the app signs for each custom extension: `PassAuthenticate` is `None` (the account signs for itself)
 * and adds nothing to the implicit data; `DynamicMaxBlockWeight` adds nothing at all. Passed to polkadot-api as its
 * `customSignedExtensions`, so it never infers them. */
export const KREIVO_CUSTOM_VALUES = {
  PassAuthenticate: { value: Uint8Array.of(0), additionalSigned: new Uint8Array(0) },
  DynamicMaxBlockWeight: { value: new Uint8Array(0), additionalSigned: new Uint8Array(0) },
} as const;

/** The extensions polkadot.js knows by itself, and the JSON field each one fills from polkadot-api's bytes. */
type Mapper = (e: { value: Uint8Array; additionalSigned: Uint8Array }, atBlockNumber: number) => Record<string, string | number>;

/** Hex as polkadot.js's JSON payload writes integers: big-endian, at least `bytes` long. */
const pjsHex = (v: bigint, bytes: number) => {
  let h = v.toString(16);
  if (h.length % 2) h = `0${h}`;
  return `0x${'00'.repeat(Math.max(0, bytes - h.length / 2))}${h}`;
};
const readU32 = (b: Uint8Array) => {
  const r = new Reader(b);
  const v = r.u32();
  r.end('u32');
  return BigInt(v);
};
const readCompact = (b: Uint8Array) => {
  const r = new Reader(b);
  const v = r.compact();
  r.end('compact');
  return v;
};
const empty = (e: { value: Uint8Array; additionalSigned: Uint8Array }) => e.value.length === 0 && e.additionalSigned.length === 0;

const PJS_KNOWN: Record<string, Mapper> = {
  CheckNonZeroSender: () => ({}),
  CheckWeight: () => ({}),
  CheckSpecVersion: (e) => ({ specVersion: pjsHex(readU32(e.additionalSigned), 4) }),
  CheckTxVersion: (e) => ({ transactionVersion: pjsHex(readU32(e.additionalSigned), 4) }),
  CheckGenesis: (e) => ({ genesisHash: toHex(e.additionalSigned) }),
  CheckMortality: (e, at) => ({ era: toHex(e.value), blockHash: toHex(e.additionalSigned), blockNumber: pjsHex(BigInt(at), 4) }),
  CheckNonce: (e) => ({ nonce: pjsHex(readCompact(e.value), 4) }),
  ChargeAssetTxPayment: (e) => {
    // `{ tip: Compact<u128>, asset_id: Option<…> }`: the app never pays fees in an asset, so the option is None.
    const r = new Reader(e.value);
    const tip = r.compact();
    if (r.u8() !== 0) throw new Error('The app pays Kreivo’s fees in KSM only; refusing a fee asset.');
    r.end('ChargeAssetTxPayment');
    return { tip: pjsHex(tip, 16) };
  },
  ChargeTransactionPayment: (e) => ({ tip: pjsHex(readCompact(e.value), 16) }),
};

/** The custom extensions, each with the exact bytes the app signs; any other value is refused. */
const CUSTOM: Record<string, { value: Uint8Array; additionalSigned: Uint8Array }> = KREIVO_CUSTOM_VALUES;

const equal = (a: Uint8Array, b: Uint8Array) => a.length === b.length && a.every((x, i) => x === b[i]);

/** The signed extensions a runtime's metadata names, in order, and its extrinsic versions. */
export function metadataExtensions(metadata: Uint8Array): { identifiers: string[]; versions: number[] } {
  const { extrinsic } = unifyMetadata(decAnyMetadata(metadata));
  // Extension version 0: the only one extrinsic version 4 signs.
  const list = extrinsic.signedExtensions[0] ?? [];
  return { identifiers: list.map((e) => e.identifier), versions: extrinsic.version };
}

/** Checks a runtime's signed extensions before anything is signed: every one is known to polkadot.js or registered
 * here. Throws naming the first that is neither: the app would not know how the wallet signs it. */
export function checkExtensions(identifiers: readonly string[]): void {
  for (const id of identifiers) {
    if (!PJS_KNOWN[id] && !CUSTOM[id]) {
      throw new Error(`Kreivo’s runtime signs an extension this app does not register (${id}); nothing is signed. The app must be updated for this runtime.`);
    }
  }
  if (!identifiers.includes('PassAuthenticate')) {
    // Not an error in itself, but the app was written against a runtime that has it: say so rather than guess.
    throw new Error('Kreivo’s runtime no longer names PassAuthenticate among its signed extensions; the app was built for one that does, so nothing is signed.');
  }
}

/** polkadot.js's `SignerPayloadJSON`, as the wallets take it. */
export interface SignerPayloadJSON {
  address: string;
  blockHash: string;
  blockNumber: string;
  era: string;
  genesisHash: string;
  method: string;
  nonce: string;
  specVersion: string;
  tip: string;
  transactionVersion: string;
  signedExtensions: string[];
  version: number;
  [field: string]: unknown;
}

/** The polkadot.js JSON payload for these signed extensions, which the wallet signs, the explicit data (the
 * extrinsic's `extra`) and the implicit data (`additional`), each in the runtime's order. Refuses an extension it
 * cannot describe, a custom one whose bytes are not the ones registered, and a runtime without extrinsic
 * version 4. */
export function pjsPayload(
  address: string,
  callData: Uint8Array,
  signed: Record<string, { value: Uint8Array; additionalSigned: Uint8Array }>,
  metadata: Uint8Array,
  atBlockNumber: number,
): { payload: SignerPayloadJSON; extra: Uint8Array[]; additional: Uint8Array[] } {
  const { identifiers, versions } = metadataExtensions(metadata);
  if (!versions.includes(4)) throw new Error('Kreivo’s runtime does not take extrinsic version 4; the wallets sign only that.');
  checkExtensions(identifiers);
  const fields: Record<string, unknown> = {};
  const extra: Uint8Array[] = [];
  const additional: Uint8Array[] = [];
  for (const id of identifiers) {
    const e = signed[id];
    if (!e) throw new Error(`The transaction carries no ${id}; nothing is signed.`);
    extra.push(e.value);
    additional.push(e.additionalSigned);
    const custom = CUSTOM[id];
    if (custom) {
      if (!equal(e.value, custom.value) || !equal(e.additionalSigned, custom.additionalSigned)) {
        throw new Error(`${id} is not the value this app registers with the wallet (${toHex(e.value)}); nothing is signed.`);
      }
      continue;
    }
    const map = PJS_KNOWN[id]!;
    if (id === 'CheckNonZeroSender' || id === 'CheckWeight') {
      if (!empty(e)) throw new Error(`${id} carries data; nothing is signed.`);
    }
    Object.assign(fields, map(e, atBlockNumber));
  }
  const payload = {
    ...fields,
    address,
    method: toHex(callData),
    signedExtensions: identifiers,
    version: 4,
  } as SignerPayloadJSON;
  return { payload, extra, additional };
}

const blake2_256 = (b: Uint8Array) => blake2b(b, { dkLen: 32 });

/** The bytes Kreivo verifies a v4 signature over: the call, every extension's explicit data, then every one's
 * implicit data, in the runtime's order; over 256 bytes, their BLAKE2b-256. */
export function signingPayload(callData: Uint8Array, extra: readonly Uint8Array[], additional: readonly Uint8Array[]): Uint8Array {
  const bytes = concat([callData, ...extra, ...additional]);
  return bytes.length > 256 ? blake2_256(bytes) : bytes;
}

/** Whether a `MultiSignature` (its kind byte, then the signature) signs `message` for the 32-byte account
 * `account`, as Kreivo checks it: Ed25519 and Sr25519 over the message, by the account's public key; Ecdsa over the
 * message's BLAKE2b-256, by the key it recovers, whose compressed form hashes (BLAKE2b-256) to the account. */
export function verifyMultiSignature(signature: Uint8Array, message: Uint8Array, account: Uint8Array): boolean {
  const body = signature.subarray(1);
  try {
    switch (signature[0]) {
      case 0:
        return body.length === 64 && ed25519.verify(body, message, account);
      case 1:
        return body.length === 64 && sr25519Verify(message, body, account);
      case 2: {
        if (body.length !== 65) return false;
        const v = body[64]!;
        const recovery = v >= 27 ? v - 27 : v;
        if (recovery > 3) return false;
        const key = secp256k1.Signature.fromCompact(body.subarray(0, 64)).addRecoveryBit(recovery).recoverPublicKey(blake2_256(message));
        return equal(blake2_256(key.toRawBytes(true)), account);
      }
      default:
        return false;
    }
  } catch {
    return false;
  }
}

/** The wallet signed another payload than the one the page built, or Kreivo said so (BadProof): most likely the
 * wallet does not know Kreivo's custom signed extensions. Nothing was broadcast. */
export class WalletSignatureMismatch extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'WalletSignatureMismatch';
  }
}

/** A signed v4 extrinsic: `compact(len) ‖ 0x84 ‖ MultiAddress::Id(account) ‖ MultiSignature ‖ extra ‖ call`. */
export function signedExtrinsic(publicKey: Uint8Array, signature: Uint8Array, extra: Uint8Array[], callData: Uint8Array): Uint8Array {
  if (publicKey.length !== 32) throw new Error('A Kreivo account is 32 bytes.');
  const kind = signature[0];
  const length = kind === 2 ? 66 : 65;
  if (kind === undefined || kind > 2 || signature.length !== length) {
    throw new Error('The wallet’s signature is not a MultiSignature (Ed25519, Sr25519 or Ecdsa); nothing is sent.');
  }
  const body = concat([Uint8Array.of(0x84, 0x00), publicKey, signature, ...extra, callData]);
  return concat([compact(BigInt(body.length)), body]);
}

/** What the wallet's injected `signer.signPayload` returns. */
export type SignPayload = (payload: SignerPayloadJSON) => Promise<{ signature: string }>;

/** A polkadot-api signer for one account of an extension wallet: it hands the wallet polkadot.js's JSON payload,
 * with Kreivo's custom extensions mapped, checks the signature it returns over the exact signing payload against
 * the account's key, and puts the extrinsic together from it. A signature that does not verify is refused
 * (`WalletSignatureMismatch`) and nothing is broadcast. `signBytes` is not offered: the app signs nothing but
 * transactions. */
export function pjsSigner(address: string, publicKey: Uint8Array, signPayload: SignPayload): PolkadotSigner {
  return {
    publicKey,
    signTx: async (callData, signed, metadata, atBlockNumber) => {
      const { payload, extra, additional } = pjsPayload(address, callData, signed, metadata, atBlockNumber);
      const { signature } = await signPayload(payload);
      const sig = fromHex(signature as Hex);
      const extrinsic = signedExtrinsic(publicKey, sig, extra, callData);
      if (!verifyMultiSignature(sig, signingPayload(callData, extra, additional), publicKey)) {
        throw new WalletSignatureMismatch(
          'The wallet’s signature does not verify against your account’s key over the transaction the page built: the wallet signed another payload, most likely because it does not know Kreivo’s PassAuthenticate signed extension. Offer the wallet Kreivo’s metadata again (the button above), accept it, then try again. Nothing was sent.',
        );
      }
      return extrinsic;
    },
    signBytes: () => Promise.reject(new Error('The bridge signs nothing but transactions.')),
  };
}

/** polkadot.js's `MetadataDef`: what a page gives a wallet about a chain (`injected.metadata.provide`). */
export interface MetadataDef {
  chain: string;
  genesisHash: string;
  icon: string;
  ss58Format: number;
  chainType: 'substrate';
  specVersion: number;
  tokenDecimals: number;
  tokenSymbol: string;
  types: Record<string, string>;
  userExtensions: ExtDef;
  /** The runtime's metadata (V15), so the wallet can show the call it signs. */
  rawMetadata?: string;
}

/** The initialisation metadata for Kreivo, with its custom signed extensions registered. */
export function kreivoMetadataDef(o: {
  label: string;
  genesisHash: Hex;
  ss58Format: number;
  specVersion: number;
  tokenDecimals: number;
  tokenSymbol: string;
  rawMetadata?: Uint8Array;
}): MetadataDef {
  return {
    chain: o.label,
    genesisHash: o.genesisHash,
    icon: 'substrate',
    ss58Format: o.ss58Format,
    chainType: 'substrate',
    specVersion: o.specVersion,
    tokenDecimals: o.tokenDecimals,
    tokenSymbol: o.tokenSymbol,
    types: {},
    userExtensions: KREIVO_USER_EXTENSIONS,
    ...(o.rawMetadata ? { rawMetadata: toHex(o.rawMetadata) } : {}),
  };
}
