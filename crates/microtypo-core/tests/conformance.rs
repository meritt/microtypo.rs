use std::fmt::Write as _;
use std::path::PathBuf;

use microtypo_core::MicroTypo;
use microtypo_core::api::Output;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(Vec<u16>),
    Arr(Vec<J>),
    Obj(Vec<(Vec<u16>, J)>),
}

impl J {
    fn get(&self, key: &str) -> Option<&J> {
        let key: Vec<u16> = key.encode_utf16().collect();
        match self {
            J::Obj(entries) => entries.iter().find(|(k, _)| *k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn to_value(&self) -> Option<Value> {
        Some(match self {
            J::Null => Value::Null,
            J::Bool(b) => Value::Bool(*b),
            J::Num(n) => {
                if n.fract() == 0.0 && n.abs() < 9.0e15 {
                    Value::from(*n as i64)
                } else {
                    Value::from(*n)
                }
            }
            J::Str(s) => Value::String(String::from_utf16(s).ok()?),
            J::Arr(items) => Value::Array(items.iter().map(J::to_value).collect::<Option<_>>()?),
            J::Obj(entries) => Value::Object(
                entries
                    .iter()
                    .map(|(k, v)| Some((String::from_utf16(k).ok()?, v.to_value()?)))
                    .collect::<Option<_>>()?,
            ),
        })
    }

    fn from_value(value: &Value) -> J {
        match value {
            Value::Null => J::Null,
            Value::Bool(b) => J::Bool(*b),
            Value::Number(n) => J::Num(n.as_f64().unwrap_or(f64::NAN)),
            Value::String(s) => J::Str(s.encode_utf16().collect()),
            Value::Array(items) => J::Arr(items.iter().map(J::from_value).collect()),
            Value::Object(map) => {
                J::Obj(map.iter().map(|(k, v)| (k.encode_utf16().collect(), J::from_value(v))).collect())
            }
        }
    }

    fn obj(entries: Vec<(&str, J)>) -> J {
        J::Obj(entries.into_iter().map(|(k, v)| (k.encode_utf16().collect(), v)).collect())
    }

    fn str(s: &str) -> J {
        J::Str(s.encode_utf16().collect())
    }

    fn same(&self, other: &J) -> bool {
        match (self, other) {
            (J::Obj(a), J::Obj(b)) => {
                a.len() == b.len() && a.iter().all(|(k, v)| b.iter().any(|(k2, v2)| k == k2 && v.same(v2)))
            }
            (J::Arr(a), J::Arr(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.same(y)),
            _ => self == other,
        }
    }

    fn show(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Num(n) => write!(out, "{n}").unwrap(),
            J::Str(s) => out.push_str(&serde_json::to_string(&String::from_utf16_lossy(s)).unwrap()),
            J::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.show(out);
                }
                out.push(']');
            }
            J::Obj(entries) => {
                out.push('{');
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    J::Str(k.clone()).show(out);
                    out.push(':');
                    v.show(out);
                }
                out.push('}');
            }
        }
    }

    fn to_json(&self) -> String {
        let mut out = String::new();
        self.show(&mut out);
        out
    }
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

