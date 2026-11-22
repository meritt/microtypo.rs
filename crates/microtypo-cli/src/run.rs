use std::fs::{self, File, OpenOptions};
use std::hash::{BuildHasher, RandomState};
use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Instant;

use microtypo_core::{Host, MicroTypo, VERSION};
use serde_json::Value;

use crate::args::{Options, parse};
use crate::diagnostics::{self, Processed};
use crate::errors::{CliError, Result, if_present, system, usage};
use crate::help::HELP;
use crate::paths::{Ignore, dirname, is_inside, resolve, select_files};
use crate::project::Project;

const BYTES_PER_CHAR_BOUND: u64 = 3;

pub(crate) const HOST: Host = Host { now_ms, fill_random };

fn now_ms() -> f64 {
    static START: LazyLock<Instant> = LazyLock::new(Instant::now);
    START.elapsed().as_secs_f64() * 1000.0
}

fn fill_random(bytes: &mut [u8]) {
    getrandom::fill(bytes).expect("the system CSPRNG is available");
}

pub fn run(args: &[String], cwd: &Path) -> u8 {
    let opts = match parse(args) {
        Ok(opts) => opts,
        Err(e) => return fail(&e),
    };
    if opts.help {
        write_out(HELP.as_bytes());
        return 0;
    }
    if opts.version {
        write_out(format!("{VERSION}\n").as_bytes());
        return 0;
    }
    match Project::load(&opts, cwd).and_then(|project| transform(&project, &opts, cwd)) {
        Ok(code) => code,
        Err(e) => fail(&e),
    }
}

fn write_out(bytes: &[u8]) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(bytes);
    let _ = out.flush();
}

fn color_enabled(terminal: bool) -> bool {
    let get = |key: &str| std::env::var_os(key).map(|v| v.to_string_lossy().into_owned());
    if let Some(force) = get("FORCE_COLOR") {
        return matches!(force.as_str(), "" | "1" | "true" | "2" | "3");
    }
    if !terminal
        || get("NO_COLOR").is_some_and(|v| !v.is_empty())
        || get("NODE_DISABLE_COLORS").is_some_and(|v| !v.is_empty())
        || get("TERM").as_deref() == Some("dumb")
    {
        return false;
    }
    if get("TMUX").is_some_and(|v| !v.is_empty()) || (get("TF_BUILD").is_some() && get("AGENT_NAME").is_some()) {
        return true;
    }
    if get("CI").is_some() {
        return [
            "APPVEYOR",
            "BUILDKITE",
            "CIRCLECI",
            "DRONE",
            "GITEA_ACTIONS",
            "GITHUB_ACTIONS",
            "GITLAB_CI",
            "TRAVIS",
        ]
        .iter()
        .any(|name| get(name).is_some())
            || get("CI_NAME").as_deref() == Some("codeship");
    }
    if let Some(version) = get("TEAMCITY_VERSION") {
        let mut parts = version.split('.');
        let major = parts.next().unwrap_or_default();
        let minor = parts.next();
        let has_second_dot = parts.next().is_some();
        return minor.is_some()
            && ((major.len() >= 2 && major.bytes().all(|c| c.is_ascii_digit()))
                || (major == "9"
                    && has_second_dot
                    && minor.is_some_and(|m| {
                        !m.is_empty() && m.bytes().all(|c| c.is_ascii_digit()) && m.bytes().any(|c| c != b'0')
                    })));
    }
    if matches!(get("TERM_PROGRAM").as_deref(), Some("iTerm.app" | "HyperTerm" | "MacTerm" | "Apple_Terminal")) {
        return true;
    }
    if let Some(term) = get("TERM").filter(|s| !s.is_empty()) {
        let term = term.to_ascii_lowercase();
        let recognized = [
            "eterm",
            "cons25",
            "console",
            "cygwin",
            "dtterm",
            "gnome",
            "hurd",
            "jfbterm",
            "konsole",
            "kterm",
            "mlterm",
            "mosh",
            "putty",
            "st",
            "rxvt-unicode-24bit",
            "terminator",
            "xterm-kitty",
        ];
        let con = term.strip_prefix("con").is_some_and(|rest| {
            let n = rest.bytes().take_while(u8::is_ascii_digit).count();
            rest.as_bytes().get(n) == Some(&b'x') && rest.as_bytes().get(n + 1).is_some_and(u8::is_ascii_digit)
        });
        if recognized.contains(&term.as_str())
            || con
            || ["ansi", "color", "linux", "direct"].iter().any(|s| term.contains(s))
            || ["rxvt", "screen", "xterm", "vt100", "vt220"].iter().any(|s| term.starts_with(s))
        {
            return true;
        }
    }
    get("COLORTERM").is_some_and(|s| !s.is_empty())
}

