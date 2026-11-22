use std::collections::HashMap;
use std::fmt::{Display, Write as _};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::{env, fs};

use serde_json::{Value, json};

const USAGE: &str = "usage: cargo xtask header [--check] | wasm | xcframework | dist [<dir>] \
                     | pin [--check] [<dir>] | corpus [--check] [<src>] | matchers [--check]";

const RELEASES: &str = "https://github.com/meritt/microtypo.rs/releases/download";

const BINARYEN: &str = "133";

const WASM_OPT_FLAGS: [&str; 3] = ["-Os", "--strip-debug", "--strip-producers"];

const WASM_GZIP_BUDGET: u64 = 750_000;

const CORPUS_SOURCE: &str = "test/conformance/corpus.jsonl";
const CORPUS: &str = "corpus.jsonl";
const CORPUS_PIN: &str = "testdata/conformance/corpus.sha256";

const MATCHERS: &str = "crates/microtypo-core/src/js/regex/matchers.rs";

const MATCHER_SOURCE_LIMIT: usize = 5_000;

const MATCHERS_LINTS: &str =
    "#![allow(clippy::all, clippy::pedantic, unused_labels, unused_mut, unused_variables, unreachable_code)]";

const MACOS: &str = "26.0";

const SOURCE_DATE_EPOCH: &str = "315532800";

const APPLE_TARGETS: [&str; 2] = ["aarch64-apple-darwin", "x86_64-apple-darwin"];

