mod matchers;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use super::{JsString, is_high, is_low};

/// An ECMAScript regular expression over UTF-16 code units, as `new RegExp(source, flags)`.
#[derive(Clone)]
pub struct Re {
    inner: Arc<Compiled>,
    #[doc(hidden)]
    pub global: bool,
    #[doc(hidden)]
    pub sticky: bool,
    #[doc(hidden)]
    pub unicode: bool,
    named: bool,
    builtin: bool,
}

struct Compiled {
    regex: regress::Regex,
    source: String,
    source_units: Option<JsString>,
    flags: String,
    builtin: AtomicBool,
}

impl std::fmt::Debug for Compiled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "/{}/{}", self.source, self.flags)
    }
}

impl std::fmt::Debug for Re {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "/{}/{}", self.source(), self.flags())
    }
}

const INLINE_GROUPS: usize = 8;

type Slot = Option<Range<usize>>;

type Search<'s> = dyn FnMut(usize, &mut [Slot]) -> Option<Range<usize>> + 's;

/// A match of a [`Re`]: the whole match and its groups, as slices of the searched text.
#[derive(Debug)]
pub struct Caps<'t> {
    #[doc(hidden)]
    pub input: &'t [u16],
    whole: Range<usize>,
    count: usize,
    inline: [Slot; INLINE_GROUPS],
    spilled: Vec<Slot>,
    names: Option<Arc<Compiled>>,
}

impl<'t> Caps<'t> {
    fn groups(&self) -> &[Slot] {
        if self.count <= INLINE_GROUPS { &self.inline[..self.count] } else { &self.spilled }
    }

    #[doc(hidden)]
    pub fn range(&self, i: usize) -> Option<Range<usize>> {
        if i == 0 { Some(self.whole.clone()) } else { self.groups().get(i - 1).cloned().flatten() }
    }

    /// Group `i`, 0 being the whole match; `None` for a group that did not take part.
    pub fn get(&self, i: usize) -> Option<&'t [u16]> {
        self.range(i).map(|r| &self.input[r])
    }

    #[doc(hidden)]
    pub fn s(&self, i: usize) -> &'t [u16] {
        self.get(i).unwrap_or(&[])
    }

    #[doc(hidden)]
    pub fn has(&self, i: usize) -> bool {
        self.range(i).is_some()
    }

    pub fn whole(&self) -> &'t [u16] {
        &self.input[self.whole.clone()]
    }

    /// The UTF-16 offset where the match starts.
    pub fn index(&self) -> usize {
        self.whole.start
    }

    /// The UTF-16 offset where the match ends.
    pub fn end(&self) -> usize {
        self.whole.end
    }

    /// The number of capture groups of the expression, group 0 aside.
    pub fn group_count(&self) -> usize {
        self.count
    }

    /// The named group `name`; `None` when it did not take part or does not exist.
    pub fn named(&self, name: &str) -> Option<&'t [u16]> {
        if name.is_empty() {
            return None;
        }
        let names = self.names.as_ref()?.regex.group_names();
        let i = names.iter().position(|s| s.as_ref() == name)?;
        self.range(i + 1).map(|r| &self.input[r])
    }
}

/// Why [`Re::try_new`] rejected an expression, worded as the `SyntaxError` of V8.
#[derive(Debug)]
pub struct RegexSyntaxError(pub String);

impl Re {
    pub(crate) fn try_new_utf16(source: &[u16], flags: &str) -> Result<Self, RegexSyntaxError> {
        if let Ok(source) = String::from_utf16(source) {
            return Self::try_new(&source, flags);
        }
        check_flags(flags)?;
        let display = super::lossy(source);
        let regex = regress::Regex::from_utf16(source, flags).map_err(|e| syntax_error(&display, flags, e))?;
        Ok(Self::from_compiled(Arc::new(Compiled {
            regex,
            source: display,
            source_units: Some(source.to_vec()),
            flags: flags.into(),
            builtin: AtomicBool::new(false),
        })))
    }

    fn from_compiled(inner: Arc<Compiled>) -> Re {
        let flags = &inner.flags;
        Re {
            global: flags.contains('g'),
            sticky: flags.contains('y'),
            unicode: flags.contains('u') || flags.contains('v'),
            named: inner.source.contains("(?<") && has_named_group(&inner.source),
            builtin: false,
            inner,
        }
    }
    /// Compiles `source` with `flags` (`dgimsuvy`), as `new RegExp(source, flags)`.
    ///
    /// ```
    /// use microtypo_core::Re;
    ///
    /// let re = Re::try_new(r"\d+", "g").unwrap();
    /// assert_eq!((re.source(), re.flags()), (r"\d+", "g"));
    /// assert!(Re::try_new("(", "").is_err());
    /// ```
    pub fn try_new(source: &str, flags: &str) -> Result<Re, RegexSyntaxError> {
        check_flags(flags)?;
        Ok(Self::from_compiled(compile(source, flags)?))
    }