fn fail(e: &CliError) -> u8 {
    let code = e.exit_code();
    let message = format!("error: {}\n", e.message());
    let mut err = io::stderr().lock();
    if color_enabled(err.is_terminal()) {
        let styled = message.replace("\x1b[39m", "\x1b[31m");
        let _ = write!(err, "\x1b[31m{styled}\x1b[39m");
    } else {
        let _ = err.write_all(message.as_bytes());
    }
    if code == 2 {
        let _ = err.write_all(b"run `microtypo --help` for usage\n");
    }
    code
}

struct Target {
    file: String,
    path: PathBuf,
    mode: u32,
    result: Processed,
}

fn build(project: &Project, file: Option<&str>, opts: &Options, cwd: &Path) -> Result<(MicroTypo, u64)> {
    let config = project.config_for(file, opts, cwd)?;
    let max = config.value.get("maxInputLength").and_then(Value::as_f64).unwrap_or(5_000_000.0) as u64;
    Ok((MicroTypo::from_json(&config.to_json(), HOST)?, max))
}

fn transform(project: &Project, opts: &Options, cwd: &Path) -> Result<u8> {
    if opts.write && opts.output_file.as_deref().is_some_and(|f| !f.is_empty()) {
        return Err(usage("--output-file cannot combine with --write"));
    }
    if (opts.check && opts.diff)
        || ((opts.check || opts.diff) && (opts.write || opts.output_file.as_deref().is_some_and(|s| !s.is_empty())))
    {
        return Err(usage("--check and --diff are separate read-only modes"));
    }
    if opts.stdin_filepath.is_some() && !opts.files.is_empty() {
        return Err(usage("--stdin-filepath requires stdin input"));
    }
    let ignored = Ignore::load(&project.root)?;
    if opts.files.is_empty() {
        let stdin = io::stdin();
        if stdin.is_terminal() {
            write_out(HELP.as_bytes());
            return Ok(0);
        }
        if opts.write {
            return Err(usage("--write requires file arguments"));
        }
        let (mut engine, max) = build(project, opts.stdin_filepath.as_deref(), opts, cwd)?;
        let input = read_capped_stream(stdin.lock(), max)?;
        let skip = opts.stdin_filepath.as_ref().is_some_and(|p| ignored.contains(&resolve(cwd, p)));
        let result = diagnostics::process(&mut engine, input, opts, skip)?;
        diagnostics::report(opts.stdin_filepath.as_deref().unwrap_or("<stdin>"), &result, opts);
        if !opts.check && !opts.diff {
            emit(&result.output, opts.output_file.as_deref(), cwd)?;
        }
        return Ok(u8::from(opts.check && !result.edits.is_empty()));
    }
    let files = select_files(&opts.files, cwd, &ignored)?;
    if files.is_empty() {
        return Ok(0);
    }
    if files.len() > 1 && !opts.write && !opts.check && !opts.diff {
        return Err(usage("multiple files require --write"));
    }
    let root = if opts.write { Some(resolved_root(cwd)?) } else { None };
    let mut targets = Vec::new();
    let mut batch_units = 0;
    for file in files {
        let (mut engine, max_input_length) = build(project, Some(&file), opts, cwd)?;
        let named = resolve(cwd, &file);
        let path = match &root {
            Some(root) => resolve_target(&named, root)?,
            None => named,
        };
        let (content, mode) = read_capped_file(&path, max_input_length)?;
        batch_units += content.encode_utf16().count();
        if batch_units > 50_000_000 {
            return Err(CliError::Engine(microtypo_core::Error::input(
                "batch exceeds 50000000 input UTF-16 units",
                serde_json::json!({"max":50_000_000}),
            )));
        }
        let result = diagnostics::process(&mut engine, content, opts, false)?;
        if opts.write && result.output != result.input {
            assert_writable(&path)?;
        }
        targets.push(Target { file, path, mode, result });
    }
    if opts.write {
        for target in targets.iter().filter(|t| t.result.output != t.result.input) {
            atomic_write(&target.path, &target.result.output, Some(target.mode))?;
        }
    } else if !opts.check && !opts.diff {
        emit(&targets[0].result.output, opts.output_file.as_deref(), cwd)?;
    }
    for target in &targets {
        diagnostics::report(&target.file, &target.result, opts);
    }
    Ok(u8::from(opts.check && targets.iter().any(|t| !t.result.edits.is_empty())))
}

struct Root {
    named: PathBuf,
    real: PathBuf,
}

fn resolved_root(cwd: &Path) -> Result<Root> {
    let real = fs::canonicalize(cwd).map_err(|e| system(&e, "realpath", Some(cwd), None))?;
    Ok(Root { named: resolve(cwd, "."), real })
}

