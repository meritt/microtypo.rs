use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn the_budget_ends_a_catastrophic_input() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_microtypo"))
        .args(["--input", "markdown", "--max-ms", "200", "--max-input", "60000"])
        .current_dir(env!("CARGO_TARGET_TMPDIR"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(" ".repeat(60_000).as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with("error: Processing exceeded 200ms budget at "), "{stderr}");
}
