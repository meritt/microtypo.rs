import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { load } from '@meritt/microtypo-wasm';

const text = '"Амбер" - город';
const expected = '«Амбер» — город';

const read = () => readFile(new URL(import.meta.resolve('@meritt/microtypo-wasm/microtypo.wasm')));
const sources = {
  url: async () => undefined,
  bytes: read,
  module: async () => WebAssembly.compile(await read())
};
const kind = process.argv[2];

if (!Object.hasOwn(sources, kind)) {
  console.error(`usage: consumer.mjs <${Object.keys(sources).join(' | ')}>`);
  process.exit(2);
}

await assert.rejects(load(new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0])), {
  name: 'MicroTypoVersionError',
  code: 'ERR_MICROTYPO_VERSION'
});

const api = await load(await sources[kind]());
const typo = new api.MicroTypo({ html: false });
assert.equal(typo.process(text), expected);
assert.throws(() => typo.setLayout('none'), api.MicroTypoConfigError);
assert.equal(api.microtypo(text, { html: false }), expected);
assert.equal(api.DEFAULT_MAX_INPUT_LENGTH, 30_000);

assert.equal(await load(), api);

console.log(`@meritt/microtypo-wasm ${api.VERSION}: loads by ${kind}`);
