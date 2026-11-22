import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { isDeepStrictEqual } from 'node:util';

const [entry, corpusArg] = process.argv.slice(2);

if (!entry) {
  console.error('usage: corpus.mjs <entry file> [corpus.jsonl]');
  process.exit(2);
}

const corpusPath =
  corpusArg ??
  process.env.MICROTYPO_CORPUS ??
  join(import.meta.dirname, '../../../testdata/conformance/corpus.jsonl');
const { load } = await import(pathToFileURL(resolve(entry)).href);
const { MicroTypo } = await load();

function describeError(error) {
  const out = { name: error?.name ?? 'Error', message: String(error?.message ?? error) };

  if (error?.code !== undefined) {
    out.code = error.code;
  }

  if (error?.details !== undefined) {
    out.details = error.details;
  }

  return out;
}

function replay(c) {
  let engine;

  try {
    engine = Object.hasOwn(c, 'config') ? new MicroTypo(c.config) : new MicroTypo();
  } catch (error) {
    return { construct: describeError(error) };
  }

  try {
    const ops = c.ops.map(({ method, args }) => {
      try {
        engine[method](...args);
        return null;
      } catch (error) {
        return describeError(error);
      }
    });
    const outcome = ops.some((op) => op !== null) ? { ops } : {};

    if (!Object.hasOwn(c, 'text')) {
      return outcome;
    }

    try {
      const result = Object.hasOwn(c, 'options')
        ? engine.process(c.text, c.options)
        : engine.process(c.text);

      return { ...outcome, output: result };
    } catch (error) {
      return { ...outcome, error: describeError(error) };
    }
  } finally {
    engine.free();
  }
}

const [header, ...cases] = readFileSync(corpusPath, 'utf8')
  .split('\n')
  .filter(Boolean)
  .map((line) => JSON.parse(line));

if (cases.length !== header.cases) {
  console.error(`corpus header says ${header.cases} cases, file holds ${cases.length}`);
  process.exit(1);
}

const started = performance.now();
const failed = cases.filter((c) => !isDeepStrictEqual(replay(c), c.expect));
const seconds = ((performance.now() - started) / 1000).toFixed(1);

for (const c of failed.slice(0, 10)) {
  console.error(JSON.stringify({ id: c.id, expect: c.expect, got: replay(c) }));
}

console.log(
  `${cases.length - failed.length}/${cases.length} cases match engine ${header.engine} (${seconds} s)`
);
process.exit(failed.length === 0 ? 0 : 1);
