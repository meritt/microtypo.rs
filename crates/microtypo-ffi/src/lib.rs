#![expect(unsafe_code, reason = "a C ABI takes and returns raw pointers")]

use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::LazyLock;
use std::time::Instant;

use microtypo_core::Host;
use serde_json::Value;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

const HOST: Host = Host { now_ms, fill_random };

fn now_ms() -> f64 {
    static START: LazyLock<Instant> = LazyLock::new(Instant::now);
    START.elapsed().as_secs_f64() * 1000.0
}

fn fill_random(bytes: &mut [u8]) {
    getrandom::fill(bytes).expect("the system CSPRNG is available");
}

#[derive(Debug)]
pub struct Engine(microtypo_core::MicroTypo);

#[derive(Debug)]
pub struct Error {
    name: CString,
    message: CString,
    code: Option<CString>,
    details: Option<CString>,
}

#[repr(C)]
#[derive(Debug)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub units: *mut u16,
    pub len: usize,
}

#[repr(C)]
#[derive(Debug)]
pub struct Trace {
    pub rule: *mut u16,
    pub rule_len: usize,
    pub kind: u8,
    pub count: usize,
    pub ranges: *mut usize,
    pub ranges_len: usize,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct Output {
    pub units: *mut u16,
    pub len: usize,
    pub changes: *mut usize,
    pub changes_len: usize,
    pub edits: *mut Edit,
    pub edits_len: usize,
    pub trace: *mut Trace,
    pub trace_len: usize,
}

fn c_string(s: &str) -> CString {
    CString::new(s.replace('\0', "\u{FFFD}")).expect("no interior NUL")
}

impl Error {
    fn from_engine(e: &microtypo_core::Error) -> Error {
        Error {
            name: c_string(e.name()),
            message: c_string(&e.message),
            code: e.code().map(c_string),
            details: (!e.details.is_null()).then(|| c_string(&e.details.to_string())),
        }
    }

    fn internal(message: &str) -> Error {
        Error { name: c_string("InternalError"), message: c_string(message), code: None, details: None }
    }
}

/// # Safety
/// `slot` is `NULL` or writable.
unsafe fn report(slot: *mut *mut Error, error: Error) {
    if !slot.is_null() {
        // SAFETY: `slot` is not NULL, and the caller promises it is writable.
        unsafe { *slot = Box::into_raw(Box::new(error)) };
    }
}

/// # Safety
/// `slot` is `NULL` or writable.
unsafe fn guarded<T>(slot: *mut *mut Error, f: impl FnOnce() -> Result<T, Error>) -> Option<T> {
    if !slot.is_null() {
        // SAFETY: `slot` is not NULL, and the caller promises it is writable.
        unsafe { *slot = ptr::null_mut() };
    }
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(value)) => Some(value),
        Ok(Err(error)) => {
            // SAFETY: as this function's own contract.
            unsafe { report(slot, error) };
            None
        }
        Err(panic) => {
            let message = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_else(|| "panic".to_owned());
            // SAFETY: as this function's own contract.
            unsafe { report(slot, Error::internal(&message)) };
            None
        }
    }
}

/// # Safety
/// `json` is `NULL` or NUL-terminated.
unsafe fn read_json(json: *const c_char, what: &str) -> Result<Option<Value>, Error> {
    if json.is_null() {
        return Ok(None);
    }
    // SAFETY: not NULL, and the caller promises a NUL-terminated string.
    let text = unsafe { CStr::from_ptr(json) }
        .to_str()
        .map_err(|_| Error::from_engine(&microtypo_core::Error::type_error(format!("{what} must be UTF-8"))))?;
    serde_json::from_str(text)
        .map(Some)
        .map_err(|e| Error::from_engine(&microtypo_core::Error::type_error(format!("{what} must be JSON: {e}"))))
}

