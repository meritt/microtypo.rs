use std::hint::black_box;
use std::time::{Duration, Instant};

use microtypo_conformance::HOST;
use microtypo_core::MicroTypo;
use serde_json::{Map, Value, json};

const ROUNDS: usize = 3;

enum Run<'a> {
    New(Option<&'a Value>),
    Process(Box<MicroTypo>, Vec<Vec<u16>>),
}

fn measure(mut op: impl FnMut()) -> Vec<f64> {
    let warm = Instant::now();
    let mut runs = 0_u32;
    while runs < 3 || warm.elapsed() < Duration::from_millis(100) {
        op();
        runs += 1;
    }
    let per = warm.elapsed().as_secs_f64() / f64::from(runs);
    let batch = ((0.002 / per).ceil() as u32).max(1);
    let mut samples = Vec::new();
    let start = Instant::now();
    while samples.len() < 10 || (start.elapsed() < Duration::from_millis(333) && samples.len() < 400) {
        let t = Instant::now();
        for _ in 0..batch {
            op();
        }
        samples.push(t.elapsed().as_secs_f64() * 1e3 / f64::from(batch));
    }
    samples
}

fn bench(path: &str) {
    let points: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(path).expect("points")).expect("JSON");
    let mut runs: Vec<Run<'_>> = points
        .iter()
        .map(|point| {
            let config = Some(&point["config"]).filter(|c| !c.is_null());
            if point["op"] == "new" {
                return Run::New(config);
            }
            let texts = point["texts"]
                .as_array()
                .expect("texts")
                .iter()
                .map(|t| t.as_str().expect("text").encode_utf16().collect())
                .collect();
            Run::Process(Box::new(MicroTypo::new(config, HOST).unwrap()), texts)
        })
        .collect();
    let mut samples = vec![Vec::new(); points.len()];
    for _ in 0..ROUNDS {
        for (run, out) in runs.iter_mut().zip(&mut samples) {
            match run {
                Run::New(config) => out.extend(measure(|| drop(black_box(MicroTypo::new(*config, HOST).unwrap())))),
                Run::Process(engine, texts) => {
                    let per = texts.len() as f64;
                    let round = measure(|| {
                        for t in texts.iter() {
                            black_box(engine.process_utf16(t).unwrap());
                        }
                    });
                    out.extend(round.into_iter().map(|ms| ms / per));
                }
            }
        }
    }
    let out: Map<String, Value> = points
        .iter()
        .zip(samples)
        .map(|(point, samples)| (point["name"].as_str().expect("name").to_owned(), json!(samples)))
        .collect();
    println!("{}", Value::Object(out));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [mode, points] = args.as_slice() else { panic!("usage: profile bench <points.json>") };
    assert_eq!(mode, "bench", "usage: profile bench <points.json>");
    bench(points);
}
