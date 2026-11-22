use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

pub fn alone() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner)
}

fn samples(mut run: impl FnMut()) -> [f64; 3] {
    [(); 3].map(|()| {
        let start = Instant::now();
        run();
        start.elapsed().as_secs_f64()
    })
}

pub fn best(run: impl FnMut()) -> f64 {
    samples(run).into_iter().fold(f64::INFINITY, f64::min)
}

pub fn median(run: impl FnMut()) -> f64 {
    let mut samples = samples(run);
    samples.sort_by(f64::total_cmp);
    samples[1]
}

pub fn linear(label: &str, warm: usize, n: usize, mut measure: impl FnMut(usize) -> f64) {
    measure(warm);
    let [one, two, four] = [n, 2 * n, 4 * n].map(&mut measure);
    eprintln!(
        "{label}: N {:.3}ms, 2N {:.3}ms, 4N {:.3}ms; 4N/N {:.2}",
        one * 1000.0,
        two * 1000.0,
        four * 1000.0,
        four / one
    );
    assert!(two < 3.0 * one, "{label}: 2N/N {:.2}", two / one);
    assert!(four < 8.0 * one, "{label}: 4N/N {:.2}", four / one);
}
