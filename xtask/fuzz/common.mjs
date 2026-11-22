import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

export const reference = (refDir, path) => import(pathToFileURL(join(refDir, path)).href);
export const readCases = (refDir) =>
  readFileSync(join(refDir, 'test/conformance/corpus.jsonl'), 'utf8').split('\n').slice(1).filter(Boolean);
export const sha = (data) => createHash('sha256').update(data).digest('hex').slice(0, 16);
export const isRecord = (value) => value !== null && typeof value === 'object' && !Array.isArray(value);
