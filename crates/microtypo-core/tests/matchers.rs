use std::ops::Range;
use std::path::PathBuf;

use microtypo_conformance::Json;
use microtypo_core::Re;
use microtypo_core::internals::{GLYPH_TO_ENTITY, escape, generated, unicode_to_entities, unicode_to_xml_entities};

type Slots = Vec<Option<Range<usize>>>;
type Found = Option<(Range<usize>, Slots)>;
type Find<'m> = dyn FnMut(usize, &mut [Option<Range<usize>>]) -> Option<Range<usize>> + 'm;

const SHORT: usize = 96;
const WALKS: usize = 128;
const SHORT_SYMBOLS: usize = 10;
const BOUNDARY: usize = 16;
const CORPUS: usize = 160;

const SPECIALS: &[&str] = &[
    "\n",
    "\r",
    " ",
    "\t",
    "\u{a0}",
    "\u{202f}",
    "\u{2009}",
    "\u{2028}",
    "\u{2029}",
    "\u{feff}",
    "\u{17f}",
    "\u{212a}",
    "k",
    "K",
    "s",
    "S",
    "0",
    "5",
    "9",
    "_",
    "a",
    "z",
    "A",
    "Z",
    "я",
    "Я",
    "ё",
    "Ё",
    "й",
    "\u{1f600}",
    "-",
    ".",
    ",",
    "<",
    ">",
    "/",
    "\u{e000}",
    "\u{e001}",
    "\u{e100}",
    "\u{e101}",
];

const LONE: &[&[u16]] = &[&[0xD800], &[0xDC00], &[0xDBFF, 0xDBFF], &[0xDC00, 0xD800]];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn units(c: char) -> Vec<u16> {
    c.encode_utf16(&mut [0; 2]).to_vec()
}

fn hex(chars: &[char], i: &mut usize, digits: usize) -> Option<u32> {
    let s: String = chars.get(*i..*i + digits)?.iter().collect();
    let v = u32::from_str_radix(&s, 16).ok()?;
    *i += digits;
    Some(v)
}

fn code_unit_or_point(v: u32) -> Vec<u16> {
    char::from_u32(v).map_or_else(|| vec![v as u16], units)
}

fn literal(source: &str) -> Vec<u16> {
    let chars: Vec<char> = source.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c != '\\' {
            if !"()[]{}?*+|^$".contains(c) {
                out.extend(units(c));
            }
            continue;
        }
        let Some(&e) = chars.get(i) else { break };
        i += 1;
        match e {
            'u' if chars.get(i) == Some(&'{') => {
                let end = chars[i..].iter().position(|&c| c == '}').map_or(chars.len(), |k| i + k);
                let digits: String = chars[i + 1..end].iter().collect();
                if let Ok(v) = u32::from_str_radix(&digits, 16) {
                    out.extend(code_unit_or_point(v));
                }
                i = end + 1;
            }
            'u' => out.extend(hex(&chars, &mut i, 4).map_or_default(code_unit_or_point)),
            'x' => out.extend(hex(&chars, &mut i, 2).map_or_default(code_unit_or_point)),
            'n' => out.push(0x0A),
            'r' => out.push(0x0D),
            't' => out.push(0x09),
            'f' => out.push(0x0C),
            'v' => out.push(0x0B),
            'd' => out.push(u16::from(b'7')),
            'w' => out.push(u16::from(b'q')),
            's' => out.push(0x20),
            'D' | 'W' | 'S' => out.push(u16::from(b'-')),
            'b' | 'B' => {}
            'p' | 'P' if chars.get(i) == Some(&'{') => {
                i = chars[i..].iter().position(|&c| c == '}').map_or(chars.len(), |k| i + k + 1);
                out.extend(units('ж'));
            }
            other => out.extend(units(other)),
        }
    }
    out
}

fn alphabet(literal: &[u16], flags: &str) -> Vec<Vec<u16>> {
    let mut symbols: Vec<Vec<u16>> = SPECIALS.iter().map(|s| s.encode_utf16().collect()).collect();
    symbols.extend(LONE.iter().map(|s| s.to_vec()));
    for c in char::decode_utf16(literal.iter().copied()).filter_map(Result::ok) {
        let cp = u32::from(c);
        for near in [cp.wrapping_sub(1), cp, cp + 1] {
            symbols.extend(char::from_u32(near).map(units));
        }
        if flags.contains('i') {
            symbols.extend(c.to_uppercase().chain(c.to_lowercase()).map(units));
        }
    }
    symbols.sort();
    symbols.dedup();
    symbols
}

