import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { load } from '@meritt/microtypo-wasm';

const text = '"Амбер" - город';
const expected = '«Амбер» — город';

const api = await load();
const typo = new api.MicroTypo({ html: false });
assert.equal(typo.process(text), expected);
assert.throws(() => typo.setLayout('none'), api.MicroTypoConfigError);
assert.equal(api.microtypo(text, { html: false }), expected);

const bytes = await readFile(new URL(import.meta.resolve('@meritt/microtypo-wasm/microtypo.wasm')));
const fromBytes = await load(bytes);
assert.equal(fromBytes.VERSION, api.VERSION);
assert.equal(new fromBytes.MicroTypo({ html: false }).process(text), expected);

const fromModule = await load(await WebAssembly.compile(bytes));
assert.equal(fromModule.DEFAULT_MAX_INPUT_LENGTH, api.DEFAULT_MAX_INPUT_LENGTH);

await assert.rejects(load(new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0])), {
  name: 'MicroTypoVersionError',
  code: 'ERR_MICROTYPO_VERSION'
});

console.log(`@meritt/microtypo-wasm ${api.VERSION}: loads by URL, bytes and module`);