type Result<T = ()> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["header"] => header(false),
        ["header", "--check"] => header(true),
        ["wasm"] => wasm().map(drop),
        ["xcframework"] => xcframework().map(drop),
        ["dist"] => dist(None),
        ["dist", dir] => dist(Some(Path::new(dir))),
        ["pin"] => pin(false, None),
        ["pin", "--check"] => pin(true, None),
        ["pin", "--check", dir] => pin(true, Some(Path::new(dir))),
        ["pin", dir] => pin(false, Some(Path::new(dir))),
        ["corpus"] => corpus(false, None),
        ["corpus", "--check"] => corpus(true, None),
        ["corpus", "--check", src] => corpus(true, Some(Path::new(src))),
        ["corpus", src] => corpus(false, Some(Path::new(src))),
        ["matchers"] => matchers(false),
        ["matchers", "--check"] => matchers(true),
        _ => Err(USAGE.to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn header(check: bool) -> Result {
    let ffi = root().join("crates/microtypo-ffi");
    let path = ffi.join("include/microtypo.h");
    let config = cbindgen::Config::from_file(ffi.join("cbindgen.toml"))?;
    let bindings = cbindgen::Builder::new()
        .with_crate(&ffi)
        .with_config(config)
        .generate()
        .map_err(|e| format!("cbindgen: {e}"))?;
    if check {
        let mut fresh = Vec::new();
        bindings.write(&mut fresh);
        if fs::read(&path).map_err(at(path.display()))? != fresh {
            return Err(format!("{} is stale: run `cargo xtask header`", path.display()));
        }
    } else {
        bindings.write_to_file(&path);
    }
    Ok(())
}

fn wasm() -> Result<PathBuf> {
    let root = root();
    let version = version()?;
    let manifest = root.join("packages/npm/package.json");
    let npm: Value = serde_json::from_slice(&fs::read(&manifest).map_err(at(manifest.display()))?)
        .map_err(at(manifest.display()))?;
    if npm["version"] != version.as_str() {
        return Err(format!("{} has version {}, Cargo.toml {version}", manifest.display(), npm["version"]));
    }

    let bindgen = tool("WASM_BINDGEN", Some(root.join("target/tools/bin/wasm-bindgen")), "wasm-bindgen");
    let expected = locked_version("wasm-bindgen")?;
    let found = capture(Command::new(&bindgen).arg("--version"))?;
    if found != format!("wasm-bindgen {expected}") {
        return Err(format!(
            "{} is {found}, Cargo.lock has wasm-bindgen {expected}: \
             cargo install wasm-bindgen-cli --version ={expected} --locked --root target/tools",
            bindgen.display()
        ));
    }
    let opt = tool("WASM_OPT", None, "wasm-opt");
    let found = capture(Command::new(&opt).arg("--version"))?;
    if !found.starts_with(&format!("wasm-opt version {BINARYEN}")) {
        return Err(format!("{} is {found}, the package is built with binaryen {BINARYEN}", opt.display()));
    }

    run(cargo().args([
        "build",
        "-p",
        "microtypo-wasm",
        "--profile",
        "wasm",
        "--locked",
        "--target",
        "wasm32-unknown-unknown",
    ]))?;
    let module = target_dir().join("wasm32-unknown-unknown/wasm/microtypo_wasm.wasm");
    let bindgen_out = root.join("target/staging/bindgen");
    fresh_dir(&bindgen_out)?;
    run(Command::new(&bindgen)
        .args(["--target", "web", "--no-typescript", "--out-dir"])
        .arg(&bindgen_out)
        .arg(&module))?;

    let dist = root.join("packages/npm/dist");
    fresh_dir(&dist)?;
    run(Command::new(&opt)
        .args(WASM_OPT_FLAGS)
        .arg(bindgen_out.join("microtypo_wasm_bg.wasm"))
        .arg("-o")
        .arg(dist.join("microtypo.wasm")))?;

    let glue = bindgen_out.join("microtypo_wasm.js");
    let glue = fs::read_to_string(&glue).map_err(at(glue.display()))?;
    if glue.matches("'microtypo_wasm_bg.wasm'").count() != 1 {
        return Err("the wasm-bindgen glue needs one 'microtypo_wasm_bg.wasm'".to_owned());
    }
    write(&dist.join("glue.js"), &glue.replace("'microtypo_wasm_bg.wasm'", "'microtypo.wasm'"))?;
    let template = root.join("packages/npm/src/microtypo.js");
    let text = fs::read_to_string(&template).map_err(at(template.display()))?;
    if text.matches("'@VERSION@'").count() != 1 {
        return Err(format!("{} needs one '@VERSION@'", template.display()));
    }
    write(&dist.join("microtypo.js"), &text.replace("'@VERSION@'", &format!("'{version}'")))?;
    copy(&root.join("LICENSE"), &dist.join("LICENSE"))?;
    copy(&manifest, &dist.join("package.json"))?;
    Ok(dist)
}

fn corpus(check: bool, src: Option<&Path>) -> Result {
    let root = root();
    let copy_path = root.join("testdata/conformance").join(CORPUS);
    let pin_path = root.join(CORPUS_PIN);
    let source = src.map_or_else(|| root.join("../microtypo"), Path::to_path_buf);
    let git = |args: &[&str]| capture(Command::new("git").args(args).current_dir(&source));
    let sha256 = |path: &Path| -> Result<String> {
        let line = capture(Command::new("shasum").args(["-a", "256"]).arg(path))?;
        line.split_whitespace().next().map(str::to_owned).ok_or_else(|| format!("shasum {}: {line}", path.display()))
    };

    if !check {
        if !git(&["status", "--porcelain", "--", CORPUS_SOURCE])?.is_empty() {
            return Err(format!("{} has uncommitted changes: pin a committed corpus", source.display()));
        }
        let commit = git(&["log", "-1", "--format=%H", "--", CORPUS_SOURCE])?;
        copy(&source.join(CORPUS_SOURCE), &copy_path)?;
        let pin = format!("commit {commit}\n{}  {CORPUS}\n", sha256(&copy_path)?);
        write(&pin_path, &pin)?;
        println!("testdata/conformance is the reference's corpus at {commit}");
        return Ok(());
    }

    let pin = fs::read_to_string(&pin_path).map_err(at(pin_path.display()))?;
    let mut commit = None;
    let mut sha = None;
    for line in pin.lines().filter(|l| !l.is_empty()) {
        if let Some(c) = line.strip_prefix("commit ") {
            commit = Some(c);
        } else if let Some(s) = line.strip_suffix(&format!("  {CORPUS}")) {
            sha = Some(s);
        } else {
            return Err(format!("{CORPUS_PIN}: unexpected line {line:?}"));
        }
    }
    let (Some(commit), Some(sha)) = (commit, sha) else {
        return Err(format!("{CORPUS_PIN} needs a commit and the SHA-256 of {CORPUS}"));
    };
    if sha256(&copy_path)? != sha {
        return Err(format!("testdata/conformance/{CORPUS} differs from {CORPUS_PIN}: run `cargo xtask corpus`"));
    }
    if src.is_none() && !source.is_dir() {
        println!("testdata/conformance matches {CORPUS_PIN}; no reference checkout to compare with");
        return Ok(());
    }
    let original = Command::new("git")
        .args(["show", &format!("{commit}:{CORPUS_SOURCE}")])
        .current_dir(&source)
        .stderr(Stdio::inherit())
        .output()
        .map_err(at("git show"))?;
    if !original.status.success() {
        return Err(format!("{} has no {CORPUS_SOURCE} at {commit}", source.display()));
    }
    if fs::read(&copy_path).map_err(at(copy_path.display()))? != original.stdout {
        return Err(format!("testdata/conformance/{CORPUS} differs from the reference at {commit}"));
    }
    println!("testdata/conformance is the reference's corpus at {commit}");
    Ok(())
}

fn matchers(check: bool) -> Result {
    let path = root().join(MATCHERS);
    if check {
        return generate_matchers(&path, true);
    }
    let previous = fs::read(&path).ok();
    write(&path, matchers_file(&[], &[], &[], "")?.as_bytes())?;
    let generated = std::panic::catch_unwind(|| generate_matchers(&path, false))
        .unwrap_or_else(|_| Err("the matcher generator panicked".to_owned()));
    if generated.is_err()
        && let Some(previous) = previous
    {
        write(&path, &previous)?;
    }
    generated
}

fn generate_matchers(path: &Path, check: bool) -> Result {
    let root = root();
    run(cargo().args(["build", "--release", "--locked", "-p", "microtypo-conformance"]))?;
    let corpus_path = root.join("testdata/conformance").join(CORPUS);
    let corpus = fs::File::open(&corpus_path).map_err(at(corpus_path.display()))?;
    let listing = capture(Command::new(conformance()).arg("--patterns").stdin(corpus))?;
    let mut patterns = Vec::new();
    for line in listing.lines() {
        let pattern: Value = serde_json::from_str(line).map_err(at(line))?;
        match (pattern[0].as_str(), pattern[1].as_str()) {
            (Some(source), Some(flags)) => patterns.push((source.to_owned(), flags.to_owned())),
            _ => return Err(format!("microtypo-conformance --patterns printed {line}")),
        }
    }
    patterns.sort();

    let mut codegen = regress::codegen::Codegen::new();
    let (mut generated, mut interpreted, mut code) = (Vec::new(), Vec::new(), String::new());
    let mut factor_data = vec![0u8];
    let mut factor_bytes_at: HashMap<Vec<u8>, u32> = HashMap::new();
    let mut attempts: HashMap<(String, String), (u32, std::result::Result<String, String>)> = HashMap::new();
    let mut functions = 0;
    for (source, flags) in patterns {
        let attempt = (source.clone(), flags.replace(['g', 'y'], ""));
        if !attempts.contains_key(&attempt) {
            let units: Vec<u16> = source.encode_utf16().collect();
            let regex =
                regress::Regex::from_utf16_with(&units, flags.as_str(), regress::FactorSource::Analyze(usize::MAX))
                    .map_err(at(format!("/{source}/{flags}")))?;
            let factors = match regress::codegen::Codegen::factors(&regex) {
                Some(bytes) => {
                    let fresh = u32::try_from(factor_data.len()).map_err(|e| e.to_string())?;
                    *factor_bytes_at.entry(bytes).or_insert_with_key(|bytes| {
                        factor_data.extend_from_slice(bytes);
                        fresh
                    })
                }
                None => 0,
            };
            let name = format!("m{functions}");
            let shared = codegen.clone();
            let outcome = match codegen.matcher(&regex, &name, flags.contains('u') || flags.contains('v')) {
                Ok(matcher) if matcher.len() > MATCHER_SOURCE_LIMIT => {
                    codegen = shared;
                    Err(format!("{} bytes of source, over {MATCHER_SOURCE_LIMIT}", matcher.len()))
                }
                Ok(matcher) => {
                    code.push_str(&matcher);
                    functions += 1;
                    Ok(name)
                }
                Err(reason) => {
                    codegen = shared;
                    Err(reason)
                }
            };
            attempts.insert(attempt.clone(), (factors, outcome));
        }
        match &attempts[&attempt] {
            (factors, Ok(name)) => generated.push((source, flags, name.clone(), *factors)),
            (factors, Err(reason)) => interpreted.push((source, flags, reason.clone(), *factors)),
        }
    }
    code.push_str(&codegen.classes());
    let fresh = matchers_file(&generated, &interpreted, &factor_data, &code)?;
    if check {
        if fs::read_to_string(path).map_err(at(path.display()))? != fresh {
            return Err(format!("{MATCHERS} is stale: run `cargo xtask matchers`"));
        }
    } else {
        write(path, &fresh)?;
    }
    println!("{MATCHERS}: {} generated, {} interpreted", generated.len(), interpreted.len());
    for (source, flags, reason, _) in &interpreted {
        println!("  /{source}/{flags}: {reason}");
    }
    Ok(())
}

fn matchers_file(
    generated: &[(String, String, String, u32)],
    interpreted: &[(String, String, String, u32)],
    factor_data: &[u8],
    code: &str,
) -> Result<String> {
    let mut text = format!(
        "{MATCHERS_LINTS}\n\nuse regress::aot;\n\npub(super) static MATCHERS: &[(&str, &str, aot::Attempt, u32)] = &[\n"
    );
    text.extend(generated.iter().map(|(source, flags, name, at)| format!("({source:?}, {flags:?}, {name}, {at}),\n")));
    text.push_str("];\n\npub(super) static INTERPRETED: &[(&str, &str, &str)] = &[\n");
    text.extend(interpreted.iter().map(|(source, flags, reason, _)| format!("({source:?}, {flags:?}, {reason:?}),\n")));
    text.push_str("];\n\npub(super) static INTERPRETED_FACTORS: &[(&str, &str, u32)] = &[\n");
    text.extend(interpreted.iter().map(|(source, flags, _, at)| format!("({source:?}, {flags:?}, {at}),\n")));
    text.push_str("];\n\n");
    let bytes: Vec<String> = factor_data.iter().map(u8::to_string).collect();
    let _ = writeln!(text, "pub(super) static FACTOR_DATA: &[u8] = &[{}];\n", bytes.join(", "));
    text.push_str(code);
    let mut command = Command::new(tool("RUSTFMT", None, "rustfmt"));
    command.args(["--edition", "2024", "--emit", "stdout"]).current_dir(root());
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().map_err(at(show(&command)))?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let writer = std::thread::spawn(move || stdin.write_all(text.as_bytes()));
    let output = child.wait_with_output().map_err(at(show(&command)))?;
    writer.join().expect("writer finished").map_err(at(show(&command)))?;
    if !output.status.success() {
        return Err(format!("{} failed: {}", show(&command), output.status));
    }
    String::from_utf8(output.stdout).map_err(at(show(&command)))
}

fn target_dir() -> PathBuf {
    env::var_os("CARGO_TARGET_DIR").map_or_else(|| root().join("target"), |dir| root().join(dir))
}

fn conformance() -> PathBuf {
    target_dir().join("release/microtypo-conformance")
}

fn xcframework() -> Result<PathBuf> {
    let root = root();
    header(true)?;
    let mut libraries = Vec::new();
    for target in APPLE_TARGETS {
        run(cargo().env("MACOSX_DEPLOYMENT_TARGET", MACOS).env("SOURCE_DATE_EPOCH", SOURCE_DATE_EPOCH).args([
            "build",
            "-p",
            "microtypo-ffi",
            "--release",
            "--locked",
            "--target",
            target,
        ]))?;
        libraries.push(root.join(format!("target/{target}/release/libmicrotypo_ffi.a")));
    }

    let work = root.join("target/staging/xcframework");
    fresh_dir(&work)?;
    let headers = work.join("include/MicroTypoFFI");
    let include = root.join("crates/microtypo-ffi/include");
    for file in ["microtypo.h", "module.modulemap"] {
        copy(&include.join(file), &headers.join(file))?;
    }
    let library = work.join("libmicrotypo.a");
    run(Command::new("lipo").arg("-create").args(&libraries).arg("-output").arg(&library))?;

    let framework = root.join("packages/swift/MicroTypoFFI.xcframework");
    if framework.exists() {
        fs::remove_dir_all(&framework).map_err(at(framework.display()))?;
    }
    run(Command::new("xcodebuild")
        .arg("-create-xcframework")
        .arg("-library")
        .arg(&library)
        .arg("-headers")
        .arg(work.join("include"))
        .arg("-output")
        .arg(&framework))?;
    Ok(framework)
}

fn dist(dir: Option<&Path>) -> Result {
    let root = root();
    let version = version()?;
    let dest = dir_or_default(dir, &version)?;
    if dest.exists() && fs::read_dir(&dest).map_err(at(dest.display()))?.next().is_some() {
        return Err(format!("{} is not empty", dest.display()));
    }
    let out = root.join("target/staging/release");
    fresh_dir(&out)?;

    let stage = wasm()?;
    let wasm_gzip = gzip_size(&stage.join("microtypo.wasm"))?;
    if wasm_gzip > WASM_GZIP_BUDGET {
        return Err(format!("microtypo.wasm is {wasm_gzip} bytes in gzip, over the budget of {WASM_GZIP_BUDGET}"));
    }
    let framework = xcframework()?;

    let npm = format!("microtypo-v{version}-npm.tgz");
    pack(&stage, &out.join(&npm))?;

    let xcf = format!("microtypo-v{version}-xcframework.zip");
    zip(&framework, &out.join(&xcf))?;
    let checksum = capture(Command::new("swift").args(["package", "compute-checksum"]).arg(out.join(&xcf)))?;

    check_npm(&out.join(&npm))?;
    check_swift(&out.join(&xcf))?;

    let files = [npm.as_str(), xcf.as_str()];
    let sums = capture(Command::new("shasum").args(["-a", "256"]).args(files).current_dir(&out))?;
    write(&out.join("SHA256SUMS"), &format!("{sums}\n"))?;
    let sha = |name: &str| -> Result<String> {
        sums.lines()
            .find_map(|line| line.strip_suffix(name).map(|hash| hash.trim().to_owned()))
            .ok_or_else(|| format!("no SHA-256 for {name}"))
    };
    if checksum != sha(&xcf)? {
        return Err(format!("swift package compute-checksum {xcf} is {checksum}, not its SHA-256"));
    }
    let size = |name: &str| fs::metadata(out.join(name)).map(|m| m.len()).map_err(at(out.join(name).display()));

    let entries = [
        json!({
            "name": npm,
            "product": "npm",
            "channel": "npm",
            "package": "@meritt/microtypo-wasm",
            "wasm_gzip": wasm_gzip,
            "size": size(&npm)?,
            "sha256": sha(&npm)?,
        }),
        json!({
            "name": xcf,
        "product": "xcframework",
        "channel": "swiftpm",
        "module": "MicroTypoFFI",
        "targets": APPLE_TARGETS,
        "macos": MACOS,
        "header": "microtypo.h",
        "size": size(&xcf)?,
        "sha256": sha(&xcf)?,
        "swiftpm_checksum": checksum,
        }),
    ];
    let manifest = json!({
        "name": "microtypo",
        "version": version,
        "commit": capture(Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&root))?,
        "dirty": !capture(Command::new("git").args(["status", "--porcelain", "--untracked-files=no"]).current_dir(&root))?
            .is_empty(),
        "files": entries,
        "swift": {
            "package": "MicroTypo",
            "product": "MicroTypo",
            "macos": MACOS,
            "binary_target": format!("{RELEASES}/v{version}/{xcf}"),
            "checksum": checksum,
        },
        "channels": {
            "npm": "https://registry.npmjs.org",
            "swiftpm": "https://github.com/meritt/microtypo.rs",
            "crates.io": "microtypo-core",
            "git": format!("https://github.com/meritt/microtypo.rs.git#v{version}"),
        },
        "tools": {
            "rustc": capture(Command::new(env::var_os("RUSTC").unwrap_or_else(|| "rustc".into())).arg("-V").current_dir(&root))?,
            "wasm-bindgen": locked_version("wasm-bindgen")?,
            "binaryen": BINARYEN,
            "xcode": capture(Command::new("xcodebuild").arg("-version"))?.lines().next().unwrap_or_default(),
        },
    });
    let manifest = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    write(&out.join("release-manifest.json"), &format!("{manifest}\n"))?;

    fs::create_dir_all(&dest).map_err(at(dest.display()))?;
    for name in files.iter().copied().chain(["SHA256SUMS", "release-manifest.json"]) {
        copy(&out.join(name), &dest.join(name))?;
    }
    println!("{}", dest.display());
    Ok(())
}

