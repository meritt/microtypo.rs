use serde_json::{Map, Number, Value};

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
const MAX_DEPTH: usize = 4096;

pub fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

fn radix(digits: &str, base: u32) -> f64 {
    if digits.is_empty() {
        return f64::NAN;
    }
    let mut value = 0.0;
    for c in digits.chars() {
        match c.to_digit(base) {
            Some(d) => value = value * f64::from(base) + f64::from(d),
            None => return f64::NAN,
        }
    }
    value
}

fn decimal(s: &str) -> f64 {
    let (sign, body) = match s.as_bytes().first() {
        Some(b'+') => ("", &s[1..]),
        Some(b'-') => ("-", &s[1..]),
        _ => ("", s),
    };
    match body {
        "Infinity" => return if sign.is_empty() { f64::INFINITY } else { f64::NEG_INFINITY },
        "" => return f64::NAN,
        _ => {}
    }
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(at) => (&body[..at], Some(&body[at + 1..])),
        None => (body, None),
    };
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if (int.is_empty() && frac.is_empty()) || !digits(int) || !digits(frac) {
        return f64::NAN;
    }
    let exponent = match exponent {
        None => "0",
        Some(e) => {
            let unsigned = e.strip_prefix(['+', '-']).unwrap_or(e);
            if unsigned.is_empty() || !digits(unsigned) {
                return f64::NAN;
            }
            e
        }
    };
    let int = if int.is_empty() { "0" } else { int };
    let frac = if frac.is_empty() { "0" } else { frac };
    format!("{sign}{int}.{frac}e{exponent}").parse().unwrap_or(f64::NAN)
}

pub fn to_number(raw: &str) -> f64 {
    let s = raw.trim_matches(is_js_space);
    if s.is_empty() {
        return 0.0;
    }
    let prefixed = |p: &[&str]| p.iter().find_map(|p| s.strip_prefix(p));
    if let Some(hex) = prefixed(&["0x", "0X"]) {
        radix(hex, 16)
    } else if let Some(octal) = prefixed(&["0o", "0O"]) {
        radix(octal, 8)
    } else if let Some(binary) = prefixed(&["0b", "0B"]) {
        radix(binary, 2)
    } else {
        decimal(s)
    }
}

pub fn number(n: f64) -> Value {
    if !n.is_finite() {
        return Value::from(-0.5);
    }
    if n.fract() == 0.0 && n.abs() <= MAX_SAFE_INTEGER {
        return Value::from(n as i64);
    }
    Number::from_f64(n).map_or(Value::Null, Value::Number)
}

fn array_index(key: &str) -> Option<u32> {
    if key == "0" {
        return Some(0);
    }
    if key.starts_with('0') || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    key.parse::<u32>().ok().filter(|&i| i != u32::MAX)
}

pub fn order_keys(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for v in map.values_mut() {
                order_keys(v);
            }
            if map.keys().any(|k| array_index(k).is_some()) {
                let mut entries: Vec<(String, Value)> = std::mem::take(map).into_iter().collect();
                entries.sort_by_key(|(k, _)| array_index(k).map_or((1, 0), |i| (0, i)));
                map.extend(entries);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(order_keys),
        _ => {}
    }
}

pub fn stringify(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

struct Parser<'s> {
    text: &'s [u8],
    at: usize,
}

impl Parser<'_> {
    fn space(&mut self) {
        while matches!(self.text.get(self.at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn eat(&mut self, b: u8) -> bool {
        let found = self.text.get(self.at) == Some(&b);
        if found {
            self.at += 1;
        }
        found
    }

    fn word(&mut self, w: &str, v: Value) -> Option<Value> {
        self.text[self.at..].starts_with(w.as_bytes()).then(|| {
            self.at += w.len();
            v
        })
    }

    fn value(&mut self, depth: usize) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.space();
        match *self.text.get(self.at)? {
            b'{' => {
                self.at += 1;
                let mut map = Map::new();
                self.space();
                if self.eat(b'}') {
                    return Some(Value::Object(map));
                }
                loop {
                    self.space();
                    let key = self.string()?;
                    self.space();
                    if !self.eat(b':') {
                        return None;
                    }
                    let value = self.value(depth + 1)?;
                    map.insert(key, value);
                    self.space();
                    if self.eat(b'}') {
                        return Some(Value::Object(map));
                    }
                    if !self.eat(b',') {
                        return None;
                    }
                }
            }
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                self.space();
                if self.eat(b']') {
                    return Some(Value::Array(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.space();
                    if self.eat(b']') {
                        return Some(Value::Array(items));
                    }
                    if !self.eat(b',') {
                        return None;
                    }
                }
            }
            b'"' => self.string().map(Value::String),
            b't' => self.word("true", Value::Bool(true)),
            b'f' => self.word("false", Value::Bool(false)),
            b'n' => self.word("null", Value::Null),
            _ => self.number(),
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.at;
        while self.text.get(self.at).is_some_and(u8::is_ascii_digit) {
            self.at += 1;
        }
        self.at - start
    }

    fn number(&mut self) -> Option<Value> {
        let start = self.at;
        self.eat(b'-');
        if !self.eat(b'0') && self.digits() == 0 {
            return None;
        }
        if self.eat(b'.') && self.digits() == 0 {
            return None;
        }
        if self.eat(b'e') || self.eat(b'E') {
            if !self.eat(b'+') {
                self.eat(b'-');
            }
            if self.digits() == 0 {
                return None;
            }
        }
        let text = std::str::from_utf8(&self.text[start..self.at]).ok()?;
        Some(number(text.parse().ok()?))
    }

    fn hex4(&mut self) -> Option<u16> {
        let digits = std::str::from_utf8(self.text.get(self.at..self.at + 4)?).ok()?;
        if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        self.at += 4;
        u16::from_str_radix(digits, 16).ok()
    }

    fn string(&mut self) -> Option<String> {
        if !self.eat(b'"') {
            return None;
        }
        let mut units: Vec<u16> = Vec::new();
        let mut run = self.at;
        loop {
            let b = *self.text.get(self.at)?;
            if b == b'"' || b == b'\\' || b < 0x20 {
                units.extend(std::str::from_utf8(&self.text[run..self.at]).ok()?.encode_utf16());
            }
            match b {
                b'"' => {
                    self.at += 1;
                    return Some(String::from_utf16_lossy(&units));
                }
                b'\\' => {
                    self.at += 1;
                    let escape = *self.text.get(self.at)?;
                    self.at += 1;
                    let unit = match escape {
                        b'"' => 0x22,
                        b'\\' => 0x5c,
                        b'/' => 0x2f,
                        b'b' => 0x08,
                        b'f' => 0x0c,
                        b'n' => 0x0a,
                        b'r' => 0x0d,
                        b't' => 0x09,
                        b'u' => self.hex4()?,
                        _ => return None,
                    };
                    units.push(unit);
                    run = self.at;
                }
                0..0x20 => return None,
                _ => self.at += 1,
            }
        }
    }
}

pub fn parse_json(text: &str) -> Option<Value> {
    let mut parser = Parser { text: text.as_bytes(), at: 0 };
    let value = parser.value(0)?;
    parser.space();
    (parser.at == text.len()).then_some(value)
}
