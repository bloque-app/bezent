// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
// The build's hash (F-018, T-018-12): SHA-256 of every file under the build directory, listed in byte order
// of their paths as `sha256sum` prints them, and the SHA-256 of that list, which names the whole bundle.
// Two builds of one revision must print the same bundle hash (CI compares them), and the release publishes it.
//
//   node scripts/hash.mjs dist            # prints the list, then "bundle <hash>"
//   node scripts/hash.mjs dist out.txt    # also writes the list and the bundle line to out.txt

import { createHash } from 'node:crypto';
import { readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join, relative, sep } from 'node:path';

const [dir, out] = process.argv.slice(2);
if (!dir) {
  console.error('usage: node scripts/hash.mjs <build directory> [output file]');
  process.exit(2);
}

function files(d) {
  return readdirSync(d, { withFileTypes: true }).flatMap((e) => {
    const p = join(d, e.name);
    if (e.isDirectory()) return files(p);
    if (e.isFile()) return [p];
    throw new Error(`${p}: neither a file nor a directory`);
  });
}

const sha = (b) => createHash('sha256').update(b).digest('hex');
const paths = files(dir)
  .map((p) => relative(dir, p).split(sep).join('/'))
  .sort((a, b) => (Buffer.from(a) < Buffer.from(b) ? -1 : Buffer.from(a) > Buffer.from(b) ? 1 : 0));
const list = paths.map((p) => `${sha(readFileSync(join(dir, p)))}  ${p}\n`).join('');
const text = `${list}bundle ${sha(list)}\n`;
process.stdout.write(text);
if (out) writeFileSync(out, text);
