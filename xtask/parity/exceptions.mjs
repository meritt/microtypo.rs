const IN_CODE = 'the core tests the character or line in code instead of a regular expression';
const CONFIGURED = 'built at run time from';
const UNRUN = 'the reference compiles it too but does not run it on its corpus, and the core lists every built-in it compiles';

export const RULES = [
  {
    rule: 'quote.close',
    what: 'pattern 1 replacement',
    reference: 'function',
    core: 'string "\\\\»"',
    reason: 'the function returns a constant holding no `$`, which the template substitutes to the same text'
  }
];

const reference = (key, reason) => ({ key, side: 'reference', reason });
const core = (key, reason) => ({ key, side: 'core', reason });

const specialSpellings = `
&thinsp; &hellip; &frac12; &frac14; &frac34; &plusmn; &laquo; &#8223; &bdquo; &#8222; &ldquo;
&#8220; &raquo; &rdquo; &#8221; &#8239; &#8201; &ndash; &#8211; &mdash; &#8212; &minus; &#8722;
&equiv; &#8801; &Prime; &#8243; &#8230; &#8800; &#8804; &#8805; &#8773; &trade; &#8482; &#8364;
&#8381; &lsquo; &#8216; &rsquo; &#8217; &times; &#8470; &#8592; &#8594; &quot; &#171; &#187; &nbsp;
&#160; &#151; &#133; &cong; &#189; &#188; &#190; &#177; &#153; &#174; &copy; &#169; &sect; &#167;
&euro; &#769; &#215; &#176; &larr; &rarr; &sup2; &#178; &sup3; &#179; &#34; &reg; &deg; &ne; &le;
&ge; ‟ ” ‐ ‒
`.trim().split(/\s+/u);
const specialSource = [String.raw`&#(?:[xX]([0-9A-Fa-f]+)|([0-9]+));`, ...specialSpellings.map(RegExp.escape)].join('|');