/// # Safety
/// `config_json` is `NULL` or NUL-terminated; `error` is `NULL` or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn microtypo_engine_new(config_json: *const c_char, error: *mut *mut Error) -> *mut Engine {
    // SAFETY: `error` and `config_json` are as this function's contract states.
    unsafe {
        guarded(error, || {
            if config_json.is_null() {
                return microtypo_core::MicroTypo::new(None, HOST).map_err(|e| Error::from_engine(&e));
            }
            let config = CStr::from_ptr(config_json)
                .to_str()
                .map_err(|_| Error::from_engine(&microtypo_core::Error::type_error("config must be UTF-8")))?;
            let result = microtypo_core::MicroTypo::from_json(config, HOST);
            if result.as_ref().is_err_and(|e| e.details.get("reason").and_then(Value::as_str) == Some("json")) {
                read_json(config_json, "config")?;
            }
            result.map_err(|e| Error::from_engine(&e))
        })
    }
    .map_or(ptr::null_mut(), |engine| Box::into_raw(Box::new(Engine(engine))))
}

/// # Safety
/// `engine` is `NULL` or came from `microtypo_engine_new` and is not used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn microtypo_engine_free(engine: *mut Engine) {
    if !engine.is_null() {
        // SAFETY: the pointer came from `Box::into_raw` in `microtypo_engine_new` and is freed once.
        drop(unsafe { Box::from_raw(engine) });
    }
}

/// # Safety
/// `engine` is `NULL` or live and used by no other thread during the call; `method` is `NULL` or NUL-terminated; `args_json` is `NULL` (no arguments) or NUL-terminated; `error` is `NULL` or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn microtypo_engine_call(
    engine: *mut Engine,
    method: *const c_char,
    args_json: *const c_char,
    error: *mut *mut Error,
) -> bool {
    // SAFETY: every pointer is as this function's contract states, and `engine` is borrowed exclusively only for the call.
    unsafe {
        guarded(error, || {
            let engine = engine.as_mut().ok_or_else(|| Error::internal("engine is NULL"))?;
            if method.is_null() {
                return Err(Error::internal("method is NULL"));
            }
            let method = CStr::from_ptr(method).to_str().map_err(|_| Error::internal("method must be UTF-8"))?;
            if args_json.is_null() {
                return engine.0.call(method, &[]).map_err(|e| Error::from_engine(&e));
            }
            let args = CStr::from_ptr(args_json)
                .to_str()
                .map_err(|_| Error::from_engine(&microtypo_core::Error::type_error("args must be UTF-8")))?;
            if !args.trim_start().starts_with('[') {
                read_json(args_json, "args")?;
                return Err(Error::internal("args must be a JSON array"));
            }
            let result = engine.0.call_json(method, args);
            if result.as_ref().is_err_and(|e| e.details.get("reason").and_then(Value::as_str) == Some("json")) {
                read_json(args_json, "args")?;
            }
            result.map_err(|e| Error::from_engine(&e))
        })
    }
    .is_some()
}

/// # Safety
/// `engine` is `NULL` or live and used by no other thread during the call; `text` points to `len` units unless `len` is 0; `out` is `NULL` or writable, and an output still in it leaks; `error` is `NULL` or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn microtypo_engine_process(
    engine: *mut Engine,
    text: *const u16,
    len: usize,
    with_changes: bool,
    out: *mut Output,
    error: *mut *mut Error,
) -> bool {
    // SAFETY: every pointer is as this function's contract states; `text` is read only when `len` is not 0, and `engine` and `out` are borrowed exclusively only for the call.
    unsafe {
        guarded(error, || {
            let engine = engine.as_mut().ok_or_else(|| Error::internal("engine is NULL"))?;
            let out = out.as_mut().ok_or_else(|| Error::internal("out is NULL"))?;
            let text = if len == 0 { &[][..] } else { std::slice::from_raw_parts(text, len) };
            *out = process_owned(
                engine,
                text,
                microtypo_core::ProcessOptions { changes: with_changes, ..microtypo_core::ProcessOptions::default() },
            )?;
            Ok(())
        })
    }
    .is_some()
}

