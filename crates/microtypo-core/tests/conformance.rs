use std::path::PathBuf;

use microtypo_conformance::{Json, Replay, quoted, replay};

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
    let header = Json::parse(lines.next().expect("corpus has a header"));
    assert_eq!(header.get("schema"), Some(&Json::Num(1.0)), "unknown corpus schema");
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
                        let c = Json::parse(line);
                        let expect = c.get("expect").expect("case has an expectation");
                        match replay(&c) {
                            Replay::Done(actual) if actual.same(expect) => passed += 1,
                            Replay::Skipped => skipped += 1,
                            Replay::Done(actual) => {
                                failures.push(format!(r#"{{"case":{line},"actual":{}}}"#, actual.to_json()));
                            }
                            Replay::Panicked(message) => {
                                failures.push(format!(r#"{{"case":{line},"panic":{}}}"#, quoted(&message)));
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
