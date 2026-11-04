use super::JsString;

#[derive(Debug, Clone)]
pub struct JsonError {
    pub message: String,
    pub position: usize,
}

const CR: u16 = 0x0D;
const LF: u16 = 0x0A;

const CONTEXT: usize = 10;

const SPECIAL: [&str; 4] = ["[object Object]", "undefined", "Infinity", "NaN"];

fn is_ws(c: u16) -> bool {
    matches!(c, 0x20 | 0x09 | LF | CR)
}

fn is_digit(c: Option<&u16>) -> bool {
    matches!(c, Some(0x30..=0x39))
}

fn skip_ws(s: &[u16], mut i: usize) -> usize {
    while i < s.len() && is_ws(s[i]) {
        i += 1;
    }
    i
}

fn line_column(s: &[u16], pos: usize) -> (usize, usize) {
    let (mut line, mut line_start, mut k) = (1, 0, 0);
    while k < pos {
        if s[k] == CR && k + 1 < pos && s[k + 1] == LF {
            k += 1;
        }
        if s[k] == CR || s[k] == LF {
            line += 1;
            line_start = k + 1;
        }
        k += 1;
    }
    (line, pos - line_start + 1)
}

fn fail(message: JsString, position: usize) -> JsonError {
    JsonError { message: super::lossy(&message), position }
}

fn located(s: &[u16], pos: usize, what: &str) -> JsonError {
    let (line, column) = line_column(s, pos);
    fail(super::u(&format!("{what} at position {pos} (line {line} column {column})")), pos)
}

fn unexpected(s: &[u16], pos: usize) -> JsonError {
    let Some(&c) = s.get(pos) else {
        return fail(super::u("Unexpected end of JSON input"), pos);
    };
    match c {
        0x2D | 0x30..=0x39 => return located(s, pos, "Unexpected number in JSON"),
        0x22 => return located(s, pos, "Unexpected string in JSON"),
        _ => {}
    }
    if SPECIAL.iter().any(|w| super::u(w) == s) {
        return fail([&[0x22], s, &super::u("\" is not valid JSON")].concat(), pos);
    }
    let n = s.len();
    let (open, context, close) = if n <= 2 * CONTEXT {
        ("", s, "")
    } else if pos < CONTEXT {
        ("", &s[..pos + CONTEXT], "...")
    } else if pos < n - CONTEXT {
        ("...", &s[pos - CONTEXT..pos + CONTEXT], "...")
    } else {
        ("...", &s[pos - CONTEXT..], "")
    };
    let message = [
        &super::u("Unexpected token '")[..],
        &[c],
        &super::u(&format!("', {open}\"")),
        context,
        &super::u(&format!("\"{close} is not valid JSON")),
    ]
    .concat();
    fail(message, pos)
}

fn scan_string(s: &[u16], mut i: usize, out: Option<&mut JsString>) -> Result<usize, JsonError> {
    let mut sink = out;
    i += 1;
    loop {
        let Some(&c) = s.get(i) else {
            return Err(located(s, i, "Unterminated string in JSON"));
        };
        match c {
            0x22 => return Ok(i + 1),
            0x5C => {
                i += 1;
                let decoded: u16 = match s.get(i) {
                    Some(0x22) => 0x22,
                    Some(0x5C) => 0x5C,
                    Some(0x2F) => 0x2F,
                    Some(0x62) => 0x08,
                    Some(0x66) => 0x0C,
                    Some(0x6E) => 0x0A,
                    Some(0x72) => 0x0D,
                    Some(0x74) => 0x09,
                    Some(0x75) => {
                        let mut v: u16 = 0;
                        for _ in 0..4 {
                            i += 1;
                            let d = match s.get(i) {
                                Some(&h @ 0x30..=0x39) => h - 0x30,
                                Some(&h @ 0x41..=0x46) => h - 0x41 + 10,
                                Some(&h @ 0x61..=0x66) => h - 0x61 + 10,
                                _ => return Err(located(s, i, "Bad Unicode escape in JSON")),
                            };
                            v = v * 16 + d;
                        }
                        v
                    }
                    Some(0..=0xFF) => return Err(located(s, i, "Bad escaped character in JSON")),
                    _ => return Err(unexpected(s, i)),
                };
                if let Some(o) = sink.as_deref_mut() {
                    o.push(decoded);
                }
                i += 1;
            }
            c if c < 0x20 => return Err(located(s, i, "Bad control character in string literal in JSON")),
            c => {
                if let Some(o) = sink.as_deref_mut() {
                    o.push(c);
                }
                i += 1;
            }
        }
    }
}

