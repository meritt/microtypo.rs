use std::time::Instant;

use microtypo_core::MicroTypo;
use serde_json::{Value, json};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../../microtypo/test/fixtures/corpus.json").to_owned()
    });
    let cases: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(&path).expect("corpus.json")).expect("JSON");
    let texts: Vec<Vec<u16>> =
        cases.iter().filter_map(|c| c["text"].as_str()).map(|t| t.encode_utf16().collect()).collect();
    let config = json!({ "html": true, "entities": true });

    let until = Instant::now() + std::time::Duration::from_secs(8);
    match std::env::args().nth(2).as_deref() {
        Some("loop-new") => {
            while Instant::now() < until {
                MicroTypo::new(Some(&config)).unwrap();
            }
            return;
        }
        Some("loop-process") => {
            let mut engine = MicroTypo::new(Some(&config)).unwrap();
            while Instant::now() < until {
                for t in &texts {
                    engine.process(t).unwrap();
                }
            }
            return;
        }
        _ => {}
    }

    let n = 200;
    let start = Instant::now();
    for _ in 0..n {
        MicroTypo::new(Some(&config)).unwrap();
    }
    println!("new(): {:.3} ms", start.elapsed().as_secs_f64() * 1e3 / f64::from(n));

    let mut engine = MicroTypo::new(Some(&config)).unwrap();
    let rounds = 5;
    let start = Instant::now();
    for _ in 0..rounds {
        for t in &texts {
            engine.process(t).unwrap();
        }
    }
    let per = start.elapsed().as_secs_f64() * 1e3 / (rounds * texts.len()) as f64;
    println!("process(): {per:.3} ms per text over {} texts", texts.len());
}
