use std::time::Instant;

use microtypo_conformance::HOST;
use microtypo_core::MicroTypo;
use serde_json::json;

use crate::timing::{alone, best};

fn values(n: usize) -> String {
    let items: Vec<&str> = (0..n).map(|i| if i % 2 == 0 { "\"слово\"" } else { "\"Иван\"" }).collect();
    format!("{{\"a\":[{}]}}", items.join(","))
}

#[test]
#[ignore = "resource measurement runs separately in release mode"]
fn skipped_values_scale_linearly_and_fit_the_default_budget() {
    let _alone = alone();
    let mut unlimited =
        MicroTypo::new(Some(&json!({"input":"json","maxInputLength":5_000_000,"maxProcessingMs":0})), HOST).unwrap();
    let mut measure = |n: usize| {
        let source = values(n);
        best(|| assert_eq!(unlimited.process(&source).unwrap(), source))
    };
    measure(10_000);
    let [one, two, four] = [100_000, 200_000, 400_000].map(&mut measure);
    eprintln!(
        "skipped values: N {:.1}ms, 2N {:.1}ms, 4N {:.1}ms; 2N/N {:.2}, 4N/2N {:.2}",
        one * 1000.0,
        two * 1000.0,
        four * 1000.0,
        two / one,
        four / two
    );
    assert!(two < one * 3.0, "2N/N {}", two / one);
    assert!(four < two * 3.0, "4N/2N {}", four / two);

    let mut default = MicroTypo::new(Some(&json!({"input":"json","maxInputLength":5_000_000})), HOST).unwrap();
    let source = values(300_000);
    let start = Instant::now();
    assert_eq!(default.process(&source).unwrap(), source);
    eprintln!("300 000 values under the default budget: {:.1}ms", start.elapsed().as_secs_f64() * 1000.0);
}