fn scan_number(s: &[u16], mut i: usize) -> Result<usize, JsonError> {
    if s.get(i) == Some(&0x2D) {
        i += 1;
    }
    if s.get(i) == Some(&0x30) {
        i += 1;
        if is_digit(s.get(i)) {
            return Err(unexpected(s, i));
        }
    } else if is_digit(s.get(i)) {
        while is_digit(s.get(i)) {
            i += 1;
        }
    } else {
        return Err(located(s, i, "No number after minus sign in JSON"));
    }
    if s.get(i) == Some(&0x2E) {
        i += 1;
        if !is_digit(s.get(i)) {
            return Err(located(s, i, "Unterminated fractional number in JSON"));
        }
        while is_digit(s.get(i)) {
            i += 1;
        }
    }
    if matches!(s.get(i), Some(0x65 | 0x45)) {
        i += 1;
        if matches!(s.get(i), Some(0x2B | 0x2D)) {
            i += 1;
        }
        if !is_digit(s.get(i)) {
            return Err(located(s, i, "Exponent part is missing a number in JSON"));
        }
        while is_digit(s.get(i)) {
            i += 1;
        }
    }
    Ok(i)
}

fn scan_word(s: &[u16], i: usize, word: &str) -> Result<usize, JsonError> {
    for (k, b) in word.bytes().enumerate() {
        if s.get(i + k) != Some(&u16::from(b)) {
            return Err(unexpected(s, i + k));
        }
    }
    Ok(i + word.len())
}

fn scan_key(s: &[u16], i: usize, missing: &str) -> Result<usize, JsonError> {
    let i = skip_ws(s, i);
    if s.get(i) != Some(&0x22) {
        return Err(located(s, i, missing));
    }
    let i = skip_ws(s, scan_string(s, i, None)?);
    if s.get(i) != Some(&0x3A) {
        return Err(located(s, i, "Expected ':' after property name in JSON"));
    }
    Ok(i + 1)
}

pub fn validate(s: &[u16]) -> Result<(), JsonError> {
    #[derive(Clone, Copy, PartialEq)]
    enum Frame {
        Array,
        Object,
    }
    let mut stack: Vec<Frame> = Vec::new();
    let mut i = 0;
    'value: loop {
        i = skip_ws(s, i);
        match s.get(i) {
            Some(0x7B) => {
                i = skip_ws(s, i + 1);
                if s.get(i) == Some(&0x7D) {
                    i += 1;
                } else {
                    stack.push(Frame::Object);
                    i = scan_key(s, i, "Expected property name or '}' in JSON")?;
                    continue 'value;
                }
            }
            Some(0x5B) => {
                i = skip_ws(s, i + 1);
                if s.get(i) == Some(&0x5D) {
                    i += 1;
                } else {
                    stack.push(Frame::Array);
                    continue 'value;
                }
            }
            Some(0x22) => i = scan_string(s, i, None)?,
            Some(0x74) => i = scan_word(s, i, "true")?,
            Some(0x66) => i = scan_word(s, i, "false")?,
            Some(0x6E) => i = scan_word(s, i, "null")?,
            Some(0x2D | 0x30..=0x39) => i = scan_number(s, i)?,
            _ => return Err(unexpected(s, i)),
        }
        loop {
            i = skip_ws(s, i);
            let Some(&top) = stack.last() else {
                if i < s.len() {
                    return Err(located(s, i, "Unexpected non-whitespace character after JSON"));
                }
                return Ok(());
            };
            match (top, s.get(i)) {
                (Frame::Array, Some(0x2C)) => {
                    i += 1;
                    continue 'value;
                }
                (Frame::Object, Some(0x2C)) => {
                    i = scan_key(s, i + 1, "Expected double-quoted property name in JSON")?;
                    continue 'value;
                }
                (Frame::Array, Some(0x5D)) | (Frame::Object, Some(0x7D)) => {
                    stack.pop();
                    i += 1;
                }
                (Frame::Array, _) => return Err(located(s, i, "Expected ',' or ']' after array element in JSON")),
                (Frame::Object, _) => return Err(located(s, i, "Expected ',' or '}' after property value in JSON")),
            }
        }
    }
}

pub fn parse_string(s: &[u16]) -> Result<JsString, JsonError> {
    let i = skip_ws(s, 0);
    if s.get(i) != Some(&0x22) {
        validate(s)?;
        return Err(JsonError { message: "expected a string".into(), position: i });
    }
    let mut out = Vec::new();
    let end = skip_ws(s, scan_string(s, i, Some(&mut out))?);
    if end < s.len() {
        return Err(located(s, end, "Unexpected non-whitespace character after JSON"));
    }
    Ok(out)
}