export const EXPRESSIONS = [
  ...['gu', 'uy'].map((flags) => reference(
    `/${specialSource}/${flags}`,
    'clear-special.js SCAN_RE and SCALAR_RE: the core reads numeric references first, then the longest CHARS_TABLE spelling at the same UTF-16 offset'
  )),
  ...['/(amber)/gi', '/(b)(b)/g', '/Amber/g', '/a/g', '/b/g'].map((key) => reference(key, 'J9 JSON data-rule corpus: compiled from each descriptor through the same runtime Re/regress parser, so these application patterns are outside the builtin registry')),
  reference('/[⅓⅔⅛⅜⅝⅞′↔⇒⇔]/gu', 'lib/glyphs.js optional glyph encode check: the core tests this same active glyph set with direct UTF-16 membership'),
  reference('/\\$(\\$|&|`|\'|[0-9]{1,2}|<[^>]*>)/g', 'rule-runner.js expands data-rule replacement templates for trace: the core uses the existing UTF-16 ECMAScript replacement-template scanner for the same tokens'),
  reference('/(?:import|export)(?=[ \\t{*\'"\\r\\n]|$)/y', 'input/mdx.js ESM opener: the core compares the same two ASCII words and exact following delimiter at the current line start'),
  reference('/[A-Za-z/]/', 'protect/blocks.js JSX candidate: the core tests the same ASCII letter or slash at the candidate opener'),
  reference('/[A-Za-z0-9_$]/u', 'input/mdx.js expression word continuation: the core tests the same ASCII units'),
  reference('/[A-Za-z0-9_.:-]/u', 'input/mdx.js JSX name continuation: the core tests the same ASCII units'),
  reference('/[A-Za-z]/u', 'input/mdx.js JSX name start and regexp flags: the core tests the same ASCII letter ranges'),
  reference('/[A-Za-z_$]/u', 'input/mdx.js expression word start: the core tests the same ASCII units'),
  reference('/\\{|<(?:\\/?[A-Za-z]|\\/?>)/g', 'input/mdx.js candidate search: the core scans for the same brace or JSX opener before reading the selected candidate'),
  reference('/[A-Z]/g', 'input/attributes.js asciiLower: the core folds only the same ASCII capital range'),
  reference('/[\\t\\n\\r\\f "\'`=<>]/g', 'input/attributes.js unquoted serialization: the core escapes exactly this unit set to decimal references'),
  reference('/^<([A-Za-z][A-Za-z0-9:-]*)/u', 'input/attributes.js tag name: the core scans the same anchored ASCII name language inside each real tag span'),
  reference('/\\r(?!\\n)/', 'input/toml.js bare CR rejection: the core reports the first CR not followed by LF with the same offset and input error'),
  reference('/<<tag>(?=[ \\t\\n\\r\\f/>])/g', 'protect/blocks.js case-sensitive registered opener; configured from the same validated name'),
  reference('/<<tag>(?=[ \\t\\n\\r\\f/>])/iy', 'protect/blocks.js selected case-insensitive opener: the core anchors the same configured name expression on the UTF-16 candidate slice, preventing suffix search'),
  reference('/<<tag>(?=[ \\t\\n\\r\\f/>])/y', 'protect/blocks.js selected case-sensitive opener: the core anchors the same configured name expression on the UTF-16 candidate slice, preventing suffix search'),
  reference('/<\\/<tag>(?=[ \\t\\r\\n\\f/>])/g', 'protect/blocks.js case-sensitive closer name; the complete real closing tag endpoint comes from tag-spans'),
  reference('/<\\/<tag>(?=[ \\t\\r\\n\\f/>])/gi', 'protect/blocks.js case-insensitive closer name; the complete real closing tag endpoint comes from tag-spans'),
  reference('/<\\/<tag>(?=[ \\t\\r\\n\\f/>])/iy', 'protect/blocks.js selected closer: the core anchors the same case-insensitive name expression on the UTF-16 candidate slice and validates the complete tag endpoint'),
  reference('/<\\/<tag>(?=[ \\t\\r\\n\\f/>])/y', 'protect/blocks.js selected closer: the core anchors the same case-sensitive name expression on the UTF-16 candidate slice and validates the complete tag endpoint'),
  reference('/[gy]/g', 'rule-runner.js removes stateful flags from each requiresPattern: the core declares every requiresPattern without them'),
  reference('/[\\n*_~`]/', 'inline-lift.js J4 fast link-path eligibility: the native engine keeps its existing general Markdown lifting path with the same output'),
  reference('/(\\r\\n?|\\n)/uy', 'lib/strings.js trimBlankLines split: the core scans the same physical CRLF, CR or LF boundaries, treats CRLF atomically, and applies the same blank predicate only to complete edge lines'),
  reference('/\\r\\n?|\\n/uy', 'input/frontmatter.js readsAsYamlHeader split: the core reads the header lines at the same CRLF, CR or LF boundaries with the line reader of splitFrontmatter'),
  reference('/^(?:[ \\t]*(?:\\r\\n?|\\n))+/u','input/frontmatter.js body boundary: the core scans the same anchored sequence of space/tab-only physical lines and returns before the indentation of the first nonblank line'),
  reference('/[\\p{L}\\p{N}_]+/uy', 'input/localization.js ARGUMENT: the core anchors the expression on a slice beginning after an ASCII brace, comma or whitespace; neither boundary can split a surrogate pair, and the expression has no context-sensitive assertion'),
  core('/^[\\p{L}\\p{N}_]+/u', 'input/localization.js ARGUMENT: the reference runs the same character language sticky at the slice start'),
  reference('/[+-]?(?:\\d+(?:\\.\\d*)?|\\.\\d+)/y', 'input/localization.js NUMBER: the core anchors this ASCII-only expression at the offset value start, with no assertions or captures depending on surrounding text'),
  core('/^[+-]?(?:\\d+(?:\\.\\d*)?|\\.\\d+)/', 'input/localization.js NUMBER: the reference runs this expression sticky at the same position'),
  reference('/[*_~]/', 'inline-lift.js Markdown emphasis precheck: ' + IN_CODE),
  reference('/[*_~]/g', 'inline-lift.js Markdown emphasis marker search: ' + IN_CODE),
  reference('/^[A-Za-z]+$/u', 'protect/vault.js restore-zero fast-path eligibility: the core keeps its general restore callback for every static prefix'),
  reference('/\u{E000}B0+\u{E001}/g', 'protect/vault.js B single-string entry fast path: the core general numeric-id callback restores exactly the same all-zero ids'),
  reference('/\u{E000}E0+\u{E001}/g', 'protect/vault.js E single-string entry fast path: the core general numeric-id callback restores exactly the same all-zero ids'),
  reference('/\u{E000}F0+\u{E001}/g', 'protect/vault.js F single-string entry fast path: the core general numeric-id callback restores exactly the same all-zero ids'),
  reference('/\u{E000}H0+\u{E001}/g', 'protect/vault.js H single-string entry fast path: the core general numeric-id callback restores exactly the same all-zero ids'),
  reference('/\u{E000}I0+\u{E001}/g', 'protect/vault.js I single-string entry fast path: the core general numeric-id callback restores exactly the same all-zero ids'),
  reference('/\u{E000}N0+\u{E001}/g', 'protect/vault.js N single-string entry fast path: the core general numeric-id callback restores exactly the same all-zero ids'),
  reference('/\u{E000}W0+\u{E001}/g', 'protect/vault.js W single-string entry fast path: the core general numeric-id callback restores exactly the same all-zero ids'),
  core('/\u{E000}E(\\d+)\u{E001}/g', 'protect/vault.js E general restore remains compiled; this reference corpus takes only its single-entry all-zero fast path'),
  core('/\u{E000}F(\\d+)\u{E001}/g', 'protect/vault.js F general restore remains compiled; this reference corpus takes only its single-entry all-zero fast path'),
  reference('/&amp;/gu', 'inline-lift.js AMP_RE: the core replaces the literal entity in code'),
  reference('/[*_~[`]/', `inline-lift.js MARKER_RE: ${IN_CODE}`),
  reference('/[*_~\\]]/g', `inline-lift.js Markdown link markers: ${IN_CODE}`),
  reference('/[A-Za-z0-9]/u', `lib/inline-boundaries.js nativeTagStart placeholder name: ${IN_CODE}`),
  reference('/[\\d.,]/u', `lib/measurement-units.js measurementBeforeParticle number start: ${IN_CODE}`),
  reference('/[«»„“]/gu', `rules/quote-state.js QUOTE_MARK_RE: ${IN_CODE}`),
  reference('/^[ \\t]*$/', `rules/text.js BLANK_RE: ${IN_CODE}`),
  reference('/[ \\t]+/g', 'rules/number.js GAP_RE: the core replaces each run of spaces and tabs in code'),
  reference('/(?:[ \\t\u{A0}]|&#160\u{E000}N\\d{1,9}\u{E001};)+/uy', 'rules/dash.js GAP_AFTER_RE: the core anchors the expression at the starting position'),
  core('/^(?:[ \\t\u{A0}]|&#160\u{E000}N\\d{1,9}\u{E001};)+/u', 'rules/dash.js GAP_AFTER_RE: the reference runs it sticky at the starting position'),
  reference('/[A-ZА-ЯЁ]{2}(?=[\\s;.?!:("»“]|$)/uy', 'rules/nobr.js ACRONYM_AHEAD_RE: the core anchors the expression at the starting position'),
  core('/^[A-ZА-ЯЁ]{2}(?=[\\s;.?!:("»“]|$)/u', 'rules/nobr.js ACRONYM_AHEAD_RE: the reference runs it sticky at the starting position'),
  reference('/[\\p{L}\\p{N}]/uy', 'rules/nobr.js WORD_START_RE: the core anchors the expression at the starting position'),
  core('/^[\\p{L}\\p{N}]/u', 'rules/nobr.js WORD_START_RE: the reference runs it sticky at the starting position'),
  reference('/[a-zа-яё]{1,2}[ \\t]+([a-zа-яё]{1,2}[ \\t]+|и\u{A0}(?=[дп]р\\.))?(?!-+(?![a-zа-яё0-9-]))([a-zа-яё0-9-]{2,}|[0-9½¼¾⅓⅔⅛⅜⅝⅞])/iuy', 'rules/nobr.js SHORT_WORD_AHEAD_RE: the core anchors the expression at the starting position'),
  core('/^[a-zа-яё]{1,2}[ \\t]+([a-zа-яё]{1,2}[ \\t]+|и\u{A0}(?=[дп]р\\.))?(?!-+(?![a-zа-яё0-9-]))([a-zа-яё0-9-]{2,}|[0-9½¼¾⅓⅔⅛⅜⅝⅞])/iu', 'rules/nobr.js SHORT_WORD_AHEAD_RE: the reference runs it sticky at the starting position'),
  reference('/ /', `input/yaml.js plain scalar check: ${IN_CODE}`),
  reference('/^[-+.0-9]/', `input/yaml.js plain scalar check: ${IN_CODE}`),
  reference('/^[-?:,[\\]{}#&*!|>\'"%@`]/', `input/yaml.js PLAIN_INDICATOR: ${IN_CODE}`),
  reference('/[,[\\]{}]/', `input/yaml.js FLOW_BREAKS: ${IN_CODE}`),
  reference('/^[ \\t]+/', `input/yaml.js quoted line indent: ${IN_CODE}`),
  reference('/\\r$/', `input/yaml.js quoted line end: ${IN_CODE}`),
  reference('/[A-Za-z0-9_-]/', `input/toml.js BARE_KEY_CHAR: ${IN_CODE}`),
  reference('/[^\\s/>]/', `input/xml.js NAME_BODY_RE: ${IN_CODE}`),
  reference('/[^\\s=/<>]/', `input/xml.js ATTR_NAME_RE: ${IN_CODE}`),
  reference('/[\\t\\n\\r]/g', `input/xml.js attribute whitespace: ${IN_CODE}`),
  reference('/[a-zA-Z0-9-]/', `input/markdown.js tag name scan: ${IN_CODE}`),
  reference('/^0*1$/', `input/markdown.js ordered list start: ${IN_CODE}`),
  reference('/\\r\\n?|\\n/g', `input/frontmatter.js LINE_BREAK_RE: ${IN_CODE}`),
  reference('/[.:@-]/u', `protect/sequences.js SEQUENCE_HINT_RE: ${IN_CODE}`),
  reference('/^\\(\\?<[^=!]/', `protect/blocks.js NAMED_GROUP_OPEN_RE: ${IN_CODE}`),
  reference('/[\u{E000}\u{E001}\u{E100}\u{E101}]/u', `protect/reserved.js RESERVED_TEST_RE: ${IN_CODE}`),
  reference('/[\\r\\n]/', `rules/text.js line break test: ${IN_CODE}`),
  reference('/[\\r\\n]/u', `rules/text.js hasLineBreak: ${IN_CODE}`),
  reference('/\\S/', `rules/text.js blank paragraph test: ${IN_CODE}`),
  reference('/^[0-9a-fA-F]+$/', `lib/escape-decoder.js hex digits: ${IN_CODE}`),
  reference('/^[a-zA-Z][a-zA-Z0-9-]*$/', `lib/tag-name.js TAG_NAME_RE: ${IN_CODE}`),
  reference('/&/g', `protect/tags.js escapeAttr: ${IN_CODE}`),
  reference('/"/g', `protect/tags.js escapeAttr: ${IN_CODE}`),
  reference('/</g', `protect/tags.js escapeAttr: ${IN_CODE}`),
  reference('/>/g', `protect/tags.js escapeAttr: ${IN_CODE}`),
  reference(
    '/[\u{A0}\u{202F}\u{2009}\u{AB}\u{BB}\u{201E}\u{201C}\u{2018}\u{2019}\u{2033}\u{2014}\u{2013}\u{2212}\u{2026}\u{A9}\u{AE}\u{2122}\u{A7}\u{20AC}\u{20BD}\u{B0}\u{D7}\u{B1}\u{2116}\u{2260}\u{2264}\u{2265}\u{2245}\u{2261}\u{2190}\u{2192}\u{B2}\u{B3}\u{BD}\u{BC}\u{BE}\u{301}]/gu',
    'lib/glyphs.js GLYPH_RE: the core finds the glyphs of GLYPH_TO_ENTITY in one scan over the text'
  ),
  reference('/^<selector>$/i', `core/options.js wildcardToRegex: ${CONFIGURED} a rule selector of the configuration`),
  reference('/(<open>)([\\s\\S]*?)(<close>)/g', `protect/blocks.js: ${CONFIGURED} the delimiters of a safe block`),
  reference('/<<tag>(?=[ \\t\\n\\r\\f/>])/gi', `protect/blocks.js: ${CONFIGURED} a safe tag name`),
  reference('/<\\/<tag>[ \\t\\r\\n]*>/gi', `protect/blocks.js: ${CONFIGURED} a safe tag name, for XML`),
  reference('/[<substitutes>]/gu', `protect/reserved.js: ${CONFIGURED} the reserved characters of the input`),
  reference(
    '/((?:<\\/?(?:<placeholders>)>|\u{E100}POP\u{E101}|\u{E100}PCL\u{E101}))[ \\t]*[\\r\\n][\\r\\n \\t]*/g',
    `rules/text.js: ${CONFIGURED} the placeholders of the block tags of the input`
  ),
  reference(
    '/(?<![ \\t])[ \\t]*[\\r\\n][\\r\\n \\t]*(?=(?:<\\/?(?:<placeholders>)>|\u{E100}POP\u{E101}|\u{E100}PCL\u{E101}))/g',
    `rules/text.js: ${CONFIGURED} the placeholders of the block tags of the input`
  ),
  reference(
    '/(?<!<(?:<placeholders>)>)\\n/g',
    `rules/text.js: ${CONFIGURED} the placeholders of the block tags of the input`
  ),
  reference(
    '/(^|[>\\s,\\-\u{E001}−—–(\\[{«„])(?<![\\p{L}\\d][-\u{2010}\u{2011}])([a-z\u{430}-\u{44F}\u{451}]+)\u{A0}(\\x3c\u{E000}T#\u{E001}\\x3e)/giu',
    `rules/etc.js nobr patterns: ${CONFIGURED} the placeholders of the nowrap span`
  ),
  reference(
    '/(\\x3c\\/\u{E000}T#\u{E001}\\x3e)\u{A0}([a-z\u{430}-\u{44F}\u{451}]+)(?![-\u{2010}\u{2011}\u{BB}\u{201C}][\\p{L}\\d])($|[^a-z\u{430}-\u{44F}\u{451}])/giu',
    `rules/etc.js nobr patterns: ${CONFIGURED} the placeholders of the nowrap span`
  ),
  reference(
    '/<(?:(pre|script|style|textarea)(?=[\\s>]|$)|(!--)|\\/?([A-Za-z][A-Za-z0-9-]*))/iy',
    'input/markdown.js runs it sticky at a position; the core matches the text from that position with the expression anchored by ^'
  ),
  core(
    '/^<(?:(pre|script|style|textarea)(?=[\\s>]|$)|(!--)|\\/?([A-Za-z][A-Za-z0-9-]*))/i',
    'the sticky expression of input/markdown.js, anchored by ^ at the position it runs from'
  ),
  reference(
    '/(?:<([A-Za-z][A-Za-z0-9-]*)(?:[ \\t]+[A-Za-z_:][\\w.:-]*(?:[ \\t]*=[ \\t]*(?:[^ \\t\\n\\v\\f\\r"\'=<>`]+|\'[^\'\\n\\r]*\'|"[^"\\n\\r]*"))?)*[ \\t]*\\/?|<\\/([A-Za-z][A-Za-z0-9-]*)[ \\t]*)>[ \\t]*$/my',
    'input/markdown.js runs it sticky and multiline at a position; the core matches the text from that position with the expression anchored by ^ and the multiline $ spelled out'
  ),
  core(
    '/^(?:<([A-Za-z][A-Za-z0-9-]*)(?:[ \\t]+[A-Za-z_:][\\w.:-]*(?:[ \\t]*=[ \\t]*(?:[^ \\t\\n\\v\\f\\r"\'=<>`]+|\'[^\'\\n\\r]*\'|"[^"\\n\\r]*"))?)*[ \\t]*\\/?|<\\/([A-Za-z][A-Za-z0-9-]*)[ \\t]*)>[ \\t]*(?:$|(?=[\\n\\r\\u2028\\u2029]))/',
    'the sticky multiline expression of input/markdown.js, anchored by ^ at the position it runs from, with the multiline $ spelled out'
  ),
  reference('/[ \\t\\r\\n]/u', 'rules/quote-state.js SEPARATOR_RE: ' + IN_CODE),
  reference('/["«»„“”]/gu', 'rules/quote-state.js QUOTE_MARK_AHEAD_RE: ' + IN_CODE),
  reference('/[,;:*_~`\u{E001}\\s([«—–\\->]/u', 'rules/quote-state.js OPENER_LEFT_RE: ' + IN_CODE),
  reference('/[„“”]/u', 'rules/quote-state.js HIGH_QUOTE_RE: ' + IN_CODE),
  reference('/[\\s([«—–\\->]/u', 'lib/boundaries.js STRONG_LEFT_RE: ' + IN_CODE),
  reference('/[,;:*_~`\u{E001}]/u', 'lib/boundaries.js WEAK_LEFT_RE: ' + IN_CODE),
  reference('/[A-Za-z!?]/', 'lib/tag-name.js HTML_NAME_START_RE: ' + IN_CODE),
  reference('/[-‐‑]/u', 'rules/etc.js HYPHEN beside a nowrap tag: ' + IN_CODE),
  reference('/[А-ЯЁа-яё]/u', 'rules/nobr.js RUSSIAN_LETTER_RE: ' + IN_CODE),
  reference('/[a-zA-Z\\d_<>]/u', 'rules/number.js DIMENSION_IDENTIFIER: ' + IN_CODE),
  reference('/[-—−–]/u', 'inline-lift.js dash precheck of a lifted run and of spaced year ranges: ' + IN_CODE),
  reference('/[^ \\t]/u', 'inline-lift.js spacedYearRanges gap collapsed by space.collapse_spaces: ' + IN_CODE),
  reference('/[^ \\t\u{A0}]/u', 'inline-lift.js spacedYearRanges gap bound by nobr.nbsp_after_number: ' + IN_CODE),
  reference('/[«»„“”&]/u', 'protect/sequences.js quoted address cut: ' + IN_CODE),
  reference('/[»“”&]/u', 'protect/sequences.js quoted address cut, the unit after it: ' + IN_CODE),
  reference('/[^\\r\\n]+/gu', 'input/markdown.js blanks a span to spaces keeping its line endings: ' + IN_CODE),
  reference('/\\r\\n|[\\r\\n]/gu', 'input/markdown.js line endings of a visible span: ' + IN_CODE),
  reference('/^[ \\t]+$/u', 'protect/blocks.js blank raw text after a token: ' + IN_CODE),
  reference('/^[ \\t]+/u', 'rules/text.js trims the ASCII indent of a paragraph: ' + IN_CODE),
  reference('/[ \\t]/u', 'rules/quote.js closesLast space after the closer: ' + IN_CODE),
  reference('/^[!?]/u', 'rules/quote.js nodeBetween comment, CDATA, instruction or declaration raw: ' + IN_CODE),
  reference('/^[!?]|\\/$/u', 'rules/quote.js closeAtBoundary whole-node raw before the quote: ' + IN_CODE),
  reference('/^[.,;:!?…]+$/u', 'rules/quote.js PUNCTUATION_GAP_RE between the closers of a run: ' + IN_CODE),
  reference('/^[^\\s/>]+/u', 'rules/quote.js endsBefore element name of the raw: ' + IN_CODE),
  ...['/(?<=http:\\/\\/|<\u{E000}A\\d+\u{E001}>)/y', '/NUM\\d+ID/y', '/NUM\\d+ID@simonenko\\.xyz/y', '/TOK\\d+TK/y'].map((key) => reference(key, 'protect/sequences.js census reads the lead and tail around the SAFE and nonce of a placeholder with these sticky expressions: the core compares the same units in code')),
  reference('/\\+|тел|[-−]\\d{2}[-−]\\d{2}/iu', 'rules/number.js PHONE_MARK_RE skips phoneContextAt for speed where no phone pattern can match: the core runs the phone patterns themselves'),
  core('/\u{E000}A(\\d+)\u{E001}/g', `protect/vault.js restore expression of the A vault: ${UNRUN}`),
  core('/\u{E000}L(\\d+)\u{E001}/g', `protect/vault.js restore expression of the L vault: ${UNRUN}`),
  core('/\u{E000}T(\\d+)\u{E001}/g', `protect/vault.js restore expression of the T vault: ${UNRUN}`)
];