fn sample(rng: &mut Rng, literal: &[u16], alphabet: &[Vec<u16>], symbols: usize) -> Vec<u16> {
    let mut s = Vec::new();
    for _ in 0..symbols {
        if !literal.is_empty() && rng.below(5) < 3 {
            let at = rng.below(literal.len());
            let len = 1 + rng.below((literal.len() - at).min(8));
            s.extend_from_slice(&literal[at..at + len]);
        } else {
            s.extend_from_slice(&alphabet[rng.below(alphabet.len())]);
        }
    }
    s
}

fn filler(rng: &mut Rng, alphabet: &[Vec<u16>], len: usize) -> Vec<u16> {
    let mut s = Vec::new();
    while s.len() < len {
        s.extend_from_slice(if rng.below(4) == 0 { &alphabet[rng.below(alphabet.len())] } else { &[0x78] });
    }
    s.truncate(len);
    s
}

fn trace(re: &regress::Regex, unicode: bool, text: &[u16], every: bool) -> Vec<(usize, Found)> {
    let mut slots: Slots = vec![None; re.group_count()];
    let mut out = Vec::new();
    let mut walk = |find: &mut Find<'_>| {
        let mut from = 0;
        while from <= text.len() {
            slots.fill(None);
            let found = find(from, &mut slots);
            out.push((from, found.clone().map(|r| (r, slots.clone()))));
            from = match found {
                _ if every => from + 1,
                Some(r) if r.end > r.start => r.end,
                Some(r) => r.end + 1,
                None => break,
            };
        }
    };
    if unicode {
        let mut matches = re.find_from_utf16(text, 0);
        walk(&mut |from, slots| matches.find_at_into(from, slots));
    } else {
        let mut matches = re.find_from_ucs2(text, 0);
        walk(&mut |from, slots| matches.find_at_into(from, slots));
    }
    out
}

fn show(text: &[u16]) -> String {
    char::decode_utf16(text.iter().copied())
        .map(|c| match c {
            Ok(c) if (' '..='~').contains(&c) && c != '\\' && c != '"' => c.to_string(),
            Ok(c) if c.is_alphanumeric() => c.to_string(),
            Ok(c) => format!("\\u{{{:x}}}", u32::from(c)),
            Err(e) => format!("\\u{{{:x}}}", e.unpaired_surrogate()),
        })
        .collect()
}