pub fn stringify(s: &[u16]) -> JsString {
    let mut out = Vec::with_capacity(s.len() + 2);
    out.push(0x22);
    let hex = |out: &mut JsString, c: u16| {
        for b in format!("\\u{c:04x}").bytes() {
            out.push(u16::from(b));
        }
    };
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        match c {
            0x22 => out.extend_from_slice(&[0x5C, 0x22]),
            0x5C => out.extend_from_slice(&[0x5C, 0x5C]),
            0x08 => out.extend_from_slice(&[0x5C, 0x62]),
            0x0C => out.extend_from_slice(&[0x5C, 0x66]),
            0x0A => out.extend_from_slice(&[0x5C, 0x6E]),
            0x0D => out.extend_from_slice(&[0x5C, 0x72]),
            0x09 => out.extend_from_slice(&[0x5C, 0x74]),
            c if c < 0x20 => hex(&mut out, c),
            c if super::is_high(c) => {
                if i + 1 < s.len() && super::is_low(s[i + 1]) {
                    out.push(c);
                    out.push(s[i + 1]);
                    i += 1;
                } else {
                    hex(&mut out, c);
                }
            }
            c if super::is_low(c) => hex(&mut out, c),
            c => out.push(c),
        }
        i += 1;
    }
    out.push(0x22);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::js::u;

    fn message(s: &[u16]) -> String {
        validate(s).err().map(|e| e.message).unwrap_or_default()
    }

    #[test]
    fn v8_messages() {
        let table: &[(&str, &str)] = &[
            ("", "Unexpected end of JSON input"),
            ("[", "Unexpected end of JSON input"),
            ("tru", "Unexpected end of JSON input"),
            ("\"\\", "Unexpected end of JSON input"),
            ("{", "Expected property name or '}' in JSON at position 1 (line 1 column 2)"),
            ("{\"a\" 1}", "Expected ':' after property name in JSON at position 5 (line 1 column 6)"),
            ("{\"a\":1 2}", "Expected ',' or '}' after property value in JSON at position 7 (line 1 column 8)"),
            ("{\"a\":1,}", "Expected double-quoted property name in JSON at position 7 (line 1 column 8)"),
            ("[1 2]", "Expected ',' or ']' after array element in JSON at position 3 (line 1 column 4)"),
            ("1 x", "Unexpected non-whitespace character after JSON at position 2 (line 1 column 3)"),
            ("\"abc", "Unterminated string in JSON at position 4 (line 1 column 5)"),
            ("\"\\x\"", "Bad escaped character in JSON at position 2 (line 1 column 3)"),
            ("\"\\u12\"", "Bad Unicode escape in JSON at position 5 (line 1 column 6)"),
            ("\"\\Ж\"", "Unexpected token 'Ж', \"\"\\Ж\"\" is not valid JSON"),
            ("\r\n\r\n\"\u{1}\"", "Bad control character in string literal in JSON at position 5 (line 3 column 2)"),
            ("\n\r\"\u{1}\"", "Bad control character in string literal in JSON at position 3 (line 3 column 2)"),
            ("-", "No number after minus sign in JSON at position 1 (line 1 column 2)"),
            ("-01", "Unexpected number in JSON at position 2 (line 1 column 3)"),
            ("1.a", "Unterminated fractional number in JSON at position 2 (line 1 column 3)"),
            ("1e+", "Exponent part is missing a number in JSON at position 3 (line 1 column 4)"),
            ("t\"", "Unexpected string in JSON at position 1 (line 1 column 2)"),
            ("undefined", "\"undefined\" is not valid JSON"),
            ("[object Object]", "\"[object Object]\" is not valid JSON"),
            (" undefined", "Unexpected token 'u', \" undefined\" is not valid JSON"),
            ("xxxxxxxxxxxxxxxxxxxx", "Unexpected token 'x', \"xxxxxxxxxxxxxxxxxxxx\" is not valid JSON"),
            ("xxxxxxxxxxxxxxxxxxxxx", "Unexpected token 'x', \"xxxxxxxxxx\"... is not valid JSON"),
            (
                "[1,2,3,4,5,6,7,8,9,x,1,2,3,4,5,6,7,8,9,0]",
                "Unexpected token 'x', ...\"5,6,7,8,9,x,1,2,3,4,\"... is not valid JSON",
            ),
            (
                "[1,2,3,4,5,6,7,8,9,0,1,2,3,4,x,6,7,8,9]",
                "Unexpected token 'x', ...\"0,1,2,3,4,x,6,7,8,9]\" is not valid JSON",
            ),
            ("😀", "Unexpected token '\u{FFFD}', \"😀\" is not valid JSON"),
        ];
        for &(src, expected) in table {
            assert_eq!(message(&u(src)), expected, "{src:?}");
        }
        assert_eq!(message(&[0x5B, 0xDE00, 0x5D]), "Unexpected token '\u{FFFD}', \"[\u{FFFD}]\" is not valid JSON");
        assert_eq!(message(&u("[1, \"a\", {\"b\": [true, null, -0.5e+3]}]\r\n")), "");
    }

    #[test]
    fn string_literals() {
        assert_eq!(parse_string(&u(" \"a\\u00e9\\ud83d\" ")).unwrap(), [0x61, 0xE9, 0xD83D]);
        let e = parse_string(&u("\"a\" \"b\"")).unwrap_err();
        assert_eq!(e.message, "Unexpected non-whitespace character after JSON at position 4 (line 1 column 5)");
        let e = parse_string(&u("\"\\u00g0\"")).unwrap_err();
        assert_eq!(e.message, "Bad Unicode escape in JSON at position 5 (line 1 column 6)");
    }
}
