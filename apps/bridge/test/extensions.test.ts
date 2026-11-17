// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// T-018-15 (amendment 0029, Open 9): Kreivo's custom signed extensions, registered in the polkadot.js initialisation
// metadata the page gives the wallet, so Talisman, SubWallet and Polkadot.js can sign Kreivo's extrinsics.
//
// The runtime's extension list is read from Kreivo's own metadata (spec 136, the endpoint's fixture). A model of
// how a polkadot.js wallet builds the payload it signs (its registry: the extensions it knows, plus the page's
// `userExtensions`, anything else "no effect") signs the JSON the bridge hands it. With Kreivo's extensions
// registered, the extrinsic is byte for byte the one polkadot-api's own signer builds with the same key; without
// them, the wallet signs a payload one byte short (no `PassAuthenticate`), whose signature Kreivo would reject, and
// the page refuses it before anything is broadcast: it checks every signature over the exact signing payload against
// the account's key (Ed25519, Sr25519, Ecdsa). And polkadot-api's own polkadot.js bridge refuses to sign at all.

import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { ed25519 } from '@noble/curves/ed25519';
import { secp256k1 } from '@noble/curves/secp256k1';
import * as sr25519 from '@scure/sr25519';
import { getPolkadotSigner } from 'polkadot-api/signer';
import { getPolkadotSignerFromPjs } from 'polkadot-api/pjs-signer';
import {
  KREIVO_CUSTOM_VALUES,
  KREIVO_USER_EXTENSIONS,
  checkExtensions,
  kreivoMetadataDef,
  metadataExtensions,
  pjsPayload,
  pjsSigner,
  signedExtrinsic,
  signingPayload,
  verifyMultiSignature,
  WalletSignatureMismatch,
  type ExtDef,
  type SignerPayloadJSON,
} from '../src/kreivo/extensions';
import { blake2_256 } from '../src/kreivo/bezent';
import { compact, Reader, u32 } from '../src/lib/scale';
import { concat, fromHex, toHex, type Hex } from '../src/lib/bytes';
import { encodeSs58 } from '../src/lib/ss58';

const METADATA = new Uint8Array(readFileSync(new URL('../../../services/kreivo-endpoint/fixtures/kreivo-spec-136.metadata', import.meta.url)));
const GENESIS: Hex = '0xc710a5f16adc17bcd212cff0aedcbf1c1212a043cdc0fb2dcba861efe5305b01';
const KREIVO_EXTENSIONS = [
  'PassAuthenticate',
  'CheckNonZeroSender',
  'CheckSpecVersion',
  'CheckTxVersion',
  'CheckGenesis',
  'CheckMortality',
  'CheckNonce',
  'CheckWeight',
  'ChargeAssetTxPayment',
  'DynamicMaxBlockWeight',
];

// A published test key (RFC 8032's first test vector's secret): a test value.
const SECRET = fromHex('0x9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60');
const PUBLIC = ed25519.getPublicKey(SECRET);
const ADDRESS = encodeSs58(PUBLIC, 2);

const empty = new Uint8Array(0);
const BLOCK_HASH: Hex = `0x${'5b'.repeat(32)}`;
/** The signed extensions as polkadot-api builds them for Kreivo: nonce 5, mortal from block 41 026 407, no tip. */
const signed = (over: Partial<Record<string, { value: Uint8Array; additionalSigned: Uint8Array }>> = {}) => {
  const base: Record<string, { value: Uint8Array; additionalSigned: Uint8Array }> = {
    PassAuthenticate: KREIVO_CUSTOM_VALUES.PassAuthenticate,
    CheckNonZeroSender: { value: empty, additionalSigned: empty },
    CheckSpecVersion: { value: empty, additionalSigned: u32(136) },
    CheckTxVersion: { value: empty, additionalSigned: u32(14) },
    CheckGenesis: { value: empty, additionalSigned: fromHex(GENESIS) },
    CheckMortality: { value: fromHex('0x7500'), additionalSigned: fromHex(BLOCK_HASH) },
    CheckNonce: { value: compact(5n), additionalSigned: empty },
    CheckWeight: { value: empty, additionalSigned: empty },
    ChargeAssetTxPayment: { value: concat([compact(0n), Uint8Array.of(0)]), additionalSigned: empty },
    DynamicMaxBlockWeight: KREIVO_CUSTOM_VALUES.DynamicMaxBlockWeight,
    ...over,
  };
  return Object.fromEntries(Object.entries(base).map(([identifier, v]) => [identifier, { identifier, ...v! }]));
};
/** A Utility.batch_all call's bytes, long enough that the payload is signed as its hash. */
const CALL = fromHex(`0x2b0208${'00'.repeat(300)}`);