    pub fn source(&self) -> &str {
        &self.inner.source
    }

    /// The flags as given, not in the order `RegExp.prototype.flags` reports them.
    pub fn flags(&self) -> &str {
        &self.inner.flags
    }
}

#[doc(hidden)]
impl Re {
    pub fn new(source: &str, flags: &str) -> Re {
        let re = Re::configured(source, flags);
        if !re.inner.builtin.swap(true, Ordering::Relaxed) {
            BUILTINS.lock().unwrap_or_else(PoisonError::into_inner).insert((source.to_owned(), flags.to_owned()));
        }
        Re { builtin: true, ..re }
    }

    pub fn configured(source: &str, flags: &str) -> Re {
        Re::try_new(source, flags).unwrap_or_else(|e| panic!("invalid built-in pattern /{source}/{flags}: {}", e.0))
    }

    #[must_use]
    pub fn with_flags(&self, flags: &str) -> Re {
        if let Some(source) = &self.inner.source_units {
            Re::try_new_utf16(source, flags).expect("validated pattern with global flag")
        } else if self.builtin {
            Re::new(self.source(), flags)
        } else {
            Re::configured(self.source(), flags)
        }
    }

    fn with_search<R>(&self, text: &[u16], scan: impl FnOnce(&mut Search<'_>) -> R) -> R {
        let re = &self.inner.regex;
        let sticky = self.sticky;
        let (result, stopped) = if self.unicode {
            let mut matches = re.find_from_utf16(text, 0);
            matches.stop_when(over_budget);
            let result = scan(&mut |from, slots| {
                if sticky { matches.attempt_at_into(from, slots) } else { matches.find_at_into(from, slots) }
            });
            (result, matches.stopped())
        } else {
            let mut matches = re.find_from_ucs2(text, 0);
            matches.stop_when(over_budget);
            let result = scan(&mut |from, slots| {
                if sticky { matches.attempt_at_into(from, slots) } else { matches.find_at_into(from, slots) }
            });
            (result, matches.stopped())
        };
        if stopped {
            OUT_OF_BUDGET.set(true);
        }
        result
    }

    fn exec_with<'t>(&self, text: &'t [u16], search: &mut Search<'_>, last_index: usize) -> Option<Caps<'t>> {
        let count = self.inner.regex.group_count();
        let mut c =
            Caps { input: text, whole: 0..0, count, inline: Default::default(), spilled: Vec::new(), names: None };
        let slots = if count <= INLINE_GROUPS {
            &mut c.inline[..count]
        } else {
            c.spilled.resize(count, None);
            &mut c.spilled[..]
        };
        c.whole = search(last_index, slots)?;
        if self.sticky && c.whole.start != last_index {
            return None;
        }
        if !self.inner.regex.group_names().is_empty() {
            c.names = Some(self.inner.clone());
        }
        Some(c)
    }

    pub fn exec_at<'t>(&self, text: &'t [u16], last_index: usize) -> Option<Caps<'t>> {
        let last_index = if self.unicode { super::pair_start(text, last_index) } else { last_index };
        self.with_search(text, |search| self.exec_with(text, search, last_index))
    }

    fn next_index(&self, text: &[u16], c: &Caps<'_>) -> usize {
        if c.end() == c.index() { self.advance(text, c.end()) } else { c.end() }
    }

    pub fn test(&self, text: &[u16]) -> bool {
        self.exec_at(text, 0).is_some()
    }

    pub fn exec<'t>(&self, text: &'t [u16]) -> Option<Caps<'t>> {
        self.exec_at(text, 0)
    }

    pub fn advance(&self, text: &[u16], index: usize) -> usize {
        if !self.unicode || index + 1 >= text.len() {
            return index + 1;
        }
        if is_high(text[index]) && is_low(text[index + 1]) { index + 2 } else { index + 1 }
    }

    pub fn find_all<'t>(&self, text: &'t [u16]) -> Vec<Caps<'t>> {
        self.with_search(text, |search| {
            let mut out = Vec::new();
            let mut last = 0usize;
            while let Some(c) = self.exec_with(text, search, last) {
                last = self.next_index(text, &c);
                out.push(c);
                if last > text.len() {
                    break;
                }
            }
            out
        })
    }

    pub fn replace_with<F>(&self, text: &[u16], f: F) -> JsString
    where
        F: FnMut(&Caps<'_>) -> JsString,
    {
        self.replace_matches(text, f).unwrap_or_else(|| text.to_vec())
    }

    pub fn replace_owned<F>(&self, text: JsString, f: F) -> JsString
    where
        F: FnMut(&Caps<'_>) -> JsString,
    {
        let changed = self.replace_matches(&text, f);
        recycled(text, changed)
    }

    pub fn replace_matches<F>(&self, text: &[u16], mut f: F) -> Option<JsString>
    where
        F: FnMut(&Caps<'_>) -> JsString,
    {
        let write: &mut dyn FnMut(&Caps<'_>, &mut JsString) = &mut |c, out| out.extend_from_slice(&f(c));
        self.replace_into(text, write)
    }

    fn replace_into<F>(&self, text: &[u16], mut write: F) -> Option<JsString>
    where
        F: FnMut(&Caps<'_>, &mut JsString),
    {
        self.with_search(text, |search| {
            let mut out: Option<JsString> = None;
            let mut first = Vec::new();
            let mut next = 0;
            let mut last = 0usize;
            while let Some(c) = self.exec_with(text, search, last) {
                if c.index() >= next {
                    if let Some(out) = &mut out {
                        out.extend_from_slice(&text[next..c.index()]);
                        write(&c, out);
                    } else {
                        write(&c, &mut first);
                        if first != c.whole() {
                            let mut changed = spare(text.len());
                            changed.extend_from_slice(&text[..c.index()]);
                            changed.extend_from_slice(&first);
                            out = Some(changed);
                        }
                        first.clear();
                    }
                    next = c.end();
                } else {
                    write(&c, &mut Vec::new());
                }
                last = self.next_index(text, &c);
                if !self.global || last > text.len() {
                    break;
                }
            }
            let mut out = out?;
            out.extend_from_slice(&text[next.min(text.len())..]);
            Some(out)
        })
    }

    pub fn try_replace_with<F, E>(&self, text: &[u16], f: F) -> Result<JsString, E>
    where
        F: FnMut(&Caps<'_>) -> Result<JsString, E>,
    {
        Ok(self.try_replace_matches(text, f)?.unwrap_or_else(|| text.to_vec()))
    }

    pub fn try_replace_matches<F, E>(&self, text: &[u16], mut f: F) -> Result<Option<JsString>, E>
    where
        F: FnMut(&Caps<'_>) -> Result<JsString, E>,
    {
        let mut error = None;
        let out = self.replace_matches(text, |c| {
            if error.is_some() {
                return Vec::new();
            }
            f(c).unwrap_or_else(|e| {
                error = Some(e);
                Vec::new()
            })
        });
        match error {
            Some(e) => Err(e),
            None => Ok(out),
        }
    }

    pub fn replace_tpl(&self, text: &[u16], template: &[u16]) -> JsString {
        self.replace_tpl_matches(text, template).unwrap_or_else(|| text.to_vec())
    }

    pub fn replace_tpl_matches(&self, text: &[u16], template: &[u16]) -> Option<JsString> {
        if !template.contains(&0x24) {
            return self.replace_into(text, |_, out| out.extend_from_slice(template));
        }
        let named = self.named;
        self.replace_into(text, |c, out| substitute_into(c, template, named, out))
    }

    pub fn replace_tpl_owned(&self, text: JsString, template: &[u16]) -> JsString {
        let changed = self.replace_tpl_matches(&text, template);
        recycled(text, changed)
    }

    pub fn replace_str(&self, text: &[u16], template: &str) -> JsString {
        self.replace_tpl(text, &super::u(template))
    }
}

type Budget = (fn() -> f64, f64, f64);

thread_local! {
    static BUDGET: Cell<Option<Budget>> = const { Cell::new(None) };
    static OUT_OF_BUDGET: Cell<bool> = const { Cell::new(false) };
    static SPARE: RefCell<JsString> = const { RefCell::new(Vec::new()) };
}

pub fn recycle(text: JsString) {
    SPARE.with_borrow_mut(|spare| {
        if text.capacity() > spare.capacity() {
            *spare = text;
        }
    });
}

pub(crate) fn recycled(text: JsString, changed: Option<JsString>) -> JsString {
    match changed {
        Some(changed) => {
            recycle(text);
            changed
        }
        None => text,
    }
}

pub(crate) fn spare(len: usize) -> JsString {
    let mut out = SPARE.with_borrow_mut(std::mem::take);
    out.clear();
    out.reserve(len);
    out
}

fn over_budget() -> bool {
    BUDGET.get().is_some_and(|(now_ms, start, ms)| now_ms() - start > ms)
}

pub(crate) fn scan_out_of_budget() -> bool {
    if OUT_OF_BUDGET.get() {
        return true;
    }
    let expired = over_budget();
    OUT_OF_BUDGET.set(expired);
    expired
}

struct RestoreBudget(Option<Budget>, bool);

impl Drop for RestoreBudget {
    fn drop(&mut self) {
        BUDGET.set(self.0);
        OUT_OF_BUDGET.set(self.1);
    }
}

pub fn with_budget<R>(now_ms: fn() -> f64, start: f64, ms: f64, run: impl FnOnce() -> R) -> R {
    let _restore =
        RestoreBudget(BUDGET.replace((ms > 0.0).then_some((now_ms, start, ms))), OUT_OF_BUDGET.replace(false));
    run()
}

pub fn out_of_budget() -> bool {
    OUT_OF_BUDGET.get()
}

static COMPILED: LazyLock<Mutex<HashMap<String, Vec<Arc<Compiled>>>>> = LazyLock::new(Default::default);
const COMPILED_LIMIT: usize = 4096;

static BUILTINS: Mutex<BTreeSet<(String, String)>> = Mutex::new(BTreeSet::new());

pub fn builtins() -> Vec<(String, String)> {
    BUILTINS.lock().unwrap_or_else(PoisonError::into_inner).iter().cloned().collect()
}

pub fn generated() -> &'static [(&'static str, &'static str, regress::aot::Attempt, u32)] {
    matchers::MATCHERS
}

pub fn interpreted() -> &'static [(&'static str, &'static str, &'static str)] {
    matchers::INTERPRETED
}

fn compile(source: &str, flags: &str) -> Result<Arc<Compiled>, RegexSyntaxError> {
    let cached = |compiled: &HashMap<String, Vec<Arc<Compiled>>>| {
        compiled.get(source).and_then(|variants| variants.iter().find(|c| c.flags == flags).cloned())
    };
    if let Some(c) = cached(&COMPILED.lock().unwrap_or_else(PoisonError::into_inner)) {
        return Ok(c);
    }
    let matcher = matchers::MATCHERS
        .binary_search_by(|&(s, f, ..)| (s, f).cmp(&(source, flags)))
        .ok()
        .map(|i| matchers::MATCHERS[i]);
    let factors = match matcher {
        Some((.., at)) => Some(at),
        None => matchers::INTERPRETED_FACTORS
            .binary_search_by(|&(s, f, _)| (s, f).cmp(&(source, flags)))
            .ok()
            .map(|i| matchers::INTERPRETED_FACTORS[i].2),
    };
    let factors = factors.map_or_else(regress::FactorSource::default, |at| {
        regress::FactorSource::Given(regress::aot::Factors(&matchers::FACTOR_DATA[at as usize..]))
    });
    let mut regex = regress::Regex::from_utf16_with(&super::u(source), flags, factors)
        .map_err(|e| syntax_error(source, flags, e))?;
    if let Some((_, _, attempt, _)) = matcher {
        regex.set_attempt(attempt);
    }
    let c = Arc::new(Compiled {
        regex,
        source: source.to_owned(),
        source_units: None,
        flags: flags.to_owned(),
        builtin: AtomicBool::new(false),
    });
    let mut compiled = COMPILED.lock().unwrap_or_else(PoisonError::into_inner);
    if compiled.len() >= COMPILED_LIMIT {
        compiled.clear();
    }
    compiled.entry(source.to_owned()).or_default().push(c.clone());
    Ok(c)
}

fn syntax_error(source: &str, flags: &str, e: regress::Error) -> RegexSyntaxError {
    let reason = match e.text.as_str() {
        "Unbalanced bracket" => "Unterminated character class",
        "Unbalanced class set bracket" | "Empty class set operand" => "Unterminated character class",
        "Missing class set operand" | "Invalid class set character" => "Invalid character in character class",
        "Range values reversed, start char code is greater than end char code." => {
            "Range out of order in character class"
        }
        "Incomplete escape" | "Unterminated escape" => "\\ at end of pattern",

        "Invalid group modifier" => "Invalid group",
        "Invalid unicode escape" => "Invalid Unicode escape",
        "Invalid character escape" => "Invalid escape",
        "Invalid token at named capture group identifier" => "Invalid capture group name",
        "Invalid named backreference syntax" => "Invalid named reference",
        text if text.starts_with("Backreference to invalid named capture group:") => "Invalid named capture referenced",
        "Invalid property name" | "Invalid property escape" | "Invalid character at property escape start" => {
            "Invalid property name"
        }
        "Duplicate capture group name" => "Duplicate capture group name",
        other => other,
    };
    RegexSyntaxError(format!("Invalid regular expression: /{source}/{flags}: {reason}"))
}

fn check_flags(flags: &str) -> Result<(), RegexSyntaxError> {
    let invalid = flags.char_indices().any(|(i, c)| !"dgimsuvy".contains(c) || flags[..i].contains(c));
    if invalid || (flags.contains('u') && flags.contains('v')) {
        return Err(RegexSyntaxError(format!("Invalid flags supplied to RegExp constructor '{flags}'")));
    }
    Ok(())
}

fn has_named_group(source: &str) -> bool {
    let b = source.as_bytes();
    let mut i = 0;
    while i + 3 < b.len() {
        if b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == b'(' && b[i + 1] == b'?' && b[i + 2] == b'<' && b[i + 3] != b'=' && b[i + 3] != b'!' {
            return true;
        }
        i += 1;
    }
    false
}

pub(crate) fn substitute(c: &Caps<'_>, template: &[u16]) -> JsString {
    let mut output = Vec::new();
    substitute_into(c, template, c.names.is_some(), &mut output);
    output
}

fn substitute_into(c: &Caps<'_>, template: &[u16], named: bool, out: &mut JsString) {
    let text = c.input;
    let m = c.group_count();
    let mut i = 0;
    let digit = |u: u16| (0x30..=0x39).contains(&u);
    while i < template.len() {
        let ch = template[i];
        if ch != 0x24 || i + 1 >= template.len() {
            out.push(ch);
            i += 1;
            continue;
        }
        let next = template[i + 1];
        match next {
            0x24 => {
                out.push(0x24);
                i += 2;
            }
            0x26 => {
                out.extend_from_slice(c.whole());
                i += 2;
            }
            0x60 => {
                out.extend_from_slice(&text[..c.index()]);
                i += 2;
            }
            0x27 => {
                out.extend_from_slice(&text[c.end().min(text.len())..]);
                i += 2;
            }
            d if digit(d) => {
                let two = i + 2 < template.len() && digit(template[i + 2]);
                let mut count = if two { 2 } else { 1 };
                let mut index = usize::from(d - 0x30);
                if two {
                    index = index * 10 + usize::from(template[i + 2] - 0x30);
                }
                if index > m && count == 2 {
                    count = 1;
                    index = usize::from(d - 0x30);
                }
                if (1..=m).contains(&index) {
                    if let Some(g) = c.get(index) {
                        out.extend_from_slice(g);
                    }
                } else {
                    out.extend_from_slice(&template[i..i + 1 + count]);
                }
                i += 1 + count;
            }
            0x3C if named => {
                let rest = &template[i + 2..];
                if let Some(end) = rest.iter().position(|&u| u == 0x3E) {
                    let name = String::from_utf16_lossy(&rest[..end]);
                    if let Some(g) = c.named(&name) {
                        out.extend_from_slice(g);
                    }
                    i += 2 + end + 1;
                } else {
                    out.extend_from_slice(&template[i..i + 2]);
                    i += 2;
                }
            }
            _ => {
                out.push(0x24);
                i += 1;
            }
        }
    }
}

pub fn escape(s: &[u16]) -> String {
    let mut out = String::new();
    for (i, c) in char::decode_utf16(s.iter().copied()).enumerate() {
        let c = match c {
            Ok(c) => c,
            Err(lone) => {
                let _ = write!(out, "\\u{:04x}", lone.unpaired_surrogate());
                continue;
            }
        };
        let code = u32::from(c);
        if i == 0 && c.is_ascii_alphanumeric() {
            let _ = write!(out, "\\x{code:02x}");
        } else if "^$\\.*+?()[]{}|/".contains(c) {
            out.push('\\');
            out.push(c);
        } else if let Some(control) = match c {
            '\t' => Some('t'),
            '\n' => Some('n'),
            '\u{B}' => Some('v'),
            '\u{C}' => Some('f'),
            '\r' => Some('r'),
            _ => None,
        } {
            out.push('\\');
            out.push(control);
        } else if ",-=<>#&!%:;@~'`\"".contains(c) || u16::try_from(code).is_ok_and(super::is_js_space) {
            if code <= 0xFF {
                let _ = write!(out, "\\x{code:02x}");
            } else {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn escape_str(s: &str) -> String {
    escape(&super::u(s))
}
