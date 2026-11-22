use std::fmt::Write as _;
use std::sync::LazyLock;
use std::time::Instant;

use microtypo_core::{Error, Host, MicroTypo, Output};
use serde_json::Value;

/// The host of a native process: the monotonic clock and the system CSPRNG.
pub const HOST: Host = Host { now_ms, fill_random };

fn now_ms() -> f64 {
    static START: LazyLock<Instant> = LazyLock::new(Instant::now);
    START.elapsed().as_secs_f64() * 1000.0
}

fn fill_random(bytes: &mut [u8]) {
    getrandom::fill(bytes).expect("the system CSPRNG is available");
}

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(Vec<u16>),
    Arr(Vec<Json>),
    Obj(Vec<(Vec<u16>, Json)>),
}

impl Json {
    pub fn parse(line: &str) -> Json {
        Parser { s: line.as_bytes(), i: 0 }.value()
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        let key: Vec<u16> = key.encode_utf16().collect();
        match self {
            Json::Obj(entries) => entries.iter().find(|(k, _)| *k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn same(&self, other: &Json) -> bool {
        match (self, other) {
            (Json::Obj(a), Json::Obj(b)) => {
                a.len() == b.len() && a.iter().all(|(k, v)| b.iter().any(|(k2, v2)| k == k2 && v.same(v2)))
            }
            (Json::Arr(a), Json::Arr(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.same(y)),
            _ => self == other,
        }
    }

    pub fn to_json(&self) -> String {
        let mut out = String::new();
        self.show(&mut out);
        out
    }

    fn to_value(&self) -> Option<Value> {
        Some(match self {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Num(n) => {
                if n.fract() == 0.0 && n.abs() < 9.0e15 {
                    Value::from(*n as i64)
                } else {
                    Value::from(*n)
                }
            }
            Json::Str(s) => Value::String(String::from_utf16(s).ok()?),
            Json::Arr(items) => Value::Array(items.iter().map(Json::to_value).collect::<Option<_>>()?),
            Json::Obj(entries) => Value::Object(
                entries
                    .iter()
                    .map(|(k, v)| Some((String::from_utf16(k).ok()?, v.to_value()?)))
                    .collect::<Option<_>>()?,
            ),
        })
    }

    fn from_value(value: &Value) -> Json {
        match value {
            Value::Null => Json::Null,
            Value::Bool(b) => Json::Bool(*b),
            Value::Number(n) => Json::Num(n.as_f64().unwrap_or(f64::NAN)),
            Value::String(s) => Json::Str(s.encode_utf16().collect()),
            Value::Array(items) => Json::Arr(items.iter().map(Json::from_value).collect()),
            Value::Object(map) => {
                Json::Obj(map.iter().map(|(k, v)| (k.encode_utf16().collect(), Json::from_value(v))).collect())
            }
        }
    }

    fn obj(entries: Vec<(&str, Json)>) -> Json {
        Json::Obj(entries.into_iter().map(|(k, v)| (k.encode_utf16().collect(), v)).collect())
    }

    fn str(s: &str) -> Json {
        Json::Str(s.encode_utf16().collect())
    }

    fn show(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(n) => write!(out, "{n}").unwrap(),
            Json::Str(s) => quote(s, out),
            Json::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.show(out);
                }
                out.push(']');
            }
            Json::Obj(entries) => {
                out.push('{');
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    quote(k, out);
                    out.push(':');
                    v.show(out);
                }
                out.push('}');
            }
        }
    }
}

pub fn quoted(s: &str) -> String {
    let mut out = String::new();
    quote(&s.encode_utf16().collect::<Vec<_>>(), &mut out);
    out
}