fn corpus_texts() -> Vec<Vec<u16>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/conformance/corpus.jsonl");
    let Ok(source) = std::fs::read_to_string(&path) else { return Vec::new() };
    let lines: Vec<&str> = source.lines().skip(1).collect();
    let step = (lines.len() / CORPUS).max(1);
    lines
        .iter()
        .step_by(step)
        .filter_map(|line| match Json::parse(line).get("text") {
            Some(Json::Str(text)) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn check(index: usize, source: &str, flags: &str, attempt: regress::aot::Attempt, corpus: &[Vec<u16>]) -> Vec<String> {
    let unicode = flags.contains('u') || flags.contains('v');
    let interpreted = regress::Regex::with_flags(source, flags).expect("a generated pattern compiles");
    let mut compiled = interpreted.clone();
    compiled.set_attempt(attempt);
    let literal = literal(source);
    let alphabet = alphabet(&literal, flags);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ (index as u64 + 1).wrapping_mul(0xBF58_476D_1CE4_E5B9));

    let mut inputs: Vec<(Vec<u16>, bool)> = vec![(Vec::new(), true), (literal.clone(), true)];
    inputs.extend(alphabet.iter().map(|symbol| (symbol.clone(), true)));
    for _ in 0..SHORT {
        let symbols = rng.below(SHORT_SYMBOLS + 1);
        inputs.push((sample(&mut rng, &literal, &alphabet, symbols), true));
    }
    for _ in 0..WALKS {
        let Some(walk) = regress::codegen::sample(&interpreted, rng.next()) else { continue };
        let (head, tail) = (rng.below(3), rng.below(3));
        let mut text = sample(&mut rng, &literal, &alphabet, head);
        text.extend_from_slice(&walk);
        text.extend(sample(&mut rng, &literal, &alphabet, tail));
        let mut mutated = text.clone();
        let at = rng.below(mutated.len() + 1);
        match rng.below(3) {
            0 if at < mutated.len() => drop(mutated.remove(at)),
            1 if at < mutated.len() => mutated[at] = alphabet[rng.below(alphabet.len())][0],
            _ => mutated.splice(at..at, alphabet[rng.below(alphabet.len())].iter().copied()).for_each(drop),
        }
        inputs.push((text, true));
        inputs.push((mutated, true));
    }
    for _ in 0..BOUNDARY {
        let symbols = 1 + rng.below(4);
        let piece = match regress::codegen::sample(&interpreted, rng.next()) {
            Some(walk) if rng.below(4) > 0 => walk,
            _ => sample(&mut rng, &literal, &alphabet, symbols),
        };
        let block = if rng.below(2) == 0 { 64 } else { 128 };
        let lead = (block + rng.below(9)).saturating_sub(4 + piece.len() / 2);
        let mut text = filler(&mut rng, &alphabet, lead);
        text.extend_from_slice(&piece);
        let tail = rng.below(80);
        text.extend(filler(&mut rng, &alphabet, tail));
        inputs.push((text, true));
    }
    for text in corpus {
        let mut text = text.clone();
        let symbols = 1 + rng.below(4);
        let piece = match regress::codegen::sample(&interpreted, rng.next()) {
            Some(walk) => walk,
            None => sample(&mut rng, &literal, &alphabet, symbols),
        };
        let at = rng.below(text.len() + 1);
        text.splice(at..at, piece);
        inputs.push((text, false));
    }

    let mut failures = Vec::new();
    for (text, every) in &inputs {
        let a = trace(&compiled, unicode, text, *every);
        let b = trace(&interpreted, unicode, text, *every);
        if a == b {
            continue;
        }
        let k = a.iter().zip(&b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
        let (from, generated) = a.get(k).cloned().unwrap_or((usize::MAX, None));
        let interpreted = b.get(k).and_then(|r| r.1.clone());
        failures.push(format!(
            "/{source}/{flags} on \"{}\" from {from}: generated {generated:?}, interpreter {interpreted:?}",
            show(text)
        ));
        break;
    }
    failures
}

#[test]
fn generated_matchers_agree_with_the_interpreter() {
    let corpus = corpus_texts();
    let matchers: Vec<_> = generated().iter().enumerate().collect();
    assert!(!matchers.is_empty(), "no generated matchers");
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let chunk = matchers.len().div_ceil(threads).max(1);
    let failures: Vec<String> = std::thread::scope(|scope| {
        matchers
            .chunks(chunk)
            .map(|part| {
                let corpus = &corpus;
                scope.spawn(move || {
                    part.iter()
                        .flat_map(|&(i, &(source, flags, attempt, _))| check(i, source, flags, attempt, corpus))
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|h| h.join().expect("worker finished"))
            .collect()
    });
    for f in failures.iter().take(10) {
        eprintln!("  {f}");
    }
    assert!(failures.is_empty(), "{} generated matchers differ from the interpreter", failures.len());
}

fn glyph_inputs(corpus: &[Vec<u16>]) -> Vec<Vec<u16>> {
    let glyphs: Vec<u16> = GLYPH_TO_ENTITY.iter().flat_map(|&(g, _)| units(g)).collect();
    let alphabet = alphabet(&glyphs, "u");
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut inputs: Vec<Vec<u16>> = vec![Vec::new(), glyphs.clone()];
    inputs.extend(alphabet.iter().cloned());
    for _ in 0..SHORT * 4 {
        let symbols = rng.below(SHORT_SYMBOLS * 2 + 1);
        inputs.push(sample(&mut rng, &glyphs, &alphabet, symbols));
    }
    inputs.extend(corpus.iter().cloned());
    inputs
}

#[test]
fn glyph_scans_agree_with_the_pattern() {
    let source: String = GLYPH_TO_ENTITY.iter().map(|&(g, _)| escape(&units(g))).collect();
    let re = Re::configured(&format!("[{source}]"), "gu");
    let entity = |g: u16| GLYPH_TO_ENTITY.iter().find(|&&(c, _)| units(c) == [g]).map(|&(_, e)| e);
    let mut failures = Vec::new();
    for text in glyph_inputs(&corpus_texts()) {
        let html = re.replace_with(&text, |c| {
            entity(c.whole()[0]).map_or_else(|| c.whole().to_vec(), |e| e.encode_utf16().collect())
        });
        let xml = re.replace_with(&text, |c| {
            let g = c.whole()[0];
            if entity(g).is_some() { format!("&#{g};").encode_utf16().collect() } else { c.whole().to_vec() }
        });
        if unicode_to_entities(&text) != html || unicode_to_xml_entities(&text) != xml {
            failures.push(show(&text));
        }
    }
    for f in failures.iter().take(10) {
        eprintln!("  \"{f}\"");
    }
    assert!(failures.is_empty(), "{} texts convert to entities otherwise than the pattern does", failures.len());
}
