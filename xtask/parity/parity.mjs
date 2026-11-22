import { once } from 'node:events';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { availableParallelism } from 'node:os';
import { join } from 'node:path';
import { parseArgs } from 'node:util';
import { Worker, isMainThread, parentPort, workerData } from 'node:worker_threads';

import { readCases, reference, sha } from '../fuzz/common.mjs';
import { instrument, patternKey } from '../fuzz/patterns.mjs';
import { EXPRESSIONS, RULES } from './exceptions.mjs';

async function replayShare({ refDir, start, step }) {
  const { replay } = await reference(refDir, 'test/conformance/replay.mjs');
  const probe = instrument();
  const ran = new Set();
  const cases = readCases(refDir);

  for (let i = start; i < cases.length; i += step) {
    const { id, expect, ...c } = JSON.parse(cases[i]);

    for (const key of probe(() => replay(c)).ran) {
      ran.add(key);
    }
  }

  parentPort.postMessage([...ran]);
}

async function executed(refDir, rev, cache) {
  const code = readFileSync(new URL('../fuzz/patterns.mjs', import.meta.url));
  const path = join(cache, `executed-${rev}-${sha(code)}.json`);

  if (existsSync(path)) {
    return new Set(JSON.parse(readFileSync(path, 'utf8')));
  }

  const workers = availableParallelism();
  const shares = await Promise.all(
    Array.from({ length: workers }, async (_, start) => {
      const thread = new Worker(new URL(import.meta.url), { workerData: { refDir, start, step: workers } });
      const [share] = await once(thread, 'message');

      return share;
    })
  );
  const keys = [...new Set(shares.flat())].toSorted();

  mkdirSync(cache, { recursive: true });
  writeFileSync(path, JSON.stringify(keys));

  return new Set(keys);
}

function referenceGroups(defaultGroups) {
  const replacementOf = (r) => (typeof r === 'function' ? { kind: 'function' } : { kind: 'string', value: r ?? '' });

  return defaultGroups.map(([name, def]) => ({
    name,
    title: def.title,
    classes: def.classes ?? null,
    preParse: typeof def.preParse === 'function',
    rules: def.rules.map((rule) => {
      let body;

      if (typeof rule.handler === 'function') {
        body = { kind: 'handler' };
      } else {
        const patterns = Array.isArray(rule.pattern) ? rule.pattern : [rule.pattern];
        const replacements = Array.isArray(rule.replacement) ? rule.replacement : patterns.map(() => rule.replacement);

        body = {
          kind: 'patterns',
          patterns: patterns.map((re, i) => ({
            source: re.source,
            flags: re.flags,
            replacement: replacementOf(replacements[i])
          }))
        };
      }

      return {
        id: rule.id,
        enabled: rule.enabled !== false,
        description: rule.description ?? '',
        htmlOnly: Boolean(rule.htmlOnly),
        cycled: Boolean(rule.cycled),
        requires: rule.requires ?? 0,
        requiresAny: rule.requiresAny ?? 0,
        requiresPattern: rule.requiresPattern ? regex(rule.requiresPattern) : null,
        body
      };
    })
  }));
}

const asRegExp = (source, flags) => {
  try {
    return new RegExp(source, flags);
  } catch {
    return null;
  }
};

const normalized = (p) => ({ ...p, source: asRegExp(p.source, p.flags)?.source ?? p.source });

const rustGroups = (groups) =>
  groups.map((group) => ({
    ...group,
    rules: group.rules.map((rule) => {
      const requiresPattern = rule.requiresPattern === null ? null : regex(normalized(rule.requiresPattern));

      return rule.body.kind === 'patterns'
        ? { ...rule, requiresPattern, body: { ...rule.body, patterns: rule.body.patterns.map(normalized) } }
        : { ...rule, requiresPattern };
    })
  }));