/// # Safety
/// As `microtypo_engine_process`; `options_json` is `NULL` or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn microtypo_engine_process_diagnostics(
    engine: *mut Engine,
    text: *const u16,
    len: usize,
    options_json: *const c_char,
    out: *mut Output,
    error: *mut *mut Error,
) -> bool {
    // SAFETY: every pointer is as this function's contract states; `text` is read only when `len` is not 0, and `engine` and `out` are borrowed exclusively only for the call.
    unsafe {
        guarded(error, || {
            let engine = engine.as_mut().ok_or_else(|| Error::internal("engine is NULL"))?;
            let out = out.as_mut().ok_or_else(|| Error::internal("out is NULL"))?;
            let text = if len == 0 { &[][..] } else { std::slice::from_raw_parts(text, len) };
            let options = read_json(options_json, "options")?;
            let options = microtypo_core::ProcessOptions::read(options.as_ref()).map_err(|e| Error::from_engine(&e))?;
            *out = process_owned(engine, text, options)?;
            Ok(())
        })
    }
    .is_some()
}

fn process_owned(engine: &mut Engine, text: &[u16], options: microtypo_core::ProcessOptions) -> Result<Output, Error> {
    let output = engine.0.process_with_diagnostics(text, options).map_err(|e| Error::from_engine(&e))?;
    let (units, len) = into_raw(output.text);
    let (changes, changes_flat) =
        into_raw(output.changes.unwrap_or_default().into_iter().flat_map(|(a, b)| [a, b]).collect());
    let edits = output
        .edits
        .unwrap_or_default()
        .into_iter()
        .map(|edit| {
            let (units, len) = into_raw(edit.text);
            Edit { start: edit.start, end: edit.end, units, len }
        })
        .collect();
    let (edits, edits_len) = into_raw(edits);
    let trace = output
        .trace
        .unwrap_or_default()
        .into_iter()
        .map(|entry| {
            let (rule, rule_len) = into_raw(entry.rule);
            let (ranges, ranges_flat) = into_raw(entry.ranges.into_iter().flat_map(|(a, b)| [a, b]).collect());
            Trace {
                rule,
                rule_len,
                kind: match entry.kind {
                    microtypo_core::TraceKind::Regex => 0,
                    microtypo_core::TraceKind::Handler => 1,
                },
                count: entry.count,
                ranges,
                ranges_len: ranges_flat / 2,
            }
        })
        .collect();
    let (trace, trace_len) = into_raw(trace);
    Ok(Output { units, len, changes, changes_len: changes_flat / 2, edits, edits_len, trace, trace_len })
}

fn into_raw<T>(v: Vec<T>) -> (*mut T, usize) {
    let boxed = v.into_boxed_slice();
    let len = boxed.len();
    (Box::into_raw(boxed).cast::<T>(), len)
}

/// # Safety
/// `out` is `NULL`, zeroed, or filled by `microtypo_engine_process` or `microtypo_engine_process_diagnostics` with its buffers not freed yet; the call zeroes `*out`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn microtypo_output_free(out: *mut Output) {
    // SAFETY: `out` is NULL or a live `Output`, as this function's contract states.
    let Some(out) = (unsafe { out.as_mut() }) else { return };
    // SAFETY: every buffer came from `into_raw` with these lengths and is freed once: `*out` is zeroed below.
    unsafe {
        if !out.units.is_null() {
            drop(Box::from_raw(ptr::slice_from_raw_parts_mut(out.units, out.len)));
        }
        if !out.changes.is_null() {
            drop(Box::from_raw(ptr::slice_from_raw_parts_mut(out.changes, out.changes_len * 2)));
        }
        if !out.edits.is_null() {
            for edit in Box::from_raw(ptr::slice_from_raw_parts_mut(out.edits, out.edits_len)) {
                if !edit.units.is_null() {
                    drop(Box::from_raw(ptr::slice_from_raw_parts_mut(edit.units, edit.len)));
                }
            }
        }
        if !out.trace.is_null() {
            for entry in Box::from_raw(ptr::slice_from_raw_parts_mut(out.trace, out.trace_len)) {
                if !entry.rule.is_null() {
                    drop(Box::from_raw(ptr::slice_from_raw_parts_mut(entry.rule, entry.rule_len)));
                }
                if !entry.ranges.is_null() {
                    drop(Box::from_raw(ptr::slice_from_raw_parts_mut(entry.ranges, entry.ranges_len * 2)));
                }
            }
        }
    }
    *out = Output::default();
}