fn quote(s: &[u16], out: &mut String) {
    out.push('"');
    for unit in char::decode_utf16(s.iter().copied()) {
        match unit {
            Ok('"') => out.push_str("\\\""),
            Ok('\\') => out.push_str("\\\\"),
            Ok('\n') => out.push_str("\\n"),
            Ok('\r') => out.push_str("\\r"),
            Ok('\t') => out.push_str("\\t"),
            Ok(c) if u32::from(c) < 0x20 => write!(out, "\\u{:04x}", u32::from(c)).unwrap(),
            Ok(c) => out.push(c),
            Err(lone) => write!(out, "\\u{:04x}", lone.unpaired_surrogate()).unwrap(),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn value(&mut self) -> Json {
        self.ws();
        match self.s[self.i] {
            b'{' => {
                self.i += 1;
                let mut entries = Vec::new();
                self.ws();
                if self.s[self.i] == b'}' {
                    self.i += 1;
                    return Json::Obj(entries);
                }
                loop {
                    self.ws();
                    let Json::Str(key) = self.value() else { panic!("object key at {}", self.i) };
                    self.ws();
                    assert_eq!(self.s[self.i], b':');
                    self.i += 1;
                    entries.push((key, self.value()));
                    self.ws();
                    self.i += 1;
                    if self.s[self.i - 1] == b'}' {
                        return Json::Obj(entries);
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.s[self.i] == b']' {
                    self.i += 1;
                    return Json::Arr(items);
                }
                loop {
                    items.push(self.value());
                    self.ws();
                    self.i += 1;
                    if self.s[self.i - 1] == b']' {
                        return Json::Arr(items);
                    }
                }
            }
            b'"' => Json::Str(self.string()),
            b't' => {
                self.i += 4;
                Json::Bool(true)
            }
            b'f' => {
                self.i += 5;
                Json::Bool(false)
            }
            b'n' => {
                self.i += 4;
                Json::Null
            }
            _ => {
                let start = self.i;
                while self.i < self.s.len() && matches!(self.s[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    self.i += 1;
                }
                Json::Num(std::str::from_utf8(&self.s[start..self.i]).unwrap().parse().unwrap())
            }
        }
    }

    fn string(&mut self) -> Vec<u16> {
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let start = self.i;
            while self.s[self.i] != b'"' && self.s[self.i] != b'\\' {
                self.i += 1;
            }
            out.extend(std::str::from_utf8(&self.s[start..self.i]).unwrap().encode_utf16());
            let c = self.s[self.i];
            self.i += 1;
            if c == b'"' {
                return out;
            }
            let e = self.s[self.i];
            self.i += 1;
            out.push(match e {
                b'"' => 0x22,
                b'\\' => 0x5C,
                b'/' => 0x2F,
                b'b' => 0x08,
                b'f' => 0x0C,
                b'n' => 0x0A,
                b'r' => 0x0D,
                b't' => 0x09,
                b'u' => {
                    let hex = std::str::from_utf8(&self.s[self.i..self.i + 4]).unwrap();
                    self.i += 4;
                    u16::from_str_radix(hex, 16).unwrap()
                }
                _ => panic!("bad escape at {}", self.i),
            });
        }
    }
}

fn error_json(e: &Error) -> Json {
    let mut entries = vec![("name", Json::str(e.name())), ("message", Json::str(&e.message))];
    if let Some(code) = e.code() {
        entries.push(("code", Json::str(code)));
    }
    if !e.details.is_null() {
        entries.push(("details", Json::from_value(&e.details)));
    }
    Json::obj(entries)
}

#[derive(Debug)]
pub enum Replay {
    Done(Json),
    Skipped,
    Panicked(String),
}

pub fn replay(c: &Json) -> Replay {
    match std::panic::catch_unwind(|| run(c)) {
        Ok(Some(outcome)) => Replay::Done(outcome),
        Ok(None) => Replay::Skipped,
        Err(panic) => Replay::Panicked(
            panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_default(),
        ),
    }
}

fn run(c: &Json) -> Option<Json> {
    let mut engine = match c
        .get("config")
        .map_or_else(|| MicroTypo::new(None, HOST), |config| MicroTypo::from_json(&config.to_json(), HOST))
    {
        Ok(engine) => engine,
        Err(e) => return Some(Json::obj(vec![("construct", error_json(&e))])),
    };

    let mut ops = Vec::new();
    if let Some(Json::Arr(list)) = c.get("ops") {
        for op in list {
            let Some(Json::Str(method)) = op.get("method") else { panic!("op without a method") };
            let Some(args @ Json::Arr(_)) = op.get("args") else { return None };
            ops.push(match engine.call_json(&String::from_utf16_lossy(method), &args.to_json()) {
                Ok(()) => Json::Null,
                Err(e) => error_json(&e),
            });
        }
    }

    let mut outcome = Vec::new();
    if ops.iter().any(|op| *op != Json::Null) {
        outcome.push(("ops", Json::Arr(ops)));
    }

    let Some(Json::Str(text)) = c.get("text") else { return Some(Json::obj(outcome)) };
    outcome.push(process(&mut engine, text, c.get("options"))?);

    if let Some(Json::Arr(series)) = c.get("then") {
        let mut then = Vec::new();
        for item in series {
            let Some(Json::Str(text)) = item.get("text") else { return None };
            let (key, value) = process(&mut engine, text, item.get("options"))?;
            then.push(Json::obj(vec![(key, value)]));
        }
        outcome.push(("then", Json::Arr(then)));
    }

    Some(Json::obj(outcome))
}

fn process(engine: &mut MicroTypo, text: &[u16], options: Option<&Json>) -> Option<(&'static str, Json)> {
    let options = match options {
        None => None,
        Some(j) => Some(j.to_value()?),
    };

    Some(match engine.process_with_options(text, options.as_ref()) {
        Ok(Output::Text(t)) => ("output", Json::Str(t)),
        Ok(Output::Diagnostics(output)) => ("output", Json::parse(&output.to_json())),
        Err(e) => ("error", error_json(&e)),
    })
}
