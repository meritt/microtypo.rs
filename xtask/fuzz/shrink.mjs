import { isRecord } from './common.mjs';

const SIMPLER = ['a', ' ', '0', '\uD800'];

const without = (object, key) => Object.fromEntries(Object.entries(object).filter(([k]) => k !== key));

async function ddmin(items, fails) {
  let parts = 2;
  let current = items;

  while (current.length >= 2) {
    const size = Math.ceil(current.length / parts);
    let reduced = false;

    for (let i = 0; i < current.length; i += size) {
      const rest = [...current.slice(0, i), ...current.slice(i + size)];

      if (await fails(rest)) {
        current = rest;
        parts = Math.max(parts - 1, 2);
        reduced = true;
        break;
      }
    }

    if (!reduced) {
      if (parts >= current.length) {
        break;
      }

      parts = Math.min(parts * 2, current.length);
    }
  }

  if (current.length === 1 && (await fails([]))) {
    return [];
  }

  return current;
}

const TEXT = { get: (c) => c.text, set: (c, text) => ({ ...c, text }) };

const thenText = (i) => ({
  get: (c) => c.then[i].text,
  set: (c, text) => ({ ...c, then: c.then.with(i, { ...c.then[i], text }) })
});

const texts = (c) => [TEXT, ...(c.then ?? []).map((_, i) => thenText(i))];

async function shrinkText(c, fails, split, lens) {
  const kept = await ddmin(split(lens.get(c)), (items) => fails(lens.set(c, items.join(''))));

  return lens.set(c, kept.join(''));
}

async function simplifyText(c, fails, lens) {
  const chars = Array.from(lens.get(c));

  for (let i = 0; i < chars.length; i++) {
    for (const simpler of SIMPLER) {
      if (chars[i] === simpler) {
        break;
      }

      const candidate = chars.with(i, simpler);

      if (await fails(lens.set(c, candidate.join('')))) {
        chars[i] = simpler;
        break;
      }
    }
  }

  return lens.set(c, chars.join(''));
}

function* reductions(c) {
  if (Object.hasOwn(c, 'then')) {
    yield without(c, 'then');

    for (let i = 0; i < c.then.length; i++) {
      if (c.then.length > 1) {
        yield { ...c, then: c.then.toSpliced(i, 1) };
      }

      if (Object.hasOwn(c.then[i], 'options')) {
        yield { ...c, then: c.then.with(i, without(c.then[i], 'options')) };
      }
    }
  }

  if (Object.hasOwn(c, 'options')) {
    yield without(c, 'options');
  }

  for (let i = 0; i < c.ops.length; i++) {
    yield { ...c, ops: c.ops.toSpliced(i, 1) };
  }

  if (!Object.hasOwn(c, 'config')) {
    return;
  }

  yield without(c, 'config');

  if (!isRecord(c.config)) {
    return;
  }

  for (const [key, value] of Object.entries(c.config)) {
    yield { ...c, config: without(c.config, key) };

    if (!isRecord(value)) {
      continue;
    }

    for (const sub of Object.keys(value)) {
      yield { ...c, config: { ...c.config, [key]: without(value, sub) } };
    }

    if (key === 'input' && Object.keys(value).length === 1 && typeof value.format === 'string') {
      yield { ...c, config: { ...c.config, input: value.format } };
    }
  }
}

async function shrinkShape(c, fails) {
  let current = c;
  let changed = true;

  while (changed) {
    changed = false;

    for (const candidate of reductions(current)) {
      if (await fails(candidate)) {
        current = candidate;
        changed = true;
        break;
      }
    }
  }

  return current;
}

export async function shrink(c, fails) {
  let current = c;

  for (let round = 0; round < 4; round++) {
    const before = JSON.stringify(current);

    for (const lens of texts(current)) {
      current = await shrinkText(current, fails, (text) => text.split(/(?<=\n)/), lens);
      current = await shrinkText(current, fails, (text) => Array.from(text), lens);
    }
    current = await shrinkShape(current, fails);
    for (const lens of texts(current)) {
      current = await simplifyText(current, fails, lens);
    }

    if (JSON.stringify(current) === before) {
      break;
    }
  }

  return current;
}