/** How a polkadot.js wallet builds the payload it signs from the page's JSON (`ExtrinsicPayload`): the method, then
 * every extension's explicit fields, then every extension's implicit ones, in the payload's order; an extension
 * neither it nor the page's `userExtensions` defines adds nothing ("no effect"). Over 256 bytes, its BLAKE2b-256. */
function pjsWallet(userExtensions: ExtDef, sign: (message: Uint8Array) => Uint8Array = (m) => concat([Uint8Array.of(0), ed25519.sign(m, SECRET)])) {
  const known: Record<string, { extrinsic: (p: SignerPayloadJSON) => Uint8Array[]; payload: (p: SignerPayloadJSON) => Uint8Array[] }> = {
    CheckNonZeroSender: { extrinsic: () => [], payload: () => [] },
    CheckWeight: { extrinsic: () => [], payload: () => [] },
    CheckSpecVersion: { extrinsic: () => [], payload: (p) => [u32(Number(p.specVersion))] },
    CheckTxVersion: { extrinsic: () => [], payload: (p) => [u32(Number(p.transactionVersion))] },
    CheckGenesis: { extrinsic: () => [], payload: (p) => [fromHex(p.genesisHash)] },
    CheckMortality: { extrinsic: (p) => [fromHex(p.era)], payload: (p) => [fromHex(p.blockHash)] },
    CheckNonce: { extrinsic: (p) => [compact(BigInt(p.nonce))], payload: () => [] },
    ChargeAssetTxPayment: { extrinsic: (p) => [compact(BigInt(p.tip)), p.assetId === undefined ? Uint8Array.of(0) : fromHex(p.assetId as Hex)], payload: () => [] },
  };
  const user = (id: string, part: 'extrinsic' | 'payload') => (p: SignerPayloadJSON) =>
    Object.entries(userExtensions[id]![part]).map(([field, type]) => {
      if (!/^Option</.test(type)) throw new Error(`the model knows Option fields only, not ${type}`);
      return p[field] === undefined ? Uint8Array.of(0) : concat([Uint8Array.of(1), fromHex(p[field] as Hex)]);
    });
  const rules = (id: string) => (userExtensions[id] ? { extrinsic: user(id, 'extrinsic'), payload: user(id, 'payload') } : known[id]);
  return async (p: SignerPayloadJSON) => {
    const explicit = p.signedExtensions.flatMap((id) => rules(id)?.extrinsic(p) ?? []);
    const implicit = p.signedExtensions.flatMap((id) => rules(id)?.payload(p) ?? []);
    const bytes = concat([fromHex(p.method), ...explicit, ...implicit]);
    const message = bytes.length > 256 ? blake2_256(bytes) : bytes;
    return { signature: toHex(sign(message)) };
  };
}

/** The payload Kreivo verifies: the call, the explicit data, the implicit data; hashed over 256 bytes. */
function kreivoPayload(s: ReturnType<typeof signed>) {
  const order = KREIVO_EXTENSIONS.map((id) => s[id]!);
  const bytes = concat([CALL, ...order.map((e) => e.value), ...order.map((e) => e.additionalSigned)]);
  return bytes.length > 256 ? blake2_256(bytes) : bytes;
}

describe('Kreivo’s signed extensions', () => {
  it('are read from the runtime’s metadata, in its order', () => {
    expect(metadataExtensions(METADATA)).toEqual({ identifiers: KREIVO_EXTENSIONS, versions: [4] });
    expect(() => checkExtensions(KREIVO_EXTENSIONS)).not.toThrow();
  });

  it('two of them are custom, and those two are what the app registers with the wallet', () => {
    expect(Object.keys(KREIVO_USER_EXTENSIONS).sort()).toEqual(['DynamicMaxBlockWeight', 'PassAuthenticate']);
    expect(KREIVO_USER_EXTENSIONS.PassAuthenticate).toEqual({ extrinsic: { passAuthenticate: 'Option<u8>' }, payload: {} });
    expect(KREIVO_USER_EXTENSIONS.DynamicMaxBlockWeight).toEqual({ extrinsic: {}, payload: {} });
  });

  it('refuses a runtime with an extension the app does not register, or without PassAuthenticate', () => {
    expect(() => checkExtensions([...KREIVO_EXTENSIONS, 'CheckSomethingNew'])).toThrow(/CheckSomethingNew/);
    expect(() => checkExtensions(KREIVO_EXTENSIONS.filter((e) => e !== 'PassAuthenticate'))).toThrow(/PassAuthenticate/);
  });

  it('go in the initialisation metadata given to the wallet', () => {
    const def = kreivoMetadataDef({ label: 'Kreivo', genesisHash: GENESIS, ss58Format: 2, specVersion: 136, tokenDecimals: 12, tokenSymbol: 'KSM', rawMetadata: METADATA });
    expect(def).toMatchObject({ chain: 'Kreivo', genesisHash: GENESIS, ss58Format: 2, chainType: 'substrate', specVersion: 136, tokenDecimals: 12, tokenSymbol: 'KSM' });
    expect(def.userExtensions).toBe(KREIVO_USER_EXTENSIONS);
    expect(def.rawMetadata).toBe(toHex(METADATA));
  });
});

