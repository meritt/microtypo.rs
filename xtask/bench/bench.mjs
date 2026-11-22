import { readFileSync } from 'node:fs';
import { reference } from '../fuzz/common.mjs';

const ROUNDS = 3;

const [refDir, pointsPath] = process.argv.slice(2);
const { MicroTypo } = await reference(refDir, 'src/index.js');
const points = JSON.parse(readFileSync(pointsPath, 'utf8'));
let sink;

function measure(op) {
  const warm = performance.now();
  let runs = 0;

  while (runs < 3 || performance.now() - warm < 100) {
    op();
    runs++;
  }

  const per = (performance.now() - warm) / runs;
  const batch = Math.max(1, Math.ceil(2 / per));
  const samples = [];
  const start = performance.now();

  while (samples.length < 10 || (performance.now() - start < 333 && samples.length < 400)) {
    const t = performance.now();

    for (let i = 0; i < batch; i++) {
      op();
    }

    samples.push((performance.now() - t) / batch);
  }

  return samples;
}

const runs = points.map((point) => {
  const config = point.config ?? undefined;

  if (point.op === 'new') {
    return () =>
      measure(() => {
        sink = new MicroTypo(config);
      });
  }

  const engine = new MicroTypo(config);

  return () =>
    measure(() => {
      for (const text of point.texts) {
        sink = engine.process(text);
      }
    }).map((ms) => ms / point.texts.length);
});
const samples = points.map(() => []);

for (let round = 0; round < ROUNDS; round++) {
  for (const [i, run] of runs.entries()) {
    samples[i].push(...run());
  }
}

const out = Object.fromEntries(points.map((point, i) => [point.name, samples[i]]));

process.stdout.write(`${JSON.stringify(out)}\n`);
process.exitCode = sink === undefined ? 1 : 0;