    fn value(&mut self) -> J {
        self.ws();
        match self.s[self.i] {
            b'{' => {
                self.i += 1;
                let mut entries = Vec::new();
                self.ws();
                if self.s[self.i] == b'}' {
                    self.i += 1;
                    return J::Obj(entries);
                }
                loop {
                    self.ws();
                    let J::Str(key) = self.value() else { panic!("object key at {}", self.i) };
                    self.ws();
                    assert_eq!(self.s[self.i], b':');
                    self.i += 1;
                    entries.push((key, self.value()));
                    self.ws();
                    self.i += 1;
                    if self.s[self.i - 1] == b'}' {
                        return J::Obj(entries);
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.s[self.i] == b']' {
                    self.i += 1;
                    return J::Arr(items);
                }
                loop {
                    items.push(self.value());
                    self.ws();
                    self.i += 1;
                    if self.s[self.i - 1] == b']' {
                        return J::Arr(items);
                    }
                }
            }
            b'"' => J::Str(self.string()),
            b't' => {
                self.i += 4;
                J::Bool(true)
            }
            b'f' => {
                self.i += 5;
                J::Bool(false)
            }
            b'n' => {
                self.i += 4;
                J::Null
            }
            _ => {
                let start = self.i;
                while self.i < self.s.len() && matches!(self.s[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    self.i += 1;
                }
                J::Num(std::str::from_utf8(&self.s[start..self.i]).unwrap().parse().unwrap())
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

fn parse(line: &str) -> J {
    Parser { s: line.as_bytes(), i: 0 }.value()
}

fn error_j(e: &microtypo_core::Error) -> J {
    let mut entries = vec![("name", J::str(e.name())), ("message", J::str(&e.message))];
    if let Some(code) = e.code() {
        entries.push(("code", J::str(code)));
    }
    if !e.details.is_null() {
        entries.push(("details", J::from_value(&e.details)));
    }
    J::obj(entries)
}

enum Run {
    Done(J),
    Skipped,
}

fn run(c: &J) -> Run {
    let config = match c.get("config") {
        None => None,
        Some(j) => match j.to_value() {
            Some(v) => Some(v),
            None => return Run::Skipped,
        },
    };

    let mut engine = match MicroTypo::new(config.as_ref()) {
        Ok(engine) => engine,
        Err(e) => return Run::Done(J::obj(vec![("construct", error_j(&e))])),
    };

    let mut ops = Vec::new();
    if let Some(J::Arr(list)) = c.get("ops") {
        for op in list {
            let Some(J::Str(method)) = op.get("method") else { panic!("op without a method") };
            let Some(Value::Array(args)) = op.get("args").and_then(J::to_value) else { return Run::Skipped };
            ops.push(match engine.call(&String::from_utf16_lossy(method), &args) {
                Ok(()) => J::Null,
                Err(e) => error_j(&e),
            });
        }
    }

    let mut outcome = Vec::new();
    if ops.iter().any(|op| *op != J::Null) {
        outcome.push(("ops", J::Arr(ops)));
    }

    let Some(J::Str(text)) = c.get("text") else { return Run::Done(J::obj(outcome)) };
    let options = match c.get("options") {
        None => None,
        Some(j) => match j.to_value() {
            Some(v) => Some(v),
            None => return Run::Skipped,
        },
    };

    match engine.process_with_options(text, options.as_ref()) {
        Ok(Output::Text(t)) => outcome.push(("output", J::Str(t))),
        Ok(Output::WithChanges(p)) => outcome.push((
            "output",
            J::obj(vec![
                ("text", J::Str(p.text)),
                (
                    "changes",
                    J::Arr(p.changes.iter().map(|&(a, b)| J::Arr(vec![J::Num(a as f64), J::Num(b as f64)])).collect()),
                ),
            ]),
        )),
        Err(e) => outcome.push(("error", error_j(&e))),
    }

    Run::Done(J::obj(outcome))
}

fn corpus_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("MICROTYPO_CORPUS") {
        return Some(PathBuf::from(path));
    }
    let copy = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/conformance/corpus.jsonl");
    copy.exists().then_some(copy)
}

#[test]
fn conformance_corpus() {
    let Some(path) = corpus_path() else {
        eprintln!("conformance: no corpus found (set MICROTYPO_CORPUS); skipped");
        return;
    };
    let source = std::fs::read_to_string(&path).expect("corpus is readable");
    let mut lines = source.lines();
    let header = parse(lines.next().expect("corpus has a header"));
    assert_eq!(header.get("schema"), Some(&J::Num(1.0)), "unknown corpus schema");
    let only = std::env::var("MICROTYPO_CORPUS_ONLY").ok();
    let lines: Vec<&str> = lines.filter(|l| only.as_ref().is_none_or(|p| l[7..].starts_with(p.as_str()))).collect();

    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let chunk = lines.len().div_ceil(threads).max(1);
    let results: Vec<(usize, usize, Vec<String>)> = std::thread::scope(|scope| {
        lines
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    let (mut passed, mut skipped, mut failures) = (0, 0, Vec::new());
                    for line in part {
                        let c = parse(line);
                        let expect = c.get("expect").expect("case has an expectation");
                        let caught = std::panic::catch_unwind(|| run(&c));
                        match caught {
                            Ok(Run::Done(actual)) if actual.same(expect) => passed += 1,
                            Ok(Run::Skipped) => skipped += 1,
                            Ok(Run::Done(actual)) => {
                                failures.push(format!(r#"{{"case":{line},"actual":{}}}"#, actual.to_json()));
                            }
                            Err(panic) => {
                                let msg = panic
                                    .downcast_ref::<String>()
                                    .cloned()
                                    .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                                    .unwrap_or_default();
                                failures.push(format!(
                                    r#"{{"case":{line},"panic":{}}}"#,
                                    serde_json::to_string(&msg).unwrap()
                                ));
                            }
                        }
                    }
                    (passed, skipped, failures)
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().expect("worker finished"))
            .collect()
    });

    let passed: usize = results.iter().map(|r| r.0).sum();
    let skipped: usize = results.iter().map(|r| r.1).sum();
    let failures: Vec<&String> = results.iter().flat_map(|r| &r.2).collect();
    let report = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/conformance-failures.jsonl");
    std::fs::write(&report, failures.iter().fold(String::new(), |out, f| out + f + "\n")).expect("report is writable");

    eprintln!(
        "conformance: {passed} passed, {} failed, {skipped} skipped of {} ({})",
        failures.len(),
        lines.len(),
        path.display()
    );
    for f in failures.iter().take(5) {
        eprintln!("  {}", f.chars().take(600).collect::<String>());
    }
    assert!(failures.is_empty(), "{} cases differ from the reference; see {}", failures.len(), report.display());
}