/// # Safety
/// `error` came from this library and is not freed yet.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn microtypo_error_name(error: *const Error) -> *const c_char {
    // SAFETY: `error` is a live `Error`, as this function's contract states.
    unsafe { (*error).name.as_ptr() }
}

/// # Safety
/// As `microtypo_error_name`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn microtypo_error_message(error: *const Error) -> *const c_char {
    // SAFETY: `error` is a live `Error`, as this function's contract states.
    unsafe { (*error).message.as_ptr() }
}

/// # Safety
/// As `microtypo_error_name`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn microtypo_error_code(error: *const Error) -> *const c_char {
    // SAFETY: `error` is a live `Error`, as this function's contract states.
    unsafe { (*error).code.as_ref().map_or(ptr::null(), |c| c.as_ptr()) }
}

/// # Safety
/// As `microtypo_error_name`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn microtypo_error_details(error: *const Error) -> *const c_char {
    // SAFETY: `error` is a live `Error`, as this function's contract states.
    unsafe { (*error).details.as_ref().map_or(ptr::null(), |c| c.as_ptr()) }
}

/// # Safety
/// `error` is `NULL` or came from this library and is not used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn microtypo_error_free(error: *mut Error) {
    if !error.is_null() {
        // SAFETY: the pointer came from `Box::into_raw` in `report` and is freed once.
        drop(unsafe { Box::from_raw(error) });
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn microtypo_version() -> *const c_char {
    static VERSION: LazyLock<CString> = LazyLock::new(|| c_string(microtypo_core::VERSION));
    VERSION.as_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_owns_nested_utf16_buffers_and_recovers_after_errors() {
        let config=c"{\"presets\":false,\"input\":{\"format\":\"plain\",\"placeholders\":\"off\",\"pragmas\":\"off\",\"attributes\":[]},\"maxProcessingMs\":0,\"ruleGroups\":[{\"name\":\"custom\\u0000group\",\"rules\":[{\"id\":\"replace\",\"pattern\":{\"source\":\"a\",\"flags\":\"g\"},\"replacement\":\"b\"}]}]}";
        let mut error = ptr::null_mut();
        // SAFETY: config is NUL-terminated and error is a writable local slot.
        let engine = unsafe { microtypo_engine_new(config.as_ptr(), &raw mut error) };
        assert!(!engine.is_null());
        let input = [0xd83d, 0xde00, 97, 97];
        let mut out = Output::default();
        // SAFETY: engine is live, input covers its length, and both output slots are writable.
        unsafe {
            assert!(microtypo_engine_process_diagnostics(
                engine,
                input.as_ptr(),
                input.len(),
                c"{\"changes\":true,\"edits\":true,\"trace\":true}".as_ptr(),
                &raw mut out,
                &raw mut error
            ));
            assert!(error.is_null());
            assert_eq!(std::slice::from_raw_parts(out.units, out.len), [0xd83d, 0xde00, 98, 98]);
            let trace = std::slice::from_raw_parts(out.trace, out.trace_len);
            assert_eq!(trace.len(), 1);
            assert_eq!(trace[0].kind, 0);
            assert_eq!(trace[0].count, 2);
            assert_eq!(
                String::from_utf16(std::slice::from_raw_parts(trace[0].rule, trace[0].rule_len)).unwrap(),
                "custom\0group.replace"
            );
            assert_eq!(std::slice::from_raw_parts(trace[0].ranges, trace[0].ranges_len * 2), [2, 3, 3, 4]);
            let mut restored = Vec::new();
            let mut cursor = 0;
            for edit in std::slice::from_raw_parts(out.edits, out.edits_len) {
                restored.extend_from_slice(&input[cursor..edit.start]);
                restored.extend_from_slice(std::slice::from_raw_parts(edit.units, edit.len));
                cursor = edit.end;
            }
            restored.extend_from_slice(&input[cursor..]);
            assert_eq!(restored, std::slice::from_raw_parts(out.units, out.len));
            microtypo_output_free(&raw mut out);
            assert!(out.units.is_null() && out.edits.is_null() && out.trace.is_null());
            assert!(!microtypo_engine_process_diagnostics(
                engine,
                input.as_ptr(),
                input.len(),
                c"{\"trace\":\"yes\"}".as_ptr(),
                &raw mut out,
                &raw mut error
            ));
            assert_eq!(CStr::from_ptr(microtypo_error_code(error)).to_str().unwrap(), "ERR_MICROTYPO_CONFIG");
            microtypo_error_free(error);
            assert!(microtypo_engine_process_diagnostics(
                engine,
                ptr::null(),
                0,
                c"{\"trace\":true,\"edits\":true}".as_ptr(),
                &raw mut out,
                &raw mut error
            ));
            assert!(error.is_null());
            assert_eq!((out.len, out.edits_len, out.trace_len), (0, 0, 0));
            microtypo_output_free(&raw mut out);
            assert!(microtypo_engine_call(engine,c"registerRuleGroup".as_ptr(),c"[{\"name\":\"custom\\ud800\",\"rules\":[{\"id\":\"r\\udc00\",\"pattern\":{\"source\":\"b\",\"flags\":\"g\"},\"replacement\":\"\\ud800\"}]}]".as_ptr(),&raw mut error));
            assert!(microtypo_engine_process_diagnostics(
                engine,
                [97].as_ptr(),
                1,
                c"{\"trace\":true,\"edits\":true}".as_ptr(),
                &raw mut out,
                &raw mut error
            ));
            assert_eq!(std::slice::from_raw_parts(out.units, out.len), [0xd800]);
            let trace = std::slice::from_raw_parts(out.trace, out.trace_len);
            assert_eq!(
                std::slice::from_raw_parts(trace[1].rule, trace[1].rule_len),
                [99, 117, 115, 116, 111, 109, 0xd800, 46, 114, 0xdc00]
            );
            let edits = std::slice::from_raw_parts(out.edits, out.edits_len);
            assert_eq!(std::slice::from_raw_parts(edits[0].units, edits[0].len), [0xd800]);
            microtypo_output_free(&raw mut out);
            microtypo_engine_free(engine);
        }
    }

    fn process(engine: *mut Engine, text: &str, with_changes: bool) -> Result<(String, Vec<usize>), String> {
        let units: Vec<u16> = text.encode_utf16().collect();
        let mut out = Output::default();
        let mut error = ptr::null_mut();
        // SAFETY: a live engine, `len` units at `text`, and local slots; the output is read within its lengths before it is freed.
        unsafe {
            if !microtypo_engine_process(
                engine,
                units.as_ptr(),
                units.len(),
                with_changes,
                &raw mut out,
                &raw mut error,
            ) {
                let message = CStr::from_ptr(microtypo_error_message(error)).to_string_lossy().into_owned();
                microtypo_error_free(error);
                return Err(message);
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(out.units, out.len));
            let changes = if out.changes_len == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(out.changes, out.changes_len * 2).to_vec()
            };
            microtypo_output_free(&raw mut out);
            Ok((text, changes))
        }
    }

    #[test]
    fn round_trip() {
        let config = c"{\"presets\":false,\"input\":{\"format\":\"plain\",\"placeholders\":\"off\",\"pragmas\":\"off\",\"attributes\":[]},\"maxProcessingMs\":0,\"ruleGroups\":[{\"name\":\"custom\",\"rules\":[{\"id\":\"nbsp\",\"pattern\":{\"source\":\"\\u00a0\",\"flags\":\"g\"},\"replacement\":\"&nbsp;\"}]}]}";
        let mut error = ptr::null_mut();
        // SAFETY: a C string literal and a local slot.
        let engine = unsafe { microtypo_engine_new(config.as_ptr(), &raw mut error) };
        assert!(!engine.is_null() && error.is_null());

        assert_eq!(process(engine, "\"Амбер\" - город", false).unwrap().0, "\"Амбер\" - город");
        assert_eq!(process(engine, "", true).unwrap(), (String::new(), Vec::new()));
        let (text, changes) = process(engine, "a\u{A0}b", true).unwrap();
        assert_eq!(text, "a&nbsp;b");
        assert_eq!(changes, vec![1, 7]);

        // SAFETY: a live engine, C string literals and a local slot.
        assert!(!unsafe {
            microtypo_engine_call(engine, c"setLayout".as_ptr(), c"[\"none\"]".as_ptr(), &raw mut error)
        });
        // SAFETY: the error the call above stored, read before it is freed; the engine is freed once.
        unsafe {
            assert_eq!(CStr::from_ptr(microtypo_error_name(error)).to_str().unwrap(), "MicroTypoConfigError");
            assert_eq!(CStr::from_ptr(microtypo_error_code(error)).to_str().unwrap(), "ERR_MICROTYPO_CONFIG");
            assert_eq!(CStr::from_ptr(microtypo_error_details(error)).to_str().unwrap(), "{\"layout\":\"none\"}");
            microtypo_error_free(error);

            assert!(!microtypo_engine_call(engine, ptr::null(), ptr::null(), &raw mut error));
            assert_eq!(CStr::from_ptr(microtypo_error_message(error)).to_str().unwrap(), "method is NULL");
            microtypo_error_free(error);
            microtypo_engine_free(engine);
        }
    }

    #[test]
    fn config_errors() {
        // SAFETY: a C string literal and no error slot.
        assert!(unsafe { microtypo_engine_new(c"{\"html\":\"yes\"}".as_ptr(), ptr::null_mut()) }.is_null());
        let mut error = ptr::null_mut();
        // SAFETY: a C string literal and a local slot.
        let engine = unsafe { microtypo_engine_new(c"{\"html\":\"yes\"}".as_ptr(), &raw mut error) };
        assert!(engine.is_null());
        // SAFETY: the error the call above stored, read before it is freed.
        unsafe {
            assert_eq!(
                CStr::from_ptr(microtypo_error_message(error)).to_str().unwrap(),
                "invalid config: html: expected a boolean"
            );
            microtypo_error_free(error);
        }
        // SAFETY: the version is a static NUL-terminated string.
        assert_eq!(unsafe { CStr::from_ptr(microtypo_version()) }.to_str().unwrap(), microtypo_core::VERSION);
    }

    #[test]
    fn a_panic_becomes_an_internal_error() {
        let mut error = ptr::null_mut();
        // SAFETY: a local slot.
        let value: Option<()> = unsafe { guarded(&raw mut error, || panic!("boom")) };
        assert!(value.is_none());
        // SAFETY: the error `guarded` stored, read before it is freed.
        unsafe {
            assert_eq!(CStr::from_ptr(microtypo_error_name(error)).to_str().unwrap(), "InternalError");
            assert_eq!(CStr::from_ptr(microtypo_error_message(error)).to_str().unwrap(), "boom");
            assert!(microtypo_error_code(error).is_null() && microtypo_error_details(error).is_null());
            microtypo_error_free(error);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "a run past a 200 ms budget takes hours under Miri")]
    fn the_budget_ends_a_catastrophic_input() {
        let config = c"{\"input\":\"markdown\",\"maxProcessingMs\":200,\"maxInputLength\":60000}";
        let mut error = ptr::null_mut();
        // SAFETY: a C string literal and a local slot.
        let engine = unsafe { microtypo_engine_new(config.as_ptr(), &raw mut error) };
        assert!(!engine.is_null() && error.is_null());
        let started = Instant::now();
        let message = process(engine, &" ".repeat(60_000), false).unwrap_err();
        assert!(started.elapsed().as_millis() < 400, "the budget error came after {:?}", started.elapsed());
        assert!(message.starts_with("Processing exceeded 200ms budget at "), "{message}");
        // SAFETY: the engine is freed once.
        unsafe { microtypo_engine_free(engine) };
    }
}
