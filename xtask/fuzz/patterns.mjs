const PLACEHOLDER = /\u{E000}([A-Za-z]+)[0-9]+\u{E001}/gu;
const PLACEHOLDER_LIST = /\(\?:\u{E000}[A-Za-z]+#\u{E001}(?:\|\u{E000}[A-Za-z]+#\u{E001})*\)/gu;
const SELECTOR = /^\^(?:\\x[0-9a-f]{2}|\[a-z0-9_-\]\*)[^]*\$$/u;
const SAFE_TAG = /^(<(?:\\\/)?)[a-zA-Z][a-zA-Z0-9-]*(\(\?=\[ \\t(?:\\n\\r|\\r\\n)\\f\/>\]\)|\[ \\t\\r\\n(?:\\f)?\]\*>)$/u;
const SAFE_BLOCK = /^\([^]*\)\(\[\\s\\S\]\*\?\)\([^]*\)$/u;
const SUBSTITUTES = /^\[[\u{E000}-\u{F8FF}\u{F0000}-\u{10FFFF}]+\]$/u;

export const UNMATCHABLE = new Map([
  [
    '/[gy]/g',
    'rule-runner.js removes stateful flags from requiresPattern: all 16 default prechecks have only u/iu flags; JSON replay cannot supply RegExp instances for a custom precheck'
  ],
  [
    '/\u{E000}([TALIH])(\\d+)\u{E001}/',
    'INNER_PLACEHOLDER_RE in protect/tags.js: safeTags.protect runs once per fragment and <notg> leaves its placeholder between marker spans'
  ],
  [
    '/(?<![\\p{L}\\d_-])([А-ЯЁа-яё]+|>)[ \\t]+([А-ЯЁа-яё]{1,4})(?=[.!?…»“)\\]}]*[ \\t]*\\n[ \\t]*$)/gu',
    'second end of nobr.nbsp_last_word in rules/nobr.js: engine.js trims the source edges before the rules run (sourceTrim: String.prototype.trim for text and HTML, trimBlankLines for Markdown and front matter, trimEdges for structured values), and no group before nobr leaves a line break at the end'
  ]
]);

export function patternKey(re) {
  const { flags } = re;
  let source = re.source.replace(PLACEHOLDER, '\u{E000}$1#\u{E001}').replace(PLACEHOLDER_LIST, '(?:<placeholders>)');

  if (flags === 'i' && SELECTOR.test(source)) {
    source = '^<selector>$';
  } else if (['g', 'gi', 'iy', 'y'].includes(flags) && SAFE_TAG.test(source)) {
    source = source.replace(SAFE_TAG, '$1<tag>$2');
  } else if (flags === 'g' && SAFE_BLOCK.test(source)) {
    source = '(<open>)([\\s\\S]*?)(<close>)';
  } else if (flags === 'gu' && SUBSTITUTES.test(source)) {
    source = '[<substitutes>]';
  }

  return `/${source}/${flags}`;
}

export function instrument() {
  const exec = RegExp.prototype.exec;
  const keys = new WeakMap();
  let ran = null;
  let hit = null;
  let keying = false;

  RegExp.prototype.exec = function (string) {
    const match = exec.call(this, string);

    if (ran !== null && !keying) {
      let key = keys.get(this);

      if (key === undefined) {
        keying = true;

        try {
          key = patternKey(this);
        } finally {
          keying = false;
        }

        keys.set(this, key);
      }

      ran.add(key);

      if (match !== null) {
        hit.add(key);
      }
    }

    return match;
  };

  return (fn) => {
    ran = new Set();
    hit = new Set();

    try {
      return { value: fn(), ran, hit };
    } finally {
      ran = null;
      hit = null;
    }
  };
}
