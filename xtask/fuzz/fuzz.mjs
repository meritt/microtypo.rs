import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { existsSync, globSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { isDeepStrictEqual, parseArgs } from 'node:util';
import { Worker, isMainThread, parentPort, workerData } from 'node:worker_threads';

import { isRecord, readCases, reference, sha } from './common.mjs';
import { caseSeed, createGenerator } from './generate.mjs';
import { UNMATCHABLE, instrument } from './patterns.mjs';
import { shrink } from './shrink.mjs';

const MAX_DISTINCT = 50;
const MAX_SHRINKS = 400;
const MAX_INDEXED = 256;
const SHRINKS = 0;
const DISTINCT = 1;
const COVERAGE_FLOOR = 30;
const COVERAGE_PER = 100_000;
const PROGRESS_EVERY = 200;
const SERIES = 8;
const PATTERN_WIDTH = 150;
const PATTERN_WORKER_SHARE = 10;
const LONE_SURROGATES = /[\uD800-\uDFFF]/gu;
const INVISIBLE = /[\p{Cc}\p{Cf}\p{Co}\p{Z}\uD800-\uDFFF]/u;
const INVISIBLE_ALL = /[\p{Cc}\p{Cf}\p{Co}\p{Z}\uD800-\uDFFF]/gu;

const overBudget = (outcome) =>
  [outcome.construct, outcome.error, ...(outcome.ops ?? [])].some((e) => e?.name === 'MicroTypoBudgetError');

function printable(s) {
  const out = s.replace(INVISIBLE_ALL, (ch) =>
    ch === ' ' ? ch : `\\u{${ch.codePointAt(0).toString(16).toUpperCase()}}`
  );

  return out.length > PATTERN_WIDTH ? `${out.slice(0, PATTERN_WIDTH - 1)}…` : out;
}

function visible(s) {
  let out = '"';

  for (const ch of s) {
    if (ch === '"' || ch === '\\') {
      out += `\\${ch}`;
    } else if (ch === '\n') {
      out += '\\n';
    } else if (ch !== ' ' && INVISIBLE.test(ch)) {
      out += `\\u{${ch.codePointAt(0).toString(16).toUpperCase()}}`;
    } else {
      out += ch;
    }
  }

  return `${out}"`;
}

function show(value) {
  if (typeof value === 'string') {
    return visible(value);
  }
  if (Array.isArray(value)) {
    return `[${value.map(show).join(',')}]`;
  }
  if (isRecord(value)) {
    return `{${Object.entries(value)
      .map(([k, v]) => `${visible(k)}:${show(v)}`)
      .join(',')}}`;
  }

  return JSON.stringify(value);
}

function caseKey(c) {
  return JSON.stringify([
    Object.hasOwn(c, 'config') ? c.config : '\u0000none',
    c.ops,
    c.text ?? null,
    c.options ?? null
  ]);
}

function scrub(value) {
  return JSON.parse(JSON.stringify(value), (_, item) =>
    typeof item === 'string' ? item.replace(LONE_SURROGATES, '�') : item
  );
}

function knownDivergence(expect, actual) {
  const scrubbed = { ...expect };

  for (const key of ['construct', 'error']) {
    if (expect[key] !== undefined) {
      scrubbed[key] = scrub(expect[key]);
    }
  }
  if (expect.ops !== undefined) {
    scrubbed.ops = expect.ops.map((op) => (op === null ? op : scrub(op)));
  }
  if (expect.then !== undefined) {
    scrubbed.then = expect.then.map((item) => (item.error === undefined ? item : { error: scrub(item.error) }));
  }

  return !isDeepStrictEqual(scrubbed, expect) && isDeepStrictEqual(scrubbed, actual);
}

class Rust {
  constructor(bin) {
    this.bin = bin;
    this.start();
  }

  start() {
    this.pending = null;
    this.child = spawn(this.bin, [], { stdio: ['pipe', 'pipe', 'inherit'] });
    this.child.stdin.on('error', () => {});
    this.child.stdout.setEncoding('utf8');
    let buffered = '';
    this.child.stdout.on('data', (chunk) => {
      buffered += chunk;

      for (let end = buffered.indexOf('\n'); end >= 0; end = buffered.indexOf('\n')) {
        const line = buffered.slice(0, end);
        const pending = this.pending;
        buffered = buffered.slice(end + 1);
        this.pending = null;
        pending?.(JSON.parse(line));
      }
    });
    this.child.on('exit', (code, signal) => {
      const pending = this.pending;
      this.pending = null;
      pending?.({ crash: signal ?? code });
    });
  }

  async run(c) {
    if (this.child.exitCode !== null || this.child.signalCode !== null) {
      this.start();
    }

    const { config, ops, text, options, then } = c;
    const { promise, resolve } = Promise.withResolvers();

    this.pending = resolve;
    this.child.stdin.write(`${JSON.stringify({ config, ops, text, options, then })}\n`);

    return promise;
  }

  close() {
    this.child.stdin.end();
  }
}

async function setup(refDir, indexPath) {
  const { replay } = await reference(refDir, 'test/conformance/replay.mjs');
  const { DEFAULT_GROUPS } = await reference(refDir, 'src/rules/registry.js');
  const { BUNDLE_NAMES, SETTING_SELECTORS } = await reference(refDir, 'src/core/options.js');

  const corpus = JSON.parse(readFileSync(join(refDir, 'test/fixtures/corpus.json'), 'utf8')).map((c) => c.text);
  const big = globSync('test/fixtures/big-text/*.input.txt', { cwd: refDir })
    .toSorted()
    .map((f) => readFileSync(join(refDir, f), 'utf8'));
  const seeds = readCases(refDir);
  const patterns = Object.values(JSON.parse(readFileSync(indexPath, 'utf8')));
  const rules = DEFAULT_GROUPS.flatMap(([group, def]) =>
    def.rules.map((r) => ({ id: `${group}.${r.id}`, htmlOnly: r.htmlOnly === true, enabled: r.enabled !== false }))
  );
  const ruleSelectors = [
    ...DEFAULT_GROUPS.flatMap(([group]) => [group, `${group}.*`]),
    ...rules.map((r) => r.id),
    ...BUNDLE_NAMES,
    ...SETTING_SELECTORS,
    'nobr.nbsp_*',
    'dash.em*'
  ];
  const generate = createGenerator({
    prose: [...corpus, ...big],
    groups: DEFAULT_GROUPS.map(([group]) => group),
    ruleSelectors,
    seeds,
    patterns
  });
  return { replay, rules, generate };
}

async function buildIndex({ refDir, start, step }) {
  const { replay } = await reference(refDir, 'test/conformance/replay.mjs');
  const seeds = readCases(refDir);
  const probe = instrument();
  const index = {};
  const counts = { cases: 0 };

  for (let i = start; i < seeds.length; i += step) {
    const { id, expect, ...c } = JSON.parse(seeds[i]);
    const { value, hit } = probe(() => replay(c));

    if (!overBudget(value)) {
      for (const key of hit) {
        (index[key] ??= []).push(i);
      }
    }

    counts.cases++;
  }

  parentPort.postMessage({ type: 'done', counts, index });
}

async function patternCoverage({ refDir, index: indexPath, seed, cases, start, step }) {
  const { replay, generate } = await setup(refDir, indexPath);
  const probe = instrument();
  const patternHits = {};
  const counts = { patternCases: 0 };

  for (let index = start; index < cases; index += step) {
    const { value, ran, hit } = probe(() => replay(generate(caseSeed(seed, index))));

    if (!overBudget(value)) {
      for (const key of ran) {
        patternHits[key] ??= 0;
      }
      for (const key of hit) {
        patternHits[key]++;
      }
    }

    counts.patternCases++;
  }

  return { type: 'done', counts, patternHits };
}

async function coverage({ refDir, index: indexPath, seed, cases, start, step }) {
  const { rules, generate } = await setup(refDir, indexPath);
  const { MicroTypo } = await reference(refDir, 'src/index.js');
  const hits = rules.map(() => 0);
  const counts = { cases: 0 };

  function attempt(fn) {
    try {
      return { value: fn() };
    } catch (error) {
      return { error: { name: error?.name, message: String(error?.message ?? error) } };
    }
  }

  function engineFor(c, html) {
    const config = html ? { ...(isRecord(c.config) ? c.config : {}), html: true } : c.config;
    const built = attempt(() => new MicroTypo(config));

    if (built.error) {
      return null;
    }

    for (const { method, args } of c.ops ?? []) {
      attempt(() => built.value[method](...args));
    }

    return built.value;
  }

  function countHits(c, engine, chosen) {
    const output = () => attempt(() => engine.process(c.text, c.options));
    const toggle = (rule, enabled) => attempt(() => engine.applyOptions({ rules: { [rule.id]: enabled } }));
    const base = output();

    if (base.error?.name === 'MicroTypoBudgetError') {
      return;
    }

    for (const [k, rule] of rules.entries()) {
      if (!chosen(rule)) {
        continue;
      }

      toggle(rule, true);
      const on = output();
      toggle(rule, false);
      const off = output();
      const differs = !isDeepStrictEqual(on, off);
      const budget = on.error?.name === 'MicroTypoBudgetError' || off.error?.name === 'MicroTypoBudgetError';

      if (differs && !budget) {
        hits[k]++;
      }

      toggle(rule, differs ? isDeepStrictEqual(on, base) : rule.enabled);
    }
  }

  for (let index = start; index < cases; index += step) {
    const c = generate(caseSeed(seed, index));

    if (typeof c.text === 'string') {
      const plain = engineFor(c, false);
      const html = engineFor(c, true);

      if (plain) {
        countHits(c, plain, (rule) => !rule.htmlOnly);
      }
      if (html) {
        countHits(c, html, (rule) => rule.htmlOnly);
      }
    }

    counts.cases++;

    if (counts.cases % PROGRESS_EVERY === 0) {
      parentPort.postMessage({ type: 'progress', counts: { ...counts }, hits: [...hits] });
    }
  }

  return { type: 'done', counts, hits, rules: rules.map((r) => r.id) };
}

async function fuzz({ refDir, index: indexPath, replay: bin, seed, cases, start, step, keepGoing, budget, reuse }) {
  const { replay, generate } = await setup(refDir, indexPath);
  const { MicroTypo } = await reference(refDir, 'src/index.js');
  const rust = new Rust(bin);
  const size = reuse ? SERIES : 1;

  function run(c) {
    if (c.then === undefined) {
      return replay(c);
    }

    let engine;

    try {
      engine = new MicroTypo(c.config);
    } catch {
      return replay(c);
    }

    const first = replay(c, engine);

    return { ...first, then: c.then.map((item) => replay({ ops: [], ...item }, engine)) };
  }

  function series(index) {
    const items = [];

    for (let i = index * size; i < Math.min(cases, (index + 1) * size); i++) {
      items.push(generate(caseSeed(seed, i)));
    }

    const [c, ...rest] = items;
    const then = rest.filter((item) => typeof item.text === 'string');

    if (reuse && typeof c.text === 'string' && then.length > 0) {
      c.then = then.map(({ text, options }) => (options === undefined ? { text } : { text, options }));
    }

    return { c, count: items.length };
  }

  function expectation(c) {
    const first = run(c);

    if (overBudget(first) || first.then?.some(overBudget)) {
      return { skip: 'budget' };
    }

    const written = JSON.stringify(first);

    if (!isDeepStrictEqual(JSON.parse(written), run(c))) {
      return { skip: 'unstable' };
    }

    return { expect: JSON.parse(written) };
  }

  async function verdict(c) {
    const { skip, expect } = expectation(c);

    if (skip) {
      return { kind: skip };
    }

    const reply = await rust.run(c);

    if (reply.skipped) {
      return { kind: 'rust' };
    }
    if (reply.actual !== undefined && isDeepStrictEqual(reply.actual, expect)) {
      return { kind: 'pass' };
    }
    if (reply.actual !== undefined && knownDivergence(expect, reply.actual)) {
      return { kind: 'known' };
    }

    return { kind: 'diverged', expect, actual: reply.actual ?? reply };
  }

  const counts = { cases: 0, pass: 0, budget: 0, unstable: 0, rust: 0, known: 0, diverged: 0 };

  for (let group = start; group * size < cases; group += step) {
    const { c, count } = series(group);
    const index = group * size;
    const result = await verdict(c);

    counts.cases += count;
    counts[result.kind] += count;

    if (result.kind === 'diverged') {
      if (Atomics.load(budget, DISTINCT) < MAX_DISTINCT && Atomics.add(budget, SHRINKS, 1) < MAX_SHRINKS) {
        let runs = 0;
        const small = await shrink(c, async (candidate) => {
          runs++;
          return (await verdict(candidate)).kind === 'diverged';
        });
        const final = await verdict(small);

        parentPort.postMessage({
          type: 'failure',
          counts: { ...counts },
          index,
          length: c.text?.length ?? 0,
          runs,
          case: small,
          expect: final.expect,
          actual: final.actual
        });
      } else {
        parentPort.postMessage({
          type: 'unshrunk',
          counts: { ...counts },
          index,
          case: c,
          expect: result.expect,
          actual: result.actual
        });
      }

      if (!keepGoing) {
        break;
      }
    }

    if (counts.cases % PROGRESS_EVERY === 0) {
      parentPort.postMessage({ type: 'progress', counts: { ...counts } });
    }
  }

  rust.close();
  parentPort.postMessage({ type: 'done', counts });
}

function writeCases(path, entries, header) {
  if (entries.length === 0) {
    rmSync(path, { force: true });
    return;
  }

  const lines = entries.map(({ case: c, expect }) => JSON.stringify({ id: sha(caseKey(c)), ...c, expect }));

  writeFileSync(path, `${[JSON.stringify(header), ...lines].join('\n')}\n`);
}

async function seedIndex(refDir, rev, cache, workers) {
  const code = readFileSync(new URL('./patterns.mjs', import.meta.url));
  const path = join(cache, `seed-index-${rev}-${sha(code)}.json`);

  if (existsSync(path)) {
    return path;
  }

  const began = performance.now();
  const parts = await Promise.all(
    Array.from({ length: workers }, async (_, start) => {
      const thread = new Worker(new URL(import.meta.url), {
        workerData: { mode: 'index', refDir, start, step: workers }
      });
      const [part] = await once(thread, 'message');

      return part;
    })
  );
  const merged = {};

  for (const { index } of parts) {
    for (const [key, list] of Object.entries(index)) {
      (merged[key] ??= []).push(...list);
    }
  }

  const sorted = {};

  for (const key of Object.keys(merged).toSorted()) {
    const list = merged[key].toSorted((a, b) => a - b);
    sorted[key] =
      list.length <= MAX_INDEXED
        ? list
        : Array.from({ length: MAX_INDEXED }, (_, k) => list[Math.floor((k * list.length) / MAX_INDEXED)]);
  }

  mkdirSync(cache, { recursive: true });
  const temporary = `${path}.${process.pid}.tmp`;
  writeFileSync(temporary, JSON.stringify(sorted));
  renameSync(temporary, path);
  process.stdout.write(
    `seed index: ${parts.reduce((total, part) => total + part.counts.cases, 0)} corpus cases, ` +
      `${Object.keys(sorted).length} matching patterns in ${((performance.now() - began) / 1000).toFixed(1)} s (${path})\n`
  );

  return path;
}

async function main() {
  const { values } = parseArgs({
    options: {
      'ref-dir': { type: 'string' },
      replay: { type: 'string' },
      seed: { type: 'string' },
      cases: { type: 'string' },
      jobs: { type: 'string' },
      rev: { type: 'string' },
      failures: { type: 'string' },
      unshrunk: { type: 'string' },
      cache: { type: 'string' },
      'keep-going': { type: 'boolean', default: false },
      coverage: { type: 'boolean', default: false },
      reuse: { type: 'boolean', default: false }
    }
  });
  const refDir = values['ref-dir'];
  const seed = Number(values.seed) >>> 0;
  const cases = Number(values.cases);
  const keepGoing = values['keep-going'];
  const mode = values.coverage ? 'coverage' : 'fuzz';
  const workers = Math.max(1, Math.min(Number(values.jobs), cases));
  const budget = new Int32Array(new SharedArrayBuffer(8));
  const index = await seedIndex(refDir, values.rev, values.cache, workers);
  const indexed = JSON.parse(readFileSync(index, 'utf8'));
  const began = performance.now();
  const totals = new Map();
  const hits = new Map();
  const patternHits = new Map();
  const failures = [];
  const unshrunk = [];
  const seen = new Set();
  let ruleIds = [];
  let diverging = 0;
  let finished = 0;
  let finishing = false;

  const sum = (key) => [...totals.values()].reduce((total, counts) => total + (counts[key] ?? 0), 0);
  const elapsed = () => (performance.now() - began) / 1000;

  const progress = setInterval(() => {
    const done = sum('cases');
    process.stderr.write(
      `${mode}: ${done}/${cases} cases, ${Math.round(done / elapsed())} cases/s` +
        `${mode === 'fuzz' ? `, ${diverging} diverging` : ''}\n`
    );
  }, 5000);

  function finishCoverage() {
    const done = sum('cases');
    const floor = (COVERAGE_FLOOR * done) / COVERAGE_PER;
    const totalsByRule = ruleIds
      .map((id, k) => ({ id, hits: [...hits.values()].reduce((total, list) => total + list[k], 0) }))
      .toSorted((a, b) => a.hits - b.hits || (a.id < b.id ? -1 : 1));
    const below = totalsByRule.filter((r) => r.hits < floor);
    const per = (n) => ((n * COVERAGE_PER) / done).toFixed(0).padStart(9);

    process.stdout.write(`${'rule'.padEnd(36)} ${'cases'.padStart(8)} ${'per 100k'.padStart(9)}\n`);
    for (const r of totalsByRule) {
      process.stdout.write(
        `${r.id.padEnd(36)} ${String(r.hits).padStart(8)} ${per(r.hits)}${r.hits < floor ? '  below' : ''}\n`
      );
    }

    const byPattern = new Map(Object.keys(indexed).map((key) => [key, 0]));

    for (const list of patternHits.values()) {
      for (const [key, n] of Object.entries(list)) {
        byPattern.set(key, (byPattern.get(key) ?? 0) + n);
      }
    }

    const patterns = [...byPattern]
      .map(([key, n]) => ({ key, hits: n }))
      .toSorted((a, b) => a.hits - b.hits || (a.key < b.key ? -1 : 1));
    const checked = patterns.filter((p) => !UNMATCHABLE.has(p.key));
    const rare = checked.filter((p) => p.hits < floor);

    process.stdout.write(`\n${'cases'.padStart(8)} ${'per 100k'.padStart(9)}  pattern\n`);
    for (const p of checked) {
      process.stdout.write(
        `${String(p.hits).padStart(8)} ${per(p.hits)}  ${printable(p.key)}${p.hits < floor ? '  below' : ''}\n`
      );
    }
    process.stdout.write(`\nexcluded as unmatchable:\n`);
    for (const [key, reason] of UNMATCHABLE) {
      process.stdout.write(`${String(byPattern.get(key) ?? 0).padStart(8)}  ${printable(key)}: ${reason}\n`);
    }

    process.stdout.write(
      `coverage: seed ${seed}, reference ${values.rev}: ${done} cases in ${elapsed().toFixed(1)} s; ` +
        `${totalsByRule.length} rules, ${below.length} below ${COVERAGE_FLOOR} per ${COVERAGE_PER}; ` +
        `${patterns.length} patterns, ${rare.length} below ${COVERAGE_FLOOR} per ${COVERAGE_PER}, ` +
        `${patterns.length - checked.length} excluded\n`
    );
    process.exit(below.length > 0 || rare.length > 0 ? 1 : 0);
  }

  async function finishFuzz() {
    const done = sum('cases');
    const seconds = elapsed();
    const { VERSION } = await reference(refDir, 'src/index.js');
    const header = (corpus) => ({ corpus, schema: 1, engine: VERSION, reference: values.rev });

    writeCases(values.failures, failures, header('microtypo-fuzz-failures'));
    writeCases(values.unshrunk, unshrunk, header('microtypo-fuzz-unshrunk'));

    process.stdout.write(
      `fuzz: seed ${seed}, reference ${values.rev}: ${done} cases in ${seconds.toFixed(1)} s, ` +
        `${Math.round(done / seconds)} cases/s; ${sum('pass')} matched, ${sum('known')} known divergences, ` +
        `skipped ${sum('budget')} over budget, ${sum('unstable')} unstable, ${sum('rust')} unrepresentable; ` +
        `${diverging} diverging: ${failures.length} distinct after shrinking` +
        `${failures.length > 0 ? ` (${values.failures})` : ''}, ${unshrunk.length} not shrunk` +
        `${unshrunk.length > 0 ? ` (${values.unshrunk})` : ''}\n`
    );
    process.exit(diverging > 0 ? 1 : 0);
  }

  function finish() {
    if (finishing) {
      return;
    }

    finishing = true;
    clearInterval(progress);

    if (mode === 'coverage') {
      finishCoverage();
    } else {
      finishFuzz();
    }
  }

  function report(message) {
    const key = caseKey(message.case);

    if (seen.has(key)) {
      return;
    }

    seen.add(key);
    Atomics.add(budget, DISTINCT, 1);
    failures.push(message);
    process.stdout.write(
      `fuzz: divergence at seed ${seed}, case ${message.index} ` +
        `(shrunk from ${message.length} to ${message.case.text?.length ?? 0} units in ${message.runs} runs)\n` +
        `  case:     ${show(message.case)}\n` +
        `  expected: ${show(message.expect)}\n` +
        `  actual:   ${show(message.actual)}\n`
    );
  }

  process.stdout.write(`${mode}: seed ${seed}, ${cases} cases, ${workers} workers, reuse ${values.reuse}, reference ${values.rev}\n`);

  const patternWorkers = mode === 'coverage' && workers > 1
    ? Math.min(Math.ceil(workers / PATTERN_WORKER_SHARE), workers - 1)
    : 0;
  const coverageWorkers = workers - patternWorkers;
  const jobs = mode === 'coverage' && workers === 1
    ? [{ mode: 'coverage-patterns', start: 0, step: 1 }]
    : [
        ...Array.from({ length: coverageWorkers }, (_, start) => ({ mode, start, step: coverageWorkers })),
        ...Array.from({ length: patternWorkers }, (_, start) => ({ mode: 'patterns', start, step: patternWorkers }))
      ];

  for (const job of jobs) {
    const thread = new Worker(new URL(import.meta.url), {
      workerData: {
        ...job,
        refDir,
        index,
        replay: values.replay,
        seed,
        cases,
        keepGoing,
        budget,
        reuse: values.reuse
      }
    });

    totals.set(thread, {});
    thread.on('message', (message) => {
      totals.set(thread, message.counts);

      if (message.hits) {
        hits.set(thread, message.hits);
      }
      if (message.patternHits) {
        patternHits.set(thread, message.patternHits);
      }
      if (message.rules) {
        ruleIds = message.rules;
      }
      if (message.type === 'failure' || message.type === 'unshrunk') {
        diverging++;

        if (message.type === 'failure') {
          report(message);
        } else {
          unshrunk.push(message);
        }
        if (!keepGoing) {
          finish();
        }
      }
      if (message.type === 'done' && ++finished === jobs.length) {
        finish();
      }
    });
    thread.on('error', (error) => {
      process.stderr.write(`${mode}: worker failed: ${error.stack ?? error}\n`);
      process.exit(2);
    });
  }
}

if (isMainThread) {
  await main();
} else if (workerData.mode === 'index') {
  await buildIndex(workerData);
} else if (workerData.mode === 'coverage') {
  parentPort.postMessage(await coverage(workerData));
} else if (workerData.mode === 'patterns') {
  parentPort.postMessage(await patternCoverage(workerData));
} else if (workerData.mode === 'coverage-patterns') {
  const result = await coverage(workerData);
  const patterns = await patternCoverage(workerData);
  parentPort.postMessage({ ...result, patternHits: patterns.patternHits });
} else {
  await fuzz(workerData);
}
