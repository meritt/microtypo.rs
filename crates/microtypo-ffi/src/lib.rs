#![expect(unsafe_code, reason = "a C ABI takes and returns raw pointers")]

use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::LazyLock;

use microtypo_core::api;
use serde_json::Value;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

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
pub struct Output {
    pub units: *mut u16,
    pub len: usize,
    pub changes: *mut usize,
    pub changes_len: usize,
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
/// `json` is `NULL` or a NUL-terminated string that stays valid for the call.
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
/// `config_json` is `NULL` or a NUL-terminated string; `error` is `NULL` or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn microtypo_engine_new(config_json: *const c_char, error: *mut *mut Error) -> *mut Engine {
    // SAFETY: `error` and `config_json` are as this function's contract states.
    unsafe {
        guarded(error, || {
            let config = read_json(config_json, "config")?;
            microtypo_core::MicroTypo::new(config.as_ref()).map_err(|e| Error::from_engine(&e))
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
/// `engine` is live and used by no other thread meanwhile; `method` is `NULL` or NUL-terminated;
/// `args_json` is `NULL` (no arguments) or NUL-terminated; `error` is `NULL` or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn microtypo_engine_call(
    engine: *mut Engine,
    method: *const c_char,
    args_json: *const c_char,
    error: *mut *mut Error,
) -> bool {
    // SAFETY: every pointer is as this function's contract states, and `engine` is borrowed
    // exclusively only for the call.
    unsafe {
        guarded(error, || {
            let engine = engine.as_mut().ok_or_else(|| Error::internal("engine is NULL"))?;
            if method.is_null() {
                return Err(Error::internal("method is NULL"));
            }
            let method = CStr::from_ptr(method).to_str().map_err(|_| Error::internal("method must be UTF-8"))?;
            let args = match read_json(args_json, "args")? {
                None => Vec::new(),
                Some(Value::Array(args)) => args,
                Some(_) => return Err(Error::internal("args must be a JSON array")),
            };
            engine.0.call(method, &args).map_err(|e| Error::from_engine(&e))
        })
    }
    .is_some()
}

/// # Safety
/// `engine` is live and used by no other thread meanwhile; `text` points to `len` units (or is
/// anything when `len` is 0); `out` is writable; `error` is `NULL` or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn microtypo_engine_process(
    engine: *mut Engine,
    text: *const u16,
    len: usize,
    with_changes: bool,
    out: *mut Output,
    error: *mut *mut Error,
) -> bool {
    // SAFETY: every pointer is as this function's contract states; `text` is read only when
    // `len` is not 0, and `engine` and `out` are borrowed exclusively only for the call.
    unsafe {
        guarded(error, || {
            let engine = engine.as_mut().ok_or_else(|| Error::internal("engine is NULL"))?;
            let out = out.as_mut().ok_or_else(|| Error::internal("out is NULL"))?;
            let text = if len == 0 { &[][..] } else { std::slice::from_raw_parts(text, len) };
            let options = with_changes.then(|| serde_json::json!({ "changes": true }));
            let (units, changes) = match engine.0.process_with_options(text, options.as_ref()) {
                Ok(api::Output::Text(t)) => (t, Vec::new()),
                Ok(api::Output::WithChanges(p)) => (p.text, p.changes.iter().flat_map(|&(a, b)| [a, b]).collect()),
                Err(e) => return Err(Error::from_engine(&e)),
            };
            let (units, len) = into_raw(units);
            let (changes, flat) = into_raw(changes);
            *out = Output { units, len, changes, changes_len: flat / 2 };
            Ok(())
        })
    }
    .is_some()
}

fn into_raw<T>(v: Vec<T>) -> (*mut T, usize) {
    let boxed = v.into_boxed_slice();
    let len = boxed.len();
    (Box::into_raw(boxed).cast::<T>(), len)
}

/// # Safety
/// `out` is `NULL` or was filled by `microtypo_engine_process` and not freed yet.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn microtypo_output_free(out: *mut Output) {
    // SAFETY: `out` is NULL or a live `Output`, as this function's contract states.
    let Some(out) = (unsafe { out.as_mut() }) else { return };
    // SAFETY: both buffers came from `into_raw` with these lengths, and are freed once: the
    // pointers are cleared below.
    unsafe {
        if !out.units.is_null() {
            drop(Box::from_raw(ptr::slice_from_raw_parts_mut(out.units, out.len)));
        }
        if !out.changes.is_null() {
            drop(Box::from_raw(ptr::slice_from_raw_parts_mut(out.changes, out.changes_len * 2)));
        }
    }
    *out = Output { units: ptr::null_mut(), len: 0, changes: ptr::null_mut(), changes_len: 0 };
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

    fn process(engine: *mut Engine, text: &str, with_changes: bool) -> Result<(String, Vec<usize>), String> {
        let units: Vec<u16> = text.encode_utf16().collect();
        let mut out = Output { units: ptr::null_mut(), len: 0, changes: ptr::null_mut(), changes_len: 0 };
        let mut error = ptr::null_mut();
        // SAFETY: a live engine, `len` units at `text`, and local slots; the output is read within
        // its lengths before it is freed.
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
        let config = c"{\"html\":false,\"maxProcessingMs\":0}";
        let mut error = ptr::null_mut();
        // SAFETY: a C string literal and a local slot.
        let engine = unsafe { microtypo_engine_new(config.as_ptr(), &raw mut error) };
        assert!(!engine.is_null() && error.is_null());

        assert_eq!(process(engine, "\"Амбер\" - город", false).unwrap().0, "«Амбер»\u{A0}— город");
        let (text, changes) = process(engine, "a - b", true).unwrap();
        assert_eq!(text, "a\u{A0}— b");
        assert_eq!(changes, vec![1, 3]);

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
}
