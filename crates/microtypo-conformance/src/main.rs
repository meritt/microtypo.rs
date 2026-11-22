use std::io::{self, BufRead, ErrorKind, Write};
use std::process::ExitCode;

use microtypo_conformance::{Json, Replay, quoted, replay};

fn serve() -> io::Result<()> {
    let mut out = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let reply = match replay(&Json::parse(&line?)) {
            Replay::Done(actual) => format!(r#"{{"actual":{}}}"#, actual.to_json()),
            Replay::Skipped => r#"{"skipped":true}"#.to_owned(),
            Replay::Panicked(message) => format!(r#"{{"panic":{}}}"#, quoted(&message)),
        };
        writeln!(out, "{reply}")?;
    }
    Ok(())
}

fn patterns() -> io::Result<()> {
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.starts_with(r#"{"id":"#) {
            replay(&Json::parse(&line));
        }
    }
    let mut out = io::stdout().lock();
    for (source, flags) in microtypo_core::internals::builtins() {
        writeln!(out, "[{},{}]", quoted(&source), quoted(&flags))?;
    }
    Ok(())
}

fn main() -> ExitCode {
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = match std::env::args().nth(1).as_deref() {
        None => serve(),
        Some("--patterns") => patterns(),
        Some(other) => {
            eprintln!("microtypo-conformance: unknown argument {other}; usage: microtypo-conformance [--patterns]");
            return ExitCode::FAILURE;
        }
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.kind() == ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("microtypo-conformance: {error}");
            ExitCode::FAILURE
        }
    }
}
