use microtypo_core::{Edit, MicroTypo, ProcessOptions, TraceEntry};
use serde_json::json;
use std::fmt::Write as _;
use std::io::Write as _;

use crate::args::Options;
use crate::errors::Result;

pub struct Processed {
    pub input: Vec<u16>,
    pub output: Vec<u16>,
    pub trace: Vec<TraceEntry>,
    pub edits: Vec<Edit>,
}

pub fn process(engine: &mut MicroTypo, input: String, opts: &Options, ignored: bool) -> Result<Processed> {
    let input: Vec<_> = input.encode_utf16().collect();
    let (text, trace) = if ignored {
        (input.clone(), Vec::new())
    } else if opts.explain {
        let result =
            engine.process_with_diagnostics(&input, ProcessOptions { trace: true, ..ProcessOptions::default() })?;
        (result.text, result.trace.unwrap_or_default())
    } else {
        (engine.process_utf16(&input)?, Vec::new())
    };
    let output = keep_final_newline(&input, text);
    let edits = if opts.check {
        microtypo_core::internals::replacement_edits(&input, &output, &|_| Ok(()))?
    } else {
        Vec::new()
    };
    Ok(Processed { input, output, trace, edits })
}

pub fn report(file: &str, result: &Processed, opts: &Options) {
    let mut out = std::io::stdout().lock();
    if opts.explain {
        let _ = writeln!(
            std::io::stderr().lock(),
            "{{\"file\":{},\"trace\":[{}]}}",
            json!(file),
            result.trace.iter().map(TraceEntry::to_json).collect::<Vec<_>>().join(",")
        );
    }
    if opts.check {
        let input = &result.input;
        let (mut at, mut line, mut column) = (0, 1, 1);
        for edit in &result.edits {
            while at < edit.start {
                if input[at] == 10 || (input[at] == 13 && input.get(at + 1) != Some(&10)) {
                    line += 1;
                    column = 1;
                } else {
                    column += 1;
                }
                at += 1;
            }
            let _ = writeln!(out, "{file}:{line}:{column}");
        }
    }
    if opts.diff && result.input != result.output {
        let before = lines(&result.input);
        let after = lines(&result.output);
        let _ = writeln!(
            out,
            "--- {}\n+++ {}\n@@ -{},{} +{},{} @@",
            diff_path(&format!("a/{file}")),
            diff_path(&format!("b/{file}")),
            usize::from(!before.is_empty()),
            before.len(),
            usize::from(!after.is_empty()),
            after.len()
        );
        for (text, entries, prefix) in [(&result.input, before, '-'), (&result.output, after, '+')] {
            for entry in &entries {
                let _ = writeln!(out, "{prefix}{}", String::from_utf16_lossy(entry));
            }
            if !entries.is_empty() && !text.ends_with(&[10]) {
                let _ = writeln!(out, "\\ No newline at end of file");
            }
        }
    }
}

fn lines(text: &[u16]) -> Vec<&[u16]> {
    if text.is_empty() { Vec::new() } else { text.strip_suffix(&[10]).unwrap_or(text).split(|&c| c == 10).collect() }
}

fn diff_path(path: &str) -> String {
    let mut escaped = String::new();
    let mut quoted = false;
    for character in path.chars() {
        let replacement = match character {
            '\t' => Some("\\t"),
            '\n' => Some("\\n"),
            '\r' => Some("\\r"),
            '\u{8}' => Some("\\b"),
            '\u{c}' => Some("\\f"),
            '\u{b}' => Some("\\v"),
            '"' => Some("\\\""),
            '\\' => Some("\\\\"),
            _ => None,
        };
        if let Some(replacement) = replacement {
            quoted = true;
            escaped.push_str(replacement);
        } else if character < ' ' || character == '\u{7f}' {
            quoted = true;
            write!(escaped, "\\{:03o}", character as u32).expect("write to String");
        } else {
            escaped.push(character);
        }
    }
    if quoted {
        format!("\"{escaped}\"")
    } else if path.contains(' ') {
        format!("{path}\t")
    } else {
        path.to_owned()
    }
}

fn keep_final_newline(input: &[u16], mut output: Vec<u16>) -> Vec<u16> {
    if !input.ends_with(&[10]) || output.ends_with(&[10]) {
        return output;
    }
    let carries_lf = output.windows(2).any(|w| w[0] != 13 && w[1] == 10);
    output.extend_from_slice(if input.ends_with(&[13, 10]) && !carries_lf { &[13, 10] } else { &[10] });
    output
}