const show = (value) => (typeof value === 'string' ? value : JSON.stringify(value));
const regex = (p) => `/${p.source}/${p.flags}`;
const replacement = (r) => (r.kind === 'function' ? 'function' : `string ${JSON.stringify(r.value)}`);
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const visible = (s) =>
  s.replace(/[\u{A0}\u{2009}\u{202F}\u{E000}-\u{F8FF}]/gu, (c) => `\\u{${c.codePointAt(0).toString(16).toUpperCase()}}`);

function compareRegistry(reference, rust) {
  const findings = [];
  const report = (where, what, was, now) => findings.push({ where, what, was, now });
  const order = (where, what, refNames, rustNames) => {
    if (!same(refNames, rustNames)) {
      report(where, what, refNames.join(' '), rustNames.join(' '));
    }
  };

  order(
    'registry',
    'group order',
    reference.map((g) => g.name),
    rust.map((g) => g.name)
  );

  for (const refGroup of reference) {
    const rustGroup = rust.find((g) => g.name === refGroup.name);

    if (rustGroup === undefined) {
      report(refGroup.name, 'group', 'present', 'missing');
      continue;
    }

    for (const field of ['title', 'classes', 'preParse']) {
      if (!same(refGroup[field], rustGroup[field])) {
        report(refGroup.name, field, show(refGroup[field]), show(rustGroup[field]));
      }
    }

    order(
      refGroup.name,
      'rule order',
      refGroup.rules.map((r) => r.id),
      rustGroup.rules.map((r) => r.id)
    );

    for (const refRule of refGroup.rules) {
      const where = `${refGroup.name}.${refRule.id}`;
      const rustRule = rustGroup.rules.find((r) => r.id === refRule.id);

      if (rustRule === undefined) {
        report(where, 'rule', 'present', 'missing');
        continue;
      }

      for (const field of ['enabled', 'htmlOnly', 'cycled', 'description', 'requires', 'requiresAny', 'requiresPattern']) {
        if (refRule[field] !== rustRule[field]) {
          report(where, field, show(refRule[field]), show(rustRule[field]));
        }
      }

      if (refRule.body.kind !== rustRule.body.kind) {
        report(where, 'kind', refRule.body.kind, rustRule.body.kind);
        continue;
      }

      if (refRule.body.kind !== 'patterns') {
        continue;
      }

      const refPatterns = refRule.body.patterns;
      const rustPatterns = rustRule.body.patterns;

      if (refPatterns.length !== rustPatterns.length) {
        report(where, 'pattern count', String(refPatterns.length), String(rustPatterns.length));
      }

      for (let i = 0; i < Math.min(refPatterns.length, rustPatterns.length); i++) {
        const label = refPatterns.length > 1 ? `pattern ${i + 1} ` : '';

        if (regex(refPatterns[i]) !== regex(rustPatterns[i])) {
          report(where, `${label}regex`, regex(refPatterns[i]), regex(rustPatterns[i]));
        }

        if (!same(refPatterns[i].replacement, rustPatterns[i].replacement)) {
          report(
            where,
            `${label}replacement`,
            replacement(refPatterns[i].replacement),
            replacement(rustPatterns[i].replacement)
          );
        }
      }
    }
  }

  for (const rustGroup of rust) {
    const refGroup = reference.find((g) => g.name === rustGroup.name);

    if (refGroup === undefined) {
      report(rustGroup.name, 'group', 'missing', 'present');
      continue;
    }

    for (const rustRule of rustGroup.rules) {
      if (!refGroup.rules.some((r) => r.id === rustRule.id)) {
        report(`${rustGroup.name}.${rustRule.id}`, 'rule', 'missing', 'present');
      }
    }
  }

  return findings;
}

const ESCAPED_UNIT = /(?<!\\)((?:\\\\)*)\\(?:x([0-9a-fA-F]{2})|u([0-9a-fA-F]{4})|u\{([0-9a-fA-F]+)\})/g;

