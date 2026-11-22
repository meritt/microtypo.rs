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

const group = 'custom\ud800';
const id = 'replace\udc00';
const data = new api.MicroTypo({
  presets: false,
  ruleGroups: [{ name: group, rules: [{ id, pattern: { source: 'a', flags: 'g' }, replacement: '\ud800' }] }]
});
const source = '😀 aa';
const result = data.process(source, { trace: true, edits: true, changes: true });
assert.equal(result.text, '😀 \ud800\ud800');
assert.deepEqual(result.trace, [{ rule: `${group}.${id}`, kind: 'regex', count: 2, ranges: [[3, 4], [4, 5]] }]);
let reconstructed = '';
let cursor = 0;
for (const edit of result.edits) {
  reconstructed += source.slice(cursor, edit.start) + edit.text;
  cursor = edit.end;
}
assert.equal(reconstructed + source.slice(cursor), result.text);
assert.equal(data.getRuleGroup(group).name, group);
assert.equal(data.listRuleGroups()[0].rules[0].id, id);
data.applyOptions({ rules: { [`${group}.${id}`]: false } });
assert.equal(data.process('a'), 'a');
data.registerRuleGroup({ name: 'second\udc00', rules: [{ id: 'r', pattern: { source: '\ud800', flags: 'gu' }, replacement: 'b' }] }, { position: `after:${group}` });
data.applyOptions({ rules: { [`${group}.${id}`]: true } });
assert.equal(data.process('a'), 'b');
assert.throws(() => data.process('a', { trace: 1 }), api.MicroTypoConfigError);
assert.equal(data.process('a'), 'b');
data.free();

for (const [pattern, flags, input, expected, count] of [
  [String.raw`[\q{A}]`, 'giv', 'aA', 'xA', 1],
  [String.raw`[^\q{A}]`, 'giv', 'aA', 'ax', 1],
  [String.raw`[^\W]`, 'giu', 'ſK', 'xx', 2],
  ['[a]', 'giv', 'aA', 'xx', 2],
  ['😀', 'g', '😀', 'x', 1],
  ['[😀]', 'g', '😀', 'xx', 2],
  ['😀+', 'g', '😀😀', 'xx', 2],
  ['😀+', 'gu', '😀😀', 'x', 1],
  [String.raw`\u{1F600}`, 'g', 'u{1F600}', 'x', 1],
  [String.raw`(?<\u{10400}>a)\k<𐐀>`, 'g', 'aa', 'x', 1],
  [String.raw`\uD800\u0041`, 'gu', '\ud800A', 'x', 1]
]) {
  const engine = new api.MicroTypo({ presets: false, input: { format: 'plain', placeholders: 'off' }, ruleGroups: [{
    name: 'custom', rules: [{ id: 'r', pattern: { source: pattern, flags }, replacement: 'x' }]
  }] });
  const result = engine.process(input, { trace: true, edits: true, changes: true });
  assert.equal(result.text, expected);
  assert.equal(result.trace.reduce((sum, entry) => sum + entry.count, 0), count);
  let rebuilt = '';
  let at = 0;
  for (const edit of result.edits) {
    rebuilt += input.slice(at, edit.start) + edit.text;
    at = edit.end;
  }
  assert.equal(rebuilt + input.slice(at), result.text);
  assert.throws(() => engine.registerRuleGroup({ name: 'custom', rules: [{
    id: 'bad', pattern: { source: String.raw`[a-\d]`, flags: 'gv' }, replacement: 'y'
  }] }), { name: 'MicroTypoConfigError', message: String.raw`invalid rule pattern: Invalid regular expression: /[a-\d]/gv: Invalid character class` });
  assert.equal(engine.process(input), expected);
  engine.free();
}

if (kind === 'module') {
  for (const [pattern, flags, input, expected, ranges] of [
    [String.raw`[\b]`, 'gv', 'b\b', 'bx', [[1, 2]]],
    [String.raw`[\b]`, 'giv', 'b\b', 'bx', [[1, 2]]],
    [String.raw`[\q{\b}]`, 'gv', 'b\b', 'bx', [[1, 2]]],
    [String.raw`[\q{\b}]`, 'giv', 'b\b', 'bx', [[1, 2]]],
    ['[!#]', 'gv', '!#$[]@', 'xx$[]@', [[0, 1], [1, 2]]],
    [String.raw`[\q{\[}]`, 'gv', '!#$[]@', '!#$x]@', [[3, 4]]]
  ]) {
    const engine = new api.MicroTypo({ presets: false, input: 'plain', ruleGroups: [{
      name: 'custom', rules: [{ id: 'r', pattern: { source: pattern, flags }, replacement: 'x' }]
    }] });
    const result = engine.process(input, { trace: true, edits: true, changes: true });
    assert.equal(result.text, expected);
    assert.deepEqual(result.trace, [{ rule: 'custom.r', kind: 'regex', count: ranges.length, ranges }]);
    if (input === 'b\b') {
      assert.deepEqual(result.changes, [[1, 2]]);
      assert.deepEqual(result.edits, [{ start: 1, end: 2, text: 'x' }]);
    }
    let rebuilt = '';
    let at = 0;
    for (const edit of result.edits) {
      rebuilt += input.slice(at, edit.start) + edit.text;
      at = edit.end;
    }
    assert.equal(rebuilt + input.slice(at), result.text);
    for (const source of [String.raw`[\q{[}]`, String.raw`[\q{]}]`]) {
      assert.throws(() => engine.registerRuleGroup({ name: 'custom', rules: [{
        id: 'bad', pattern: { source, flags: 'gv' }, replacement: 'y'
      }] }), {
        name: 'MicroTypoConfigError', code: 'ERR_MICROTYPO_CONFIG',
        message: `invalid rule pattern: Invalid regular expression: /${source}/gv: Invalid character in character class`
      });
      assert.equal(engine.process(input), expected);
    }
    engine.free();
  }
}

const cycled = new api.MicroTypo({ presets: false, ruleGroups: [{ name: 'cycle', rules: [{ id: 'grow\ud800', cycled: true, pattern: { source: '^', flags: 'g' }, replacement: 'x' }] }] });
const limits = [];
cycled.onCycleLimit = (id, count) => limits.push([id, count]);
assert.equal(cycled.process(''), 'x'.repeat(100));
assert.equal(cycled.process('a'), 'x'.repeat(100) + 'a');
assert.deepEqual(limits, [['grow\ud800', 100], ['grow\ud800', 100]]);
cycled.free();

const budgeted = new api.MicroTypo({ input: 'markdown', maxProcessingMs: 200, maxInputLength: 60_000 });
assert.throws(() => budgeted.process(' '.repeat(60_000)), {
  name: 'MicroTypoBudgetError',
  message: /^Processing exceeded 200ms budget at /
});

assert.equal(await load(), api);

console.log(`@meritt/microtypo-wasm ${api.VERSION}: loads by ${kind}`);
