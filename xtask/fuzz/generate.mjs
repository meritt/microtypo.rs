import { isRecord } from './common.mjs';

export function rng(seed) {
  let a = seed >>> 0;

  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export function caseSeed(seed, index) {
  let h = (seed ^ Math.imul(index + 1, 0x9e3779b1)) >>> 0;
  h = Math.imul(h ^ (h >>> 16), 0x85ebca6b);
  h = Math.imul(h ^ (h >>> 13), 0xc2b2ae35);
  return (h ^ (h >>> 16)) >>> 0;
}

const ADDRESSES = [
  'http://example.com/a-b',
  'https://amber.ru/search?q=a-b&x=1',
  'https://sub.domain.co.uk/wiki/Амбер',
  'http://localhost:8080/?a=1&amp;b=2',
  'ftp://127.0.0.1/',
  'www.example.com/~user/',
  'amber.ru/a-b',
  'https://arden.technology/docs/v1.2/api#section-1'
];
const EMAILS = ['oberon@amber.ru', 'corwin.amber@example.com', 'r+tag@rebma.co.uk', 'fiona_x@почта.рф', 'info@mail.example.org'];
const GLUE = ["'", '`', '"', '|', '(', ')', ',', ';', ':', '/', "'`"];

const POOLS = {
  glued: ADDRESSES.flatMap((a) => EMAILS.flatMap((e) => GLUE.flatMap((g) => [a + g + e, e + g + a]))),
  quotes: ['"', "'", '«', '»', '„', '“', '”', '‘', '’', '`', '""', '"\'', "''", '« ', ' »'],
  dashes: ['-', '--', '---', '–', '—', '−', '‐', ' - ', ' -- ', ' — ', ' – ', '-\n', '- '],
  numbers: [
    '1', '12', '2024', '1990-2000', '1990–2000', '3,14', '0.5', '10 000', '1000000', '1/2', '1/4',
    '+7', '-5', '5%', '100$', '$100', '19:30', '17.02.1983', 'XXI', 'XIX в.', '2x2', '10x20', '1-2',
    '5 руб.', '3 кг', '№ 5', '§ 2', '1990-е', '25°C', '(c)', '(r)', '(tm)', '+-', '<-', '->', '<=',
    '>=', '!=', '...', '..', '?!', '!!!', ',,', '1 января 2009', '12 %', '50 м²', '3 ч', 'v1.2.3'
  ],
  abbr: [
    'т. е.', 'т.е.', 'т. д.', 'и т. п.', 'и т.д.', 'г.', 'гг.', 'в.', 'вв.', 'руб.', 'коп.', 'тыс.',
    'млн', 'млрд', 'им.', 'ул.', 'д.', 'кв.', 'стр.', 'см.', 'рис.', 'ООО', 'ЗАО', 'А. С. Пушкин',
    'И. И. Иванов', 'т. к.', 'др.', 'пр.', 'с.', 'км', 'кг', 'Mr.', 'Dr.', 'etc.', 'г. Москва',
    'пос.', 'обл.', 'долл.', 'евро', 'чел.', 'мин.', 'т. н.', 'н. э.', 'до н. э.', 'г-н ', 'г-жа '
  ],
  spaces: [
    ' ', '  ', '   ', '\t', '\n', '\n\n', '\r\n', '\r', ' ', ' ', ' ', ' ',
    '​', '­', '﻿', ' ', '　', ' \n', '\n \n'
  ],
  tags: [
    '<b>', '</b>', '<br>', '<br/>', '<p>', '</p>', '<code>', '</code>', '<pre>', '</pre>',
    '<a href="http://x.ru/a-b">', '</a>', '<!-- "x" - y -->', '<!--', '-->', '<script>', '</script>',
    '<style>', '</style>', '<nobr>', '<span class="x">', '</span>', '<img alt="a - b">', '<', '>',
    '</', '<?php echo "a - b"; ?>', '<![CDATA[', ']]>', '<kbd>', '<math>', '<textarea>', '<div>',
    '</div>', '<em>', '</em>', '<sup>', '<sub>', '<x-y>', '<a>', '<!DOCTYPE html>', '`', '```',
    '**', '_', '[', ']', '(', ')', '{', '}', '{{', '}}', '{%', '%}', '<%', '%>', '{#', '#}', '# ',
    '> ', '- ', '* ', '1. ', '|', '\\', ':::', '---', '+++', 'http://example.com/a--b',
    'https://ya.ru/?q="a"', 'user@example.com', 'www.example.com', 'example.com/a-b', '[a](b)',
    '![a](b)', '[a]: http://x', '<http://x.ru>', '    ', ': ', '= ', '"key": ', '- "a"'
  ],
  entities: [
    '&nbsp;', '&mdash;', '&ndash;', '&laquo;', '&raquo;', '&quot;', '&#34;', '&#x22;', '&#034;',
    '&amp;', '&lt;', '&gt;', '&hellip;', '&#8212;', '&#xA0;', '&copy;', '&', '&#', '&amp;nbsp;',
    '&nbsp', '&#x;', '&bogus;', '&shy;', '&thinsp;', '&#0;', '&#xD800;', '&#1114112;', '&apos;'
  ],
  reserved: [
    '', '', '', '', '', 'T0', 'A12',
    'POP', 'PCL', 'BR', 'L1', '', ''
  ],
  astral: ['😀', '𝔸', '👍🏽', '🇷🇺', '𠀋', 'á', '\u{10ffff}', '💩"'],
  lone: ['\ud800', '\udfff', '\udc00', '\ud83d', 'a\ud800b'],
  triggers: [
    ' 12 - 19 ', ' 5 – 7 ', '!!', '!!!!', '???', '?…', '!…', '!,!', '?;?', '.».', ' (Корвин,) ',
    ' (Рэбма;) ', ' (слово : )', ' (тени ... )', ' (слово …)', ' 08 : 59 ', ' 9 :30 ', ' 12:30:45 ',
    ' x_1 ', ' H_2 ', ' amber_2 ', ' a^2 ', ' м^3 ', ' ^2 ', ' ^st ', ' ½ кг', ' ¼ м', ' ¾ т', ' 5 л ',
    ' 200 мл ', ' 7 ГБ ', ' 100 Мбит ', ' 5 ТБ/с ', ' 100 МГц ', ' 2.4 ГГц ', ' 16 px ', ' 1.5 em ',
    ' г-н ', ' г-жа ', ' 192.168.0.1 ', ' 1 января-5 февраля ', ' 1990г. ', ' 2024 г. ', 'ста`рый',
    'ру`чка', ' XIV-XVI ', ' XIX-XX вв. ', ' V-VI в. ', '<span style="white-space:nowrap;">и т. д.</span>',
    '<span style="white-space:nowrap;"> «Амбер» т. е. </span>', ' до н. э. ', ' и т. д. ', ' т. е. ',
    ' 50 % ', ' 3 x 4 ', ' 1/2 ', ' № 5 ', ' 1-2 ', ' 10-15 лет ', ' XX в. ', ' 25 °C ', ' +7 999 123-45-67 ',
    ' да!. ', ' да.? ', ' (c) (c)( ', '<b>";', '(<b>")', ' слово "  b " x', '( " \nb " x', '<p><div>x</div></p>', '<p><hr></p>',
    'a<br>\nb\nc', ' слово " (скобка)', '<div><em>a</em> <code>b</cod>', ` x» ${'«a» '.repeat(17)}`, ` ${'"a" '.repeat(17)}x" `,
    '<b>в</b> 1812-1814 гг. ', '<i>с</i> 1812—1814 гг. ', 'между <b>1990</b>-2000 гг. ',
    '<b>начала</b> 1900—1910 г. ', 'в <u>1812</u>-1814 гг ', '<i>в</i> 1812−1814 гг. ',
    '«Диагональ 2 ′8 “ у модели «Тайга», высота 1 \'7”» ', '«Ширина 2 ′8,5 “ у рамы «Сосна», высота 1 ′7,5”» ', '<span style="white-space:nowrap;">и (</span>20 °C) '
  ],
  words: [
    'а', 'и', 'в', 'с', 'к', 'не', 'же', 'ли', 'бы', 'что', 'Москва', 'слово', 'кто-то', 'из-за',
    'по-русски', 'Амбер', 'x', 'API', 'тест', 'Иванов', 'Оберон', 'привет', 'ВОТ', 'а-а-а', 'Hello',
    'world'
  ]
};

const POOL_WEIGHTS = [
  ['quotes', 5],
  ['dashes', 5],
  ['numbers', 4],
  ['abbr', 3],
  ['spaces', 4],
  ['tags', 4],
  ['entities', 2],
  ['reserved', 1],
  ['astral', 1],
  ['lone', 1],
  ['triggers', 6],
  ['words', 4],
  ['glued', 2]
];

const STAGED = [
  [' 1990г. ', { rules: { 'abbr.nbsp_weight_unit': false } }],
  [' «2024 г.» ', { rules: { 'abbr.nbsp_weight_unit': false } }],
  [' в 2024 г. и ', { rules: { 'abbr.nbsp_weight_unit': false } }],
  [' 1990г. ', { rules: { abbr: false } }],
  [' что?? ', { rules: { 'punctuation.repeat_limit': false } }],
  [' А.С Пушкин ', { rules: { 'nobr.initials_dots': true } }],
  [' Слово хлово ', { html: true, rules: { 'text.deduplicate_words': true } }],
  [' a ~= b ', { rules: { 'number.math': true } }],
  ['\n(скобка) ', { html: true, rules: { 'hanging.bracket': true } }],
  [' 190 000 г. Москва ', { rules: {} }],
  ['<span style="white-space:nowrap;">а </span>=> ', { html: true, rules: { 'symbol.arrows_extended': true } }],
  ['<span style="white-space:nowrap;">а </span>с пр ', { html: true, rules: {} }],
  ['<span style="white-space:nowrap;">от +</span>3 °C ', { html: true, rules: {} }]
];

const FOCUSED = [
  ['Amber', { html: true, ruleGroups: [{ name: 'custom', rules: [{ id: 'amber', pattern: { source: 'Amber', flags: 'g' }, replacement: 'Shadow' }] }] }],
  ['<em>é</em> слово', { input: 'html', html: false }],
  ['© (c)', {}],
  ['<a href="http://example.com/?x=1&amp;y=2">example.com/?x=1&amp;y=2</a>', { input: 'html', html: true }],
  ['<b>слово</b>«: текст', { input: 'html', html: false }],
  ['http://example.com/&quot;x', { html: true, render: { paragraphs: false } }],
  ['http://example.com/&lt;x', { html: true, render: { paragraphs: false } }],
  ['http://example.com/&gt;x', { html: true, render: { paragraphs: false } }],
  ['<img alt=два=слова>', { input: { format: 'html', attributes: ['alt'] } }],
  ['<notg></notg><notg> </notg>', {}],
  ['<script>$', {}],
  ['<code>$', {}],
  ['<span style="white-space:nowrap;">а:</span>+/-5', { html: true }],
  ['<doc>12 345&#160;678</doc>', { input: 'xml' }],
  ['В 1990г. НПО Амбер выпустило 200 экз. массой 2 кг.', { rules: { 'abbr.nbsp_weight_unit': false, 'nobr.nbsp_after_number': false } }]
];

const ALPHABET = ['a', 'x', 'я', 'ю', 'b', ' ', ' ', '"', "'", '«', '»', '-', '—', '1', '.', ',', '\n'];

const TEMPLATES = ['handlebars', 'mustache', 'twig', 'jinja', 'nunjucks', 'liquid', 'erb'];
const TEMPLATE_BITS = [
  '{{ name }} - "x"',
  '{% if a %}"b"{% endif %}',
  '<%= "a" - b %>',
  '{# - "c" #}',
  '{{{raw}}} "a"',
  '{% raw %}"a" - b{% endraw %}',
  '{{!-- "x" --}}'
];

const BOOLEANISH = [true, false, true, false, 'on', 'off', 1, 0, '1', 'false'];
const RENDER_AXES = [
  ['paragraphs', 0.5, BOOLEANISH],
  ['autolink', 0.4, BOOLEANISH],
  ['breakline', 0.3, BOOLEANISH],
  ['nowrap', 0.3, ['nobr', 'span']],
  ['hanging', 0.3, ['style', 'class', 'both']],
  ['prefix', 0.2, ['mt-', false, 'x_']]
];
const FIELDS = [['title'], ['body.text'], ['body.*'], ['*'], [], ['body.tags.0'], ['list'], ['n'], ['body']];
const INVALID_CONFIGS = [
  { unknown: 1 },
  { html: 'yes' },
  { render: { hanging: 'x' } },
  { rules: { 'nope.x': true } },
  { input: 'csv' },
  { render: { prefix: 'a b' } },
  { input: { format: 'json', fields: 'title' } },
  { input: { format: 'text', template: 'php' } },
  { maxInputLength: -1 },
  { rules: [] },
  { render: null }
];

const escapeXml = (s) => s.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');
const indent = (s, by) =>
  s
    .split('\n')
    .map((line) => by + line)
    .join('\n');
const firstLine = (s) => s.split('\n')[0];

const STRUCTURED = ['json', 'yaml', 'toml', 'xml', 'frontmatter'];
const SEAMS = [' ', '\n', '\n\n'];

const formatOf = (config) => {
  const input = isRecord(config) ? config.input : undefined;
  return isRecord(input) ? input.format : input;
};

export function createGenerator({ prose, groups, ruleSelectors, seeds, patterns = [] }) {
  const shortProse = prose.filter((p) => p.length < 400);

  return (seed) => {
    if (seed % 100 === 0) {
      const [text, config] = FOCUSED[Math.floor(seed / 100) % FOCUSED.length];

      return { text, config: { ...config }, ops: [] };
    }

    const random = rng(seed);
    const pick = (items) => items[Math.floor(random() * items.length)];
    const chance = (p) => random() < p;
    const int = (lo, hi) => lo + Math.floor(random() * (hi - lo + 1));
    const weighted = (pairs) => {
      let roll = random() * pairs.reduce((sum, [, w]) => sum + w, 0);

      for (const [item, w] of pairs) {
        roll -= w;
        if (roll < 0) {
          return item;
        }
      }

      return pairs[0][0];
    };
    const token = () => pick(POOLS[weighted(POOL_WEIGHTS)]);

    const shapes = {
      text: (a) => a,
      html: (a, b) =>
        pick([a, `<p>${a}</p>\n<pre>${b}</pre>`, `<div><em>${a}</em> <code>${b}</code></div>`, `${a}<br>${b}`]),
      markdown: (a, b, c) =>
        pick([
          `# ${firstLine(a)}\n\n${b}\n\n- ${firstLine(c)}\n\n\`\`\`\ncode - "x" -- 'y'\n\`\`\`\n\n> ${firstLine(a)} \`a - b\` [ссылка](http://example.com/a-b "t")\n`,
          a,
          `${a}\n\n${b}`,
          `* ${firstLine(a)}\n* ${firstLine(b)}\n\n    ${firstLine(c)}\n`,
          `| ${firstLine(a)} | ${firstLine(b)} |\n|---|---|\n| ${firstLine(c)} | x |\n`
        ]),
      json: (a, b, c) =>
        JSON.stringify({ title: a, body: { text: b, tags: [c, 'id-1'] }, n: 1 }, null, pick([0, 2, '\t'])),
      xml: (a, b) =>
        `<?xml version="1.0"?>\n<doc lang="ru"${pick(['', ' xml:space="pre&#115;erve"', ' xml:space="\tpreserve"'])}><title>${escapeXml(a)}</title><!-- "x" - y --><p>${escapeXml(b)}</p></doc>`,
      yaml: (a, b, c) =>
        pick([
          `title: ${JSON.stringify(a)}\nbody: |\n${indent(b, '  ')}\nlist:\n  - '${firstLine(c).replaceAll("'", "''")}'\nn: 1\n`,
          `title: ${firstLine(a)}\ntags: [${firstLine(b)}, ?, ${firstLine(c)}]\nmeta: {a: ${firstLine(c)}, b: x}\n`
        ]),
      toml: (a, b) => `title = ${JSON.stringify(a)}\nn = 1\n\n[body]\ntext = ${JSON.stringify(b)}\n`,
      frontmatter: (a, b) => `---\ntitle: ${JSON.stringify(firstLine(a))}\n---\n\n${b}`
    };

    function window(text) {
      const len = int(1, 240);
      let start = int(0, Math.max(0, text.length - len));

      if (start > 0 && text.charCodeAt(start) >= 0xdc00 && text.charCodeAt(start) <= 0xdfff) {
        start--;
      }

      return text.slice(start, start + len);
    }

    function piece() {
      return weighted([
        [() => window(pick(prose)), 35],
        [() => pick(shortProse), 25],
        [() => Array.from({ length: int(1, 30) }, () => pick(ALPHABET)).join(''), 15],
        [() => Array.from({ length: int(1, 12) }, () => (chance(0.5) ? token() : pick(POOLS.words))).join(pick(['', ' '])), 20],
        [() => pick(prose), 5]
      ])();
    }

    function mutate(text, least = 0) {
      let out = text;

      for (let n = Math.max(least, weighted([[0, 3], [1, 3], [2, 2], [3, 1], [6, 1]])); n > 0; n--) {
        const at = int(0, out.length);
        const len = int(1, 8);

        out = weighted([
          [() => out.slice(0, at) + token() + out.slice(at), 6],
          [() => out.slice(0, at) + out.slice(at + len), 2],
          [() => out.slice(0, at) + out.slice(at, at + len) + out.slice(at), 2],
          [
            () => {
              const t = token();
              const i = out.indexOf(t);
              return i < 0 ? out : out.slice(0, i) + (chance(0.5) ? '' : t + t) + out.slice(i + t.length);
            },
            2
          ]
        ])();
      }

      return out;
    }

    function config(format) {
      const c = {};

      if (chance(0.7)) {
        c.html = pick(BOOLEANISH);
      }
      if (chance(0.6)) {
        c.entities = pick(BOOLEANISH);
      }

      if (format !== 'text' || chance(0.3)) {
        const input = { format };

        if (chance(0.15)) {
          input.template = pick(TEMPLATES);
        }
        if (['json', 'yaml', 'toml'].includes(format) && chance(0.3)) {
          input[pick(['fields', 'exclude'])] = pick(FIELDS);
        }

        c.input = chance(0.3) && Object.keys(input).length === 1 ? format : input;
      }

      if (chance(0.6)) {
        const render = {};

        for (const [key, p, values] of RENDER_AXES) {
          if (chance(p)) {
            render[key] = pick(values);
          }
        }

        c.render = render;
      }

      if (chance(0.7)) {
        const rules = {};

        for (let n = int(0, 4); n > 0; n--) {
          rules[pick(ruleSelectors)] = pick(BOOLEANISH);
        }
        if (chance(0.2)) {
          rules[pick(groups)] = false;
        }

        c.rules = rules;
      }

      if (chance(0.03)) {
        c.presets = false;
      }
      if (chance(0.03)) {
        c.maxInputLength = pick([0, 10, 100]);
      }
      if (chance(0.03)) {
        Object.assign(c, structuredClone(pick(INVALID_CONFIGS)));
      }

      return c;
    }

    function ops() {
      const list = [];

      if (chance(0.15)) {
        list.push({ method: 'applyOptions', args: [{ rules: { [pick(ruleSelectors)]: pick(BOOLEANISH) } }] });
      }
      if (chance(0.03)) {
        list.push({ method: 'applyOptions', args: [pick([{ html: true }, { render: { nowrap: 'span' } }, { bogus: 1 }, null])] });
      }
      if (chance(0.05)) {
        list.push({ method: 'setPrefix', args: [pick(['mt-', '', true, false, 'a b'])] });
      }
      if (chance(0.05)) {
        list.push({ method: 'setLayout', args: [pick(['style', 'class', 'both', 'none'])] });
      }
      if (chance(0.03)) {
        list.push({ method: 'addSafeTag', args: [pick(['kbd', 'var', 'samp', 'a b', ''])] });
      }
      if (chance(0.03)) {
        list.push({
          method: 'addSafeBlock',
          args: [pick([{ id: 'mark', open: '\\[\\[', close: '\\]\\]' }, { id: 'pct', open: '%%', close: '%%' }, { id: 'bad', open: '(', close: ')' }])]
        });
      }

      return list;
    }

    function seeded() {
      const line = patterns.length > 0 ? seeds[pick(pick(patterns))] : pick(seeds);
      const { id, expect, ...c } = JSON.parse(line);

      if (chance(0.5)) {
        if (typeof c.text === 'string') {
          c.text = STRUCTURED.includes(formatOf(c.config))
            ? `${c.text}${pick(SEAMS)}`
            : chance(0.5)
              ? `${c.text}${pick(SEAMS)}${piece()}`
              : `${piece()}${pick(SEAMS)}${c.text}`;
        }
        if (chance(0.2)) {
          c.options = { changes: true };
        }

        return c;
      }

      if (typeof c.text === 'string') {
        c.text = mutate(c.text, 1);

        if (chance(0.15)) {
          c.text = chance(0.5) ? `${c.text} ${piece()}` : `${piece()} ${c.text}`;
        }
      }

      if (chance(0.3)) {
        const base = isRecord(c.config) ? c.config : {};
        const rules = isRecord(base.rules) ? base.rules : {};
        c.config = { ...base, rules: { ...rules, [pick(ruleSelectors)]: pick(BOOLEANISH) } };
      }
      if (chance(0.1)) {
        c.config = { ...(isRecord(c.config) ? c.config : {}), html: pick(BOOLEANISH) };
      }
      if (chance(0.1)) {
        c.options = { changes: true };
      }

      return c;
    }

    if (seeds.length > 0 && chance(0.5)) {
      return seeded();
    }

    const format = chance(0.4) ? 'text' : pick(Object.keys(shapes));
    let text = shapes[format](mutate(piece()), mutate(piece()), mutate(piece()));

    if (chance(0.5)) {
      text = mutate(text);
    }
    if (chance(0.1)) {
      text = `${text}\n${pick(TEMPLATE_BITS)}`;
    }

    const c = chance(0.04) ? { ops: ops(), text } : { config: config(format), ops: ops(), text };

    if (chance(0.065)) {
      const [trigger, { rules, ...rest }] = pick(STAGED);
      const at = int(0, c.text.length);
      const base = c.config ?? {};

      c.text = c.text.slice(0, at) + trigger + c.text.slice(at);
      c.config = { ...base, ...rest, rules: { ...(isRecord(base.rules) ? base.rules : {}), ...rules } };
    }

    if (chance(0.3)) {
      c.options = { changes: true };
    } else if (chance(0.03)) {
      c.options = pick([{ changes: false }, { changes: 1 }, { unknown: true }, {}, null]);
    }

    return c;
  };
}