fn resolve_target(path: &Path, root: &Root) -> Result<PathBuf> {
    let entry = if_present(fs::symlink_metadata(path), |e| system(e, "lstat", Some(path), None))?;
    let followed = if entry.is_some_and(|m| m.file_type().is_symlink()) {
        if_present(fs::canonicalize(path), |e| system(e, "realpath", Some(path), None))?
    } else {
        Some(path.to_path_buf())
    };
    let Some(followed) = followed else {
        return Err(usage(format!("{}: symlink target does not exist", path.display())));
    };
    let directory = dirname(&followed);
    let parent = if_present(fs::canonicalize(&directory), |e| system(e, "realpath", Some(&directory), None))?;
    let Some(parent) = parent else {
        return Err(usage(format!("{}: directory {} does not exist", path.display(), directory.display())));
    };
    let real = match followed.file_name() {
        Some(name) => parent.join(name),
        None => parent,
    };
    if is_inside(&root.named, path) && !is_inside(&root.real, &real) {
        return Err(usage(format!(
            "{}: resolves to {}, outside the working directory",
            path.display(),
            real.display()
        )));
    }
    Ok(real)
}

fn temp_name(directory: &Path) -> PathBuf {
    let seed = RandomState::new().hash_one((std::process::id(), std::time::SystemTime::now()));
    directory.join(format!(".microtypo.{}.{:012x}.tmp", std::process::id(), seed & 0xffff_ffff_ffff))
}

fn assert_writable(path: &Path) -> Result<()> {
    let directory = dirname(path);
    let probe = temp_name(&directory);
    match OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(file) => {
            drop(file);
            let _ = fs::remove_file(&probe);
        }
        Err(e) => return Err(system(&e, "access", Some(&directory), None)),
    }
    if_present(OpenOptions::new().write(true).open(path), |e| system(e, "access", Some(path), None))?;
    Ok(())
}

fn read_capped_file(path: &Path, max_input_length: u64) -> Result<(String, u32)> {
    let info = fs::metadata(path).map_err(|e| system(&e, "stat", Some(path), None))?;
    let max_bytes = max_input_length * BYTES_PER_CHAR_BOUND;
    if info.size() > max_bytes {
        let size = info.size();
        return Err(CliError::Engine(microtypo_core::Error::input(
            format!(
                "{}: file size {size} bytes exceeds the {max_bytes}-byte pre-read cap for maxInputLength {max_input_length}",
                path.display()
            ),
            Value::Null,
        )));
    }
    let file = File::open(path).map_err(|e| system(&e, "open", Some(path), None))?;
    let mut bytes = Vec::new();
    let read = file.take(max_bytes + 1).read_to_end(&mut bytes).map_err(|e| system(&e, "read", None, None))?;
    if read as u64 > max_bytes {
        return Err(CliError::Engine(microtypo_core::Error::input(
            format!(
                "{} exceeds the {max_bytes}-byte pre-read cap for maxInputLength {max_input_length}",
                path.display()
            ),
            Value::Null,
        )));
    }
    Ok((String::from_utf8_lossy(&bytes).into_owned(), info.mode() & 0o777))
}

fn read_capped_stream(stream: impl Read, max_input_length: u64) -> Result<String> {
    let max_bytes = max_input_length * BYTES_PER_CHAR_BOUND;
    let mut bytes = Vec::new();
    stream.take(max_bytes + 1).read_to_end(&mut bytes).map_err(|e| system(&e, "read", None, None))?;
    if bytes.len() as u64 > max_bytes {
        return Err(CliError::Engine(microtypo_core::Error::input(
            format!("stdin exceeds the {max_bytes}-byte pre-read cap for maxInputLength {max_input_length}"),
            Value::Null,
        )));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn emit(output: &[u16], output_file: Option<&str>, cwd: &Path) -> Result<()> {
    match output_file {
        Some(file) if !file.is_empty() => {
            let path = resolve_target(&resolve(cwd, file), &resolved_root(cwd)?)?;
            assert_writable(&path)?;
            let mode = if_present(fs::metadata(&path), |e| system(e, "stat", Some(&path), None))?;
            atomic_write(&path, output, mode.map(|m| m.mode() & 0o777))
        }
        _ => {
            write_out(String::from_utf16_lossy(output).as_bytes());
            Ok(())
        }
    }
}

fn atomic_write(path: &Path, content: &[u16], mode: Option<u32>) -> Result<()> {
    let tmp = temp_name(&dirname(path));
    let written = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode.unwrap_or(0o666))
            .open(&tmp)
            .map_err(|e| system(&e, "open", Some(&tmp), None))?;
        file.write_all(String::from_utf16_lossy(content).as_bytes()).map_err(|e| system(&e, "write", None, None))?;
        if let Some(mode) = mode {
            file.set_permissions(fs::Permissions::from_mode(mode)).map_err(|e| system(&e, "fchmod", None, None))?;
        }
        file.sync_all().map_err(|e| system(&e, "fsync", None, None))?;
        drop(file);
        fs::rename(&tmp, path).map_err(|e| system(&e, "rename", Some(&tmp), Some(path)))
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}