fn pack(from: &Path, tgz: &Path) -> Result {
    let root = root();
    let staged = root.join("target/staging/npm");
    fresh_dir(&staged)?;
    copy(from, &staged)?;
    let pack = root.join("target/staging/pack");
    fresh_dir(&pack)?;
    run(Command::new("pnpm").args(["pack", "--pack-destination"]).arg(&pack).current_dir(&staged))?;
    let packed = fs::read_dir(&pack)
        .map_err(at(pack.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(at(pack.display()))?;
    let [archive] = packed.as_slice() else {
        return Err("pnpm pack did not produce one archive".to_owned());
    };
    copy(&archive.path(), tgz)
}

fn pin(check: bool, dir: Option<&Path>) -> Result {
    let version = version()?;
    let dir = dir_or_default(dir, &version)?;
    let path = dir.join("release-manifest.json");
    let manifest: Value =
        serde_json::from_slice(&fs::read(&path).map_err(at(path.display()))?).map_err(at(path.display()))?;
    if manifest["version"] != version.as_str() {
        return Err(format!("{} is not a set of version {version}", dir.display()));
    }
    let file = manifest["files"]
        .as_array()
        .and_then(|files| files.iter().find(|f| f["product"] == "xcframework"))
        .ok_or_else(|| format!("{} lists no xcframework", dir.display()))?;
    let (Some(name), Some(sha), Some(checksum)) =
        (file["name"].as_str(), file["sha256"].as_str(), file["swiftpm_checksum"].as_str())
    else {
        return Err(format!("{} has an incomplete xcframework entry", dir.display()));
    };
    if sha != checksum {
        return Err(format!("{name}: SwiftPM checksum {checksum} is not its SHA-256 {sha}"));
    }
    let path = root().join("Package.swift");
    let text = fs::read_to_string(&path).map_err(at(path.display()))?;
    let pinned = pin_swift(&text, &format!("{RELEASES}/v{version}/{name}"), checksum)?;
    if check {
        if pinned != text {
            return Err(format!("Package.swift does not pin {name} of {}: run `cargo xtask pin`", dir.display()));
        }
        return Ok(());
    }
    write(&path, &pinned)?;
    println!("Package.swift pins {name} {checksum}");
    Ok(())
}

fn dir_or_default(dir: Option<&Path>, version: &str) -> Result<PathBuf> {
    match dir {
        Some(dir) => std::path::absolute(dir).map_err(at(dir.display())),
        None => Ok(root().join(format!("dist/v{version}"))),
    }
}

fn pin_swift(text: &str, url: &str, checksum: &str) -> Result<String> {
    let mut found = (false, false);
    let pinned: Vec<String> = text
        .lines()
        .map(|line| {
            if line.starts_with("    url: \"") && !found.0 {
                found.0 = true;
                format!("    url: \"{url}\",")
            } else if line.starts_with("    checksum: \"") && !found.1 {
                found.1 = true;
                format!("    checksum: \"{checksum}\"")
            } else {
                line.to_owned()
            }
        })
        .collect();
    if found != (true, true) {
        return Err("Package.swift has no `let release = (url:, checksum:)` to pin".to_owned());
    }
    Ok(pinned.join("\n") + "\n")
}

fn check_npm(tgz: &Path) -> Result {
    let root = root();
    let consumer = root.join("target/staging/consumer/npm");
    fresh_dir(&consumer)?;
    write(&consumer.join("package.json"), "{ \"private\": true, \"type\": \"module\" }\n")?;
    copy(&root.join("packages/npm/pnpm-workspace.yaml"), &consumer.join("pnpm-workspace.yaml"))?;
    run(Command::new("pnpm").args(["add", "--offline", "--ignore-scripts"]).arg(tgz).current_dir(&consumer))?;
    copy(&root.join("packages/npm/test/consumer.mjs"), &consumer.join("consumer.mjs"))?;
    for source in ["url", "bytes", "module"] {
        run(Command::new("node").args(["consumer.mjs", source]).current_dir(&consumer))?;
    }
    run(Command::new("node")
        .arg(root.join("packages/npm/test/corpus.mjs"))
        .arg(consumer.join("node_modules/@meritt/microtypo-wasm/microtypo.js")))
}

fn check_swift(zip: &Path) -> Result {
    let root = root();
    let consumer = root.join("target/staging/consumer/swift");
    fresh_dir(&consumer)?;
    let package = consumer.join("microtypo.rs");
    for item in ["Package.swift", "packages/swift/Sources", "packages/swift/Tests"] {
        copy(&root.join(item), &package.join(item))?;
    }
    copy(zip, &package.join("MicroTypoFFI.xcframework.zip"))?;
    let manifest = package.join("Package.swift");
    let text = fs::read_to_string(&manifest).map_err(at(manifest.display()))?;
    let remote = r#".binaryTarget(name: "MicroTypoFFI", url: release.url, checksum: release.checksum)"#;
    if text.matches(remote).count() != 1 {
        return Err(format!("Package.swift needs one `{remote}`"));
    }
    write(
        &manifest,
        &text.replace(remote, r#".binaryTarget(name: "MicroTypoFFI", path: "MicroTypoFFI.xcframework.zip")"#),
    )?;
    run(Command::new("swift").arg("test").arg("--package-path").arg(&package))?;

    let app = consumer.join("app");
    write(
        &app.join("Package.swift"),
        "// swift-tools-version: 6.2\n\
         import PackageDescription\n\n\
         let package = Package(\n    \
             name: \"App\",\n    \
             platforms: [.macOS(.v26)],\n    \
             dependencies: [.package(path: \"../microtypo.rs\")],\n    \
             targets: [.executableTarget(name: \"App\", dependencies: [.product(name: \"MicroTypo\", package: \"microtypo.rs\")])]\n\
         )\n",
    )?;
    write(
        &app.join("Sources/App/main.swift"),
        "import MicroTypo\n\n\
         let text = try MicroTypo(config: [\"html\": false]).process(\"\\\"Амбер\\\" - город\")\n\
         guard text == \"«Амбер»\\u{A0}— город\" else { fatalError(\"unexpected \\(text)\") }\n\
         print(\"MicroTypo \\(MicroTypo.version): the package resolves and runs\")\n",
    )?;
    run(Command::new("swift").arg("run").arg("--package-path").arg(&app))
}

fn zip(item: &Path, to: &Path) -> Result {
    let epoch: u64 = SOURCE_DATE_EPOCH.parse().map_err(|e| format!("SOURCE_DATE_EPOCH: {e}"))?;
    let times = fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(epoch));
    let parent = item.parent().ok_or_else(|| format!("{} has no parent", item.display()))?;
    let mut files = Vec::new();
    let mut stack = vec![item.to_path_buf()];
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            for entry in fs::read_dir(&path).map_err(at(path.display()))? {
                stack.push(entry.map_err(at(path.display()))?.path());
            }
        } else {
            fs::File::open(&path).and_then(|file| file.set_times(times)).map_err(at(path.display()))?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).map_err(at(path.display()))?;
            let name = path.strip_prefix(parent).map_err(at(path.display()))?;
            files.push(name.to_str().ok_or_else(|| format!("{} is not UTF-8", path.display()))?.to_owned());
        }
    }
    files.sort();

    let mut command = Command::new("zip");
    command.args(["-X", "-D", "-q", "-@"]).arg(to).current_dir(parent).env("TZ", "UTC").stdin(Stdio::piped());
    eprintln!("+ {}", show(&command));
    let mut child = command.spawn().map_err(at(show(&command)))?;
    let mut stdin = child.stdin.take().ok_or("zip has no stdin")?;
    stdin.write_all(format!("{}\n", files.join("\n")).as_bytes()).map_err(at(show(&command)))?;
    drop(stdin);
    let status = child.wait().map_err(at(show(&command)))?;
    if status.success() { Ok(()) } else { Err(format!("{} failed: {status}", show(&command))) }
}