const literal = (key) => {
  const unicode = /[uv]/.test(key.slice(key.lastIndexOf('/') + 1));

  return key.replace(ESCAPED_UNIT, (whole, lead, x, u, braced) => {
    if (braced !== undefined && !unicode) {
      return whole;
    }

    const code = Number.parseInt(x ?? u ?? braced, 16);
    const plain = code >= 0xa0 && code !== 0x2028 && code !== 0x2029 && (code < 0xd800 || code > 0xdfff);

    return plain && code <= 0x10ffff ? `${lead}${String.fromCodePoint(code)}` : whole;
  });
};

const keyOf = (source, flags) => literal(patternKey(asRegExp(source, flags) ?? { source, flags }));

function registryKeys(groups) {
  const patterns = groups.flatMap((g) => g.rules.flatMap((r) => (r.body.kind === 'patterns' ? r.body.patterns : [])));

  return patterns.flatMap(({ source, flags }) => [
    keyOf(source, flags),
    keyOf(source, flags.includes('g') ? flags : `${flags}g`)
  ]);
}

function compareExpressions(executedKeys, builtins, compared) {
  const ran = new Set([...executedKeys].map(literal));
  const core = new Set(builtins.map(([source, flags]) => keyOf(source, flags)));

  return [
    ...[...ran].filter((key) => !core.has(key)).map((key) => ({ key, side: 'reference' })),
    ...[...core].filter((key) => !ran.has(key)).map((key) => ({ key, side: 'core' }))
  ]
    .filter(({ key }) => !compared.has(key))
    .toSorted((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
}

async function main() {
  const { values: args } = parseArgs({
    options: {
      'ref-dir': { type: 'string' },
      rust: { type: 'string' },
      rev: { type: 'string' },
      cache: { type: 'string' }
    }
  });
  const refDir = args['ref-dir'];
  const { DEFAULT_GROUPS } = await reference(refDir, 'src/rules/registry.js');
  const rust = JSON.parse(readFileSync(args.rust, 'utf8'));

  const differs = [];
  const used = new Set();

  const refGroups = referenceGroups(DEFAULT_GROUPS);
  const coreGroups = rustGroups(rust.groups);

  for (const finding of compareRegistry(refGroups, coreGroups)) {
    const exception = RULES.find(
      (e) =>
        e.rule === finding.where && e.what === finding.what && e.reference === finding.was && e.core === finding.now
    );

    if (exception === undefined) {
      differs.push(`${finding.where} ${finding.what}\n  reference: ${finding.was}\n  core:      ${finding.now}`);
    } else {
      used.add(exception);
    }
  }

  const ran = await executed(refDir, args.rev, args.cache);

  const compared = new Set([...registryKeys(refGroups), ...registryKeys(coreGroups)]);

  for (const { key, side } of compareExpressions(ran, rust.builtins, compared)) {
    const exception = EXPRESSIONS.find((e) => e.key === key && e.side === side);

    if (exception === undefined) {
      differs.push(`expression only in the ${side}: ${key}`);
    } else {
      used.add(exception);
    }
  }

  for (const exception of [...RULES, ...EXPRESSIONS].filter((e) => !used.has(e))) {
    differs.push(`stale exception in xtask/parity/exceptions.mjs: ${exception.rule ?? exception.key}`);
  }

  const rules = DEFAULT_GROUPS.reduce((n, [, def]) => n + def.rules.length, 0);

  for (const line of differs) {
    console.log(visible(line));
  }

  console.log(
    `parity with the reference at ${args.rev}: ${DEFAULT_GROUPS.length} groups, ${rules} rules, ` +
      `${ran.size} executed expressions; ${differs.length} differences, ${used.size} listed in xtask/parity/exceptions.mjs`
  );
  process.exitCode = differs.length === 0 ? 0 : 1;
}

if (isMainThread) {
  await main();
} else {
  await replayShare(workerData);
}