describe('signing through a polkadot.js wallet', () => {
  it('polkadot-api’s own polkadot.js bridge refuses Kreivo: PassAuthenticate carries a byte it does not map', async () => {
    const s = getPolkadotSignerFromPjs(ADDRESS, pjsWallet(KREIVO_USER_EXTENSIONS) as never, async () => ({ id: 0, signature: '0x' }));
    await expect(s.signTx(CALL, signed(), METADATA, 41_026_407)).rejects.toThrow('PJS does not support this signed-extension: PassAuthenticate');
  });

  it('hands the wallet the payload it signs: every extension, in the runtime’s order, the custom ones mapped to nothing', () => {
    const { payload, extra } = pjsPayload(ADDRESS, CALL, signed(), METADATA, 41_026_407);
    expect(payload).toEqual({
      address: ADDRESS,
      method: toHex(CALL),
      signedExtensions: KREIVO_EXTENSIONS,
      version: 4,
      specVersion: '0x00000088',
      transactionVersion: '0x0000000e',
      genesisHash: GENESIS,
      era: '0x7500',
      blockHash: BLOCK_HASH,
      blockNumber: '0x02720367',
      nonce: '0x00000005',
      tip: `0x${'00'.repeat(16)}`,
    });
    expect(extra.map(toHex)).toEqual(KREIVO_EXTENSIONS.map((id) => toHex(signed()[id]!.value)));
  });

  it('with Kreivo’s extensions registered, the wallet’s extrinsic is the one polkadot-api’s own signer builds', async () => {
    const ours = await pjsSigner(ADDRESS, PUBLIC, pjsWallet(KREIVO_USER_EXTENSIONS)).signTx(CALL, signed(), METADATA, 41_026_407);
    const papi = await getPolkadotSigner(PUBLIC, 'Ed25519', (m) => ed25519.sign(m, SECRET)).signTx(CALL, signed(), METADATA, 41_026_407);
    expect(toHex(ours)).toBe(toHex(papi));
    // And it is a v4 extrinsic from this account, whose signature Kreivo verifies over its own payload.
    const r = new Reader(ours);
    r.compact();
    expect(toHex(r.take(2))).toBe('0x8400');
    expect(toHex(r.take(32))).toBe(toHex(PUBLIC));
    expect(r.u8()).toBe(0);
    expect(ed25519.verify(r.take(64), kreivoPayload(signed()), PUBLIC)).toBe(true);
    expect(r.u8()).toBe(0); // PassAuthenticate: None.
  });

  it('without them, the wallet signs another payload, which Kreivo would reject, and the page refuses it before broadcasting', async () => {
    const { payload } = pjsPayload(ADDRESS, CALL, signed(), METADATA, 41_026_407);
    const { signature } = await pjsWallet({})(payload);
    expect(ed25519.verify(fromHex(signature as Hex).slice(1), kreivoPayload(signed()), PUBLIC)).toBe(false);
    const refused = pjsSigner(ADDRESS, PUBLIC, pjsWallet({})).signTx(CALL, signed(), METADATA, 41_026_407);
    await expect(refused).rejects.toBeInstanceOf(WalletSignatureMismatch);
    await expect(refused).rejects.toThrow(/does not verify against your account’s key[^]*Offer the wallet Kreivo’s metadata again[^]*Nothing was sent\.$/);
  });

  it('refuses a signature by another key over the right payload', async () => {
    const other = ed25519.utils.randomPrivateKey();
    const wallet = pjsWallet(KREIVO_USER_EXTENSIONS, (m) => concat([Uint8Array.of(0), ed25519.sign(m, other)]));
    await expect(pjsSigner(ADDRESS, PUBLIC, wallet).signTx(CALL, signed(), METADATA, 41_026_407)).rejects.toBeInstanceOf(WalletSignatureMismatch);
  });

  it('takes an Sr25519 account’s signature over the right payload, as Kreivo verifies it', async () => {
    const secret = sr25519.secretFromSeed(fromHex(`0x${'42'.repeat(32)}`));
    const pub = sr25519.getPublicKey(secret);
    const wallet = pjsWallet(KREIVO_USER_EXTENSIONS, (m) => concat([Uint8Array.of(1), sr25519.sign(secret, m)]));
    const xt = await pjsSigner(encodeSs58(pub, 2), pub, wallet).signTx(CALL, signed(), METADATA, 41_026_407);
    const r = new Reader(xt);
    r.compact();
    r.take(2 + 32);
    expect(r.u8()).toBe(1);
    expect(sr25519.verify(kreivoPayload(signed()), r.take(64), pub)).toBe(true);
    await expect(pjsSigner(encodeSs58(pub, 2), pub, pjsWallet({}, (m) => concat([Uint8Array.of(1), sr25519.sign(secret, m)]))).signTx(CALL, signed(), METADATA, 41_026_407)).rejects.toBeInstanceOf(
      WalletSignatureMismatch,
    );
  });

  it('refuses, before the wallet is asked, a PassAuthenticate other than None and a fee paid in an asset', async () => {
    let asked = false;
    const wallet = async () => {
      asked = true;
      return { signature: '0x' };
    };
    const signer = pjsSigner(ADDRESS, PUBLIC, wallet);
    await expect(signer.signTx(CALL, signed({ PassAuthenticate: { value: fromHex('0x01'), additionalSigned: empty } }), METADATA, 1)).rejects.toThrow(/PassAuthenticate is not the value/);
    await expect(signer.signTx(CALL, signed({ ChargeAssetTxPayment: { value: fromHex('0x00010065000000'), additionalSigned: empty } }), METADATA, 1)).rejects.toThrow(/KSM only/);
    const missing = signed();
    delete missing.DynamicMaxBlockWeight;
    await expect(signer.signTx(CALL, missing, METADATA, 1)).rejects.toThrow(/carries no DynamicMaxBlockWeight/);
    expect(asked).toBe(false);
  });

  it('verifies each MultiSignature kind over the signing payload, as Kreivo does', () => {
    const payload = signingPayload(CALL, KREIVO_EXTENSIONS.map((id) => signed()[id]!.value), KREIVO_EXTENSIONS.map((id) => signed()[id]!.additionalSigned));
    expect(toHex(payload)).toBe(toHex(kreivoPayload(signed())));
    const short = signingPayload(fromHex('0x0000'), [], []);
    expect(toHex(short)).toBe('0x0000'); // At most 256 bytes: signed as it is, not hashed.
    const tampered = Uint8Array.from(payload, (b, i) => (i === 0 ? b ^ 1 : b));

    expect(verifyMultiSignature(concat([Uint8Array.of(0), ed25519.sign(payload, SECRET)]), payload, PUBLIC)).toBe(true);
    expect(verifyMultiSignature(concat([Uint8Array.of(0), ed25519.sign(payload, SECRET)]), tampered, PUBLIC)).toBe(false);

    const secret = sr25519.secretFromSeed(fromHex(`0x${'42'.repeat(32)}`));
    const srPub = sr25519.getPublicKey(secret);
    expect(verifyMultiSignature(concat([Uint8Array.of(1), sr25519.sign(secret, payload)]), payload, srPub)).toBe(true);
    expect(verifyMultiSignature(concat([Uint8Array.of(1), sr25519.sign(secret, payload)]), payload, PUBLIC)).toBe(false);

    // Ecdsa: over the payload's BLAKE2b-256; the account is the BLAKE2b-256 of the compressed public key.
    const priv = fromHex(`0x${'11'.repeat(32)}`);
    const account = blake2_256(secp256k1.getPublicKey(priv, true));
    const sig = secp256k1.sign(blake2_256(payload), priv);
    const ecdsa = concat([Uint8Array.of(2), sig.toCompactRawBytes(), Uint8Array.of(sig.recovery)]);
    expect(verifyMultiSignature(ecdsa, payload, account)).toBe(true);
    expect(verifyMultiSignature(concat([ecdsa.slice(0, 65), Uint8Array.of(sig.recovery + 27)]), payload, account)).toBe(true);
    expect(verifyMultiSignature(ecdsa, tampered, account)).toBe(false);
    expect(verifyMultiSignature(ecdsa, payload, PUBLIC)).toBe(false);

    expect(verifyMultiSignature(fromHex(`0x03${'00'.repeat(64)}`), payload, PUBLIC)).toBe(false);
    expect(verifyMultiSignature(fromHex(`0x00${'00'.repeat(63)}`), payload, PUBLIC)).toBe(false);
  });

  it('puts the extrinsic together itself, and refuses a wallet’s answer that is not a MultiSignature', () => {
    expect(() => signedExtrinsic(PUBLIC, fromHex(`0x03${'00'.repeat(64)}`), [], CALL)).toThrow(/MultiSignature/);
    expect(() => signedExtrinsic(PUBLIC, fromHex(`0x01${'00'.repeat(63)}`), [], CALL)).toThrow(/MultiSignature/);
    expect(signedExtrinsic(PUBLIC, fromHex(`0x02${'00'.repeat(65)}`), [], CALL).length).toBeGreaterThan(CALL.length + 99);
  });
});