fn gzip_size(path: &Path) -> Result<u64> {
    let mut command = Command::new("gzip");
    command.arg("-9c").arg(path);
    let output = command.stderr(Stdio::inherit()).output().map_err(at(show(&command)))?;
    if !output.status.success() {
        return Err(format!("{} failed: {}", show(&command), output.status));
    }
    Ok(output.stdout.len() as u64)
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask sits in the workspace root").to_path_buf()
}

fn version() -> Result<String> {
    let text = capture(cargo().args(["metadata", "--format-version", "1", "--no-deps", "--locked"]))?;
    let metadata: Value = serde_json::from_str(&text).map_err(|e| format!("cargo metadata: {e}"))?;
    metadata["packages"]
        .as_array()
        .and_then(|packages| packages.iter().find(|p| p["name"] == "microtypo-core"))
        .and_then(|p| p["version"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| "cargo metadata has no microtypo-core".to_owned())
}

fn locked_version(name: &str) -> Result<String> {
    let id = capture(cargo().args(["pkgid", "--locked", "-p", name]))?;
    id.rsplit_once('@').map(|(_, version)| version.to_owned()).ok_or_else(|| format!("cargo pkgid {name}: {id}"))
}

fn cargo() -> Command {
    let mut command = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.current_dir(root());
    command
}

fn tool(var: &str, local: Option<PathBuf>, name: &str) -> PathBuf {
    env::var_os(var).map(PathBuf::from).or_else(|| local.filter(|path| path.is_file())).unwrap_or_else(|| name.into())
}

fn run(command: &mut Command) -> Result {
    eprintln!("+ {}", show(command));
    let status = command.status().map_err(at(show(command)))?;
    if status.success() { Ok(()) } else { Err(format!("{} failed: {status}", show(command))) }
}

fn capture(command: &mut Command) -> Result<String> {
    let output = command.stderr(Stdio::inherit()).output().map_err(at(show(command)))?;
    if !output.status.success() {
        return Err(format!("{} failed: {}", show(command), output.status));
    }
    String::from_utf8(output.stdout).map(|s| s.trim().to_owned()).map_err(at(show(command)))
}

fn show(command: &Command) -> String {
    let mut words = vec![command.get_program().to_string_lossy()];
    words.extend(command.get_args().map(|arg| arg.to_string_lossy()));
    words.join(" ")
}

fn fresh_dir(path: &Path) -> Result {
    if path.exists() {
        fs::remove_dir_all(path).map_err(at(path.display()))?;
    }
    fs::create_dir_all(path).map_err(at(path.display()))
}

fn copy(from: &Path, to: &Path) -> Result {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(at(parent.display()))?;
    }
    if from.is_dir() {
        fs::create_dir_all(to).map_err(at(to.display()))?;
        for entry in fs::read_dir(from).map_err(at(from.display()))? {
            let entry = entry.map_err(at(from.display()))?;
            copy(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(from, to).map(drop).map_err(at(from.display()))
    }
}

fn write(path: &Path, data: &(impl AsRef<[u8]> + ?Sized)) -> Result {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(at(parent.display()))?;
    }
    fs::write(path, data).map_err(at(path.display()))
}

fn at<E: Display>(what: impl Display) -> impl FnOnce(E) -> String {
    let what = what.to_string();
    move |error| format!("{what}: {error}")
}
