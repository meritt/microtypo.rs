#![forbid(unsafe_code)]

mod args;
mod config;
mod diagnostics;
mod errors;
mod help;
mod js;
mod paths;
mod project;
mod run;
mod util;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };
    ExitCode::from(run::run(&args, &cwd))
}
