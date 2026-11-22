//! Generating Rust matchers ahead of time, for UTF-16 and UCS-2 input.
//!
//! A generated matcher does what the classical backtracking interpreter does with a regex's
//! bytecode, instruction by instruction: see `aot` for the state it shares with its pieces.
//! Every `try_at_pos` the interpreter would run, the top level and the body of each
//! lookaround, becomes one function: a loop over blocks of straight-line code, one block per
//! instruction that a jump, a loop decision or a backtrack entry may resume at. Such an
//! instruction is a label, numbered in order from the entry. A failing instruction leaves the
//! block and backtracks, as `try_backtrack` does, to the label and position the entry holds.
//!
//! Single characters are tested against the set of code points the instruction accepts: a
//! code unit at a time when the set holds no surrogate and no astral code point, or after the
//! input does not decode, and as the indexer decodes them otherwise. Sets of up to
//! `MAX_INLINE_RANGES` ranges are tested range by range; larger ones go through a `Class`
//! table, which `Codegen::classes` emits once for every matcher that shares it.

use crate::api::Regex;
use crate::codepointset::{CODE_POINT_MAX, CodePointSet, Interval};
use crate::insn::{CompiledRegex, Insn};
#[cfg(not(feature = "std"))]
use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    vec::Vec,
};
use core::fmt::Write;
#[cfg(feature = "std")]
use std::collections::BTreeMap;

/// The most ranges a set is tested by one comparison each.
const MAX_INLINE_RANGES: usize = 16;

/// The largest code unit.
const UNIT_MAX: u32 = 0xFFFF;

/// Generates matchers for a set of regexes, sharing the tables of large character classes.
#[derive(Debug, Default, Clone)]
pub struct Codegen {
    classes: BTreeMap<Vec<(u32, u32)>, usize>,
}

/// A function to generate: the top level or a lookaround body, where it starts and the
/// direction of its cursor.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Body {
    entry: usize,
    forward: bool,
}

/// How an instruction leaves control: to the next instruction in line, if any, and to the
/// labels it may jump or backtrack to.
fn successors(re: &CompiledRegex, ip: usize) -> (Option<usize>, Vec<usize>) {
    match &re.insns[ip] {
        Insn::Goal | Insn::JustFail => (None, Vec::new()),
        &Insn::Jump { target } => (None, vec![target as usize]),
        &Insn::Alt { secondary } => (Some(ip + 1), vec![secondary as usize]),
        Insn::EnterLoop(fields) => (None, vec![ip + 1, fields.exit as usize]),
        &Insn::LoopAgain { begin } => match &re.insns[begin as usize] {
            Insn::EnterLoop(fields) => (None, vec![begin as usize + 1, fields.exit as usize]),
            _ => unreachable!("LoopAgain must refer to an EnterLoop"),
        },
        &Insn::Loop1CharBody { possessive, .. } => {
            if possessive {
                (Some(ip + 2), Vec::new())
            } else {
                (Some(ip + 2), vec![ip + 2])
            }
        }
        &Insn::Lookahead { continuation, .. } | &Insn::Lookbehind { continuation, .. } => {
            (Some(continuation as usize), Vec::new())
        }
        _ => (Some(ip + 1), Vec::new()),
    }
}

/// \return the instructions control may reach from \p entry within one function, in order.
fn reachable(re: &CompiledRegex, entry: usize) -> Vec<usize> {
    let mut seen = vec![false; re.insns.len()];
    let mut work = vec![entry];
    while let Some(ip) = work.pop() {
        if seen[ip] {
            continue;
        }
        seen[ip] = true;
        let (next, targets) = successors(re, ip);
        work.extend(next);
        work.extend(targets);
    }
    (0..re.insns.len()).filter(|&ip| seen[ip]).collect()
}

/// \return the functions to generate: the top level first, then each lookaround body.
fn bodies(re: &CompiledRegex) -> Vec<Body> {
    let mut bodies = vec![Body {
        entry: 0,
        forward: true,
    }];
    let mut done = 0;
    while done < bodies.len() {
        for ip in reachable(re, bodies[done].entry) {
            let body = match re.insns[ip] {
                Insn::Lookahead { .. } => Body {
                    entry: ip + 1,
                    forward: true,
                },
                Insn::Lookbehind { .. } => Body {
                    entry: ip + 1,
                    forward: false,
                },
                _ => continue,
            };
            if !bodies.contains(&body) {
                bodies.push(body);
            }
        }
        done += 1;
    }
    bodies
}

/// \return the code points \p insn accepts as one character, or None if it is no single
/// character matcher.
fn char_set(re: &CompiledRegex, insn: &Insn) -> Option<CodePointSet> {
    let mut set = CodePointSet::new();
    match insn {
        &Insn::Char(c) => set.add_one(c),
        Insn::CharSet(chars) => chars.iter().for_each(|&c| set.add_one(c)),
        &Insn::Bracket(idx) => {
            let contents = &re.brackets[idx].contents;
            return Some(if contents.invert {
                contents.cps.inverted()
            } else {
                contents.cps.clone()
            });
        }
        Insn::AsciiBracket(bitmap) => (0..128u32)
            .filter(|&b| bitmap.0[(b >> 3) as usize] & (1 << (b & 7)) != 0)
            .for_each(|b| set.add_one(b)),
        Insn::MatchAny => set.add(Interval::new(0, CODE_POINT_MAX)),
        Insn::MatchAnyExceptLineTerminator => {
            for (first, last) in [
                (0, 0x09),
                (0x0B, 0x0C),
                (0x0E, 0x2027),
                (0x202A, CODE_POINT_MAX),
            ] {
                set.add(Interval::new(first, last));
            }
        }
        _ => return None,
    }
    Some(set)
}

/// \return whether a set holds a surrogate or an astral code point, which only decoding the
/// input tells apart from the code units around them.
fn needs_decoding(set: &CodePointSet) -> bool {
    set.intervals()
        .iter()
        .any(|iv| iv.last >= 0xD800 && iv.first <= 0xDFFF || iv.last > UNIT_MAX)
}

/// What the paths from an instruction consume first: the code points if every path that may
/// succeed consumes one before it ends; None if one may end, or match a backreference, first.
type First = Option<CodePointSet>;

fn union(a: &First, b: &First) -> First {
    let (Some(a), Some(b)) = (a, b) else {
        return None;
    };
    let mut set = a.clone();
    set.add_set(b.clone());
    Some(set)
}

/// \return what the paths from each instruction consume first, as the least fixpoint of the
/// flow from each to its successors. A path that comes back to where it started without
/// consuming adds nothing, so starting every instruction from the empty set is sound.
fn firsts(re: &CompiledRegex) -> Vec<First> {
    let mut first: Vec<First> = vec![Some(CodePointSet::new()); re.insns.len()];
    loop {
        let mut changed = false;
        for ip in (0..re.insns.len()).rev() {
            let next = first_of(re, ip, &first);
            if next != first[ip] {
                first[ip] = next;
                changed = true;
            }
        }
        if !changed {
            return first;
        }
    }
}

fn first_of(re: &CompiledRegex, ip: usize, first: &[First]) -> First {
    let insn = &re.insns[ip];
    if let Some(set) = char_set(re, insn) {
        return Some(set);
    }
    match *insn {
        Insn::JustFail => Some(CodePointSet::new()),
        Insn::Jump { target } => first[target as usize].clone(),
        Insn::Alt { secondary } => union(&first[ip + 1], &first[secondary as usize]),
        Insn::EnterLoop(ref fields) => {
            if fields.min_iters == 0 {
                union(&first[ip + 1], &first[fields.exit as usize])
            } else {
                first[ip + 1].clone()
            }
        }
        Insn::LoopAgain { begin } => match &re.insns[begin as usize] {
            Insn::EnterLoop(fields) if fields.max_iters <= 1 => first[fields.exit as usize].clone(),
            Insn::EnterLoop(fields) => {
                union(&first[begin as usize + 1], &first[fields.exit as usize])
            }
            _ => unreachable!("LoopAgain must refer to an EnterLoop"),
        },
        Insn::Loop1CharBody { min_iters, .. } => {
            let body = char_set(re, &re.insns[ip + 1]);
            if min_iters == 0 {
                union(&body, &first[ip + 2])
            } else {
                body
            }
        }
        Insn::Lookahead { continuation, .. } | Insn::Lookbehind { continuation, .. } => {
            first[continuation as usize].clone()
        }
        Insn::BeginCaptureGroup(..)
        | Insn::EndCaptureGroup(..)
        | Insn::ResetCaptureGroup(..)
        | Insn::StartOfLine { .. }
        | Insn::EndOfLine { .. }
        | Insn::WordBoundary { .. }
        | Insn::WordBoundaryUnicodeICase { .. } => first[ip + 1].clone(),
        _ => None,
    }
}

/// The generator of the functions of one matcher.
struct Gen<'a> {
    re: &'a CompiledRegex,
    name: &'a str,
    utf16: bool,
    classes: &'a mut BTreeMap<Vec<(u32, u32)>, usize>,
    bodies: Vec<Body>,
    first: Vec<First>,
    untils: usize,
    out: String,
}

/// What the code for one character runs when the character does not match.
#[derive(Clone, Copy)]
enum Fail {
    /// Backtrack.
    Backtrack,
    /// Leave the enclosing loop.
    Break,
}

impl Fail {
    fn code(self) -> &'static str {
        match self {
            Fail::Backtrack => "break 'fail",
            Fail::Break => "break",
        }
    }
}

impl Gen<'_> {
    /// \return an expression of whether \p v, a code unit if \p unit or else a code point, is in
    /// \p set.
    fn predicate(&mut self, set: &CodePointSet, v: &str, unit: bool) -> Result<String, String> {
        let ranges: Vec<(u32, u32)> = set
            .intervals()
            .iter()
            .map(|iv| (iv.first, iv.last))
            .collect();
        let clipped: Vec<(u32, u32)> = if unit {
            ranges
                .iter()
                .filter(|r| r.0 <= UNIT_MAX)
                .map(|&(first, last)| (first, last.min(UNIT_MAX)))
                .collect()
        } else {
            ranges.clone()
        };
        let all = if unit { UNIT_MAX } else { CODE_POINT_MAX };
        if clipped.is_empty() {
            return Err("a character class that matches nothing".to_string());
        }
        if clipped == [(0, all)] {
            return Ok("true".to_string());
        }
        if clipped.len() > MAX_INLINE_RANGES {
            let next = self.classes.len();
            let class = *self.classes.entry(ranges).or_insert(next);
            return Ok(if unit {
                format!("CLASS_{class}.contains(u32::from({v}))")
            } else {
                format!("CLASS_{class}.contains({v})")
            });
        }
        let alternatives: Vec<String> = clipped
            .iter()
            .map(|&(first, last)| {
                if first == last {
                    format!("{first:#x}")
                } else {
                    format!("{first:#x}..={last:#x}")
                }
            })
            .collect();
        Ok(format!("matches!({v}, {})", alternatives.join(" | ")))
    }

    /// \return code that matches one character at `pos` with \p insn in direction \p forward and
    /// moves `pos` past it, or runs \p fail.
    fn advance(
        &mut self,
        insn: &Insn,
        forward: bool,
        pos: &str,
        fail: Fail,
    ) -> Result<String, String> {
        let set =
            char_set(self.re, insn).ok_or_else(|| format!("unsupported instruction {insn:?}"))?;
        let fail = fail.code();
        if !self.utf16 || !needs_decoding(&set) {
            let test = self.predicate(&set, "u", true)?;
            let guard = if test == "true" {
                String::new()
            } else {
                format!(" if {test}")
            };
            return Ok(if forward {
                format!("match t.get({pos}) {{ Some(&u){guard} => {pos} += 1, _ => {fail} }}\n")
            } else {
                format!(
                    "match t.get({pos}.wrapping_sub(1)) {{ Some(&u){guard} => {pos} -= 1, _ => {fail} }}\n"
                )
            });
        }
        let test = self.predicate(&set, "c", false)?;
        let step = if forward { "next_right" } else { "next_left" };
        Ok(if test == "true" {
            format!(
                "match aot::{step}::<true>(t, {pos}) {{ Some((_, n)) => {pos} = n, None => {fail} }}\n"
            )
        } else {
            format!(
                "match aot::{step}::<true>(t, {pos}) {{ Some((c, n)) if {test} => {pos} = n, _ => {fail} }}\n"
            )
        })
    }

    /// \return an expression of whether the character at `pos` in direction \p forward may
    /// start a path from \p ip, or None if every position may.
    fn may_start(&mut self, ip: usize, forward: bool) -> Result<Option<String>, String> {
        let Some(set) = self.first[ip].clone() else {
            return Ok(None);
        };
        let at = if forward {
            "pos"
        } else {
            "pos.wrapping_sub(1)"
        };
        if !self.utf16 || !needs_decoding(&set) {
            if !set.intervals().iter().any(|iv| iv.first <= UNIT_MAX) {
                return Ok(Some("false".to_string()));
            }
            let test = self.predicate(&set, "u", true)?;
            return Ok(Some(format!("t.get({at}).is_some_and(|&u| {test})")));
        }
        let test = self.predicate(&set, "c", false)?;
        let step = if forward { "next_right" } else { "next_left" };
        Ok(Some(format!(
            "aot::{step}::<true>(t, pos).is_some_and(|(c, _)| {test})"
        )))
    }

    /// \return an expression of whether the body of the greedy loop \p fields, entered at
    /// \p ip, may match at `pos`; a non-greedy loop always tries it.
    fn may_enter(
        &mut self,
        fields: &crate::insn::LoopFields,
        ip: usize,
        forward: bool,
    ) -> Result<String, String> {
        if !fields.greedy {
            return Ok("true".to_string());
        }
        Ok(self
            .may_start(ip + 1, forward)?
            .unwrap_or_else(|| "true".to_string()))
    }

    /// \return the name of a function of whether a code unit may start a path from \p ip, if
    /// every unit it accepts is a character by itself; emit the function.
    fn until(&mut self, ip: usize) -> Result<Option<String>, String> {
        let Some(set) = self.first[ip].clone() else {
            return Ok(None);
        };
        if self.utf16 && needs_decoding(&set) {
            return Ok(None);
        }
        let test = if set.intervals().iter().any(|iv| iv.first <= UNIT_MAX) {
            self.predicate(&set, "u", true)?
        } else {
            "false".to_string()
        };
        let name = format!("{}_u{}", self.name, self.untils);
        self.untils += 1;
        writeln!(self.out, "fn {name}(u: u16) -> bool {{ {test} }}\n").unwrap();
        Ok(Some(name))
    }

    fn body_fn(&self, body: Body) -> String {
        let idx = self
            .bodies
            .iter()
            .position(|b| *b == body)
            .expect("Body should be known");
        format!("{}_{idx}", self.name)
    }

    /// Generate the function for \p body.
    fn function(&mut self, body: Body) -> Result<(), String> {
        let re = self.re;
        let insns = reachable(re, body.entry);
        let mut labels = vec![body.entry];
        for &ip in &insns {
            labels.extend(successors(re, ip).1);
        }
        labels.sort_unstable();
        labels.dedup();
        let entry_at = labels
            .iter()
            .position(|&l| l == body.entry)
            .expect("Entry is a label");
        labels.swap(0, entry_at);
        labels[1..].sort_unstable();
        let label = |ip: usize| {
            labels
                .iter()
                .position(|&l| l == ip)
                .expect("Target is a label") as u32
        };

        let (g, l) = (re.groups, re.loops);
        let (utf16, forward) = (self.utf16, body.forward);
        let mut code = String::new();
        writeln!(
            code,
            "fn {}(t: &[u16], mut pos: usize, s: &mut aot::State<'_, {g}, {l}>) -> Option<usize> {{",
            self.body_fn(body)
        )
        .unwrap();
        code.push_str("let base = s.bts.len();\nlet mut label = 0u32;\n'run: loop {\n'fail: {\nmatch label {\n");
        for (idx, &start) in labels.iter().enumerate() {
            writeln!(code, "{idx} => {{").unwrap();
            let mut ip = start;
            loop {
                let next = self.insn(&mut code, ip, forward, &label)?;
                match next {
                    None => break,
                    Some(next) if labels.contains(&next) => {
                        writeln!(code, "label = {};\ncontinue 'run;", label(next)).unwrap();
                        break;
                    }
                    Some(next) => ip = next,
                }
            }
            code.push_str("}\n");
        }
        code.push_str("_ => unreachable!(),\n}\n}\n");
        writeln!(
            code,
            "(label, pos) = s.backtrack::<{utf16}, {forward}>(t, base)?;\n}}\n}}\n"
        )
        .unwrap();
        self.out.push_str(&code);
        Ok(())
    }

    /// Emit the code of the instruction at \p ip into \p code.
    /// \return the instruction that follows in line, or None if control leaves the block.
    fn insn(
        &mut self,
        code: &mut String,
        ip: usize,
        forward: bool,
        label: &dyn Fn(usize) -> u32,
    ) -> Result<Option<usize>, String> {
        let re = self.re;
        let utf16 = self.utf16;
        let insn = &re.insns[ip];
        match insn {
            Insn::Char(..)
            | Insn::CharSet(..)
            | Insn::Bracket(..)
            | Insn::AsciiBracket(..)
            | Insn::MatchAny
            | Insn::MatchAnyExceptLineTerminator => {
                let advance = self.advance(insn, forward, "pos", Fail::Backtrack)?;
                code.push_str(&advance);
            }
            Insn::Goal => {
                code.push_str("s.bts.truncate(base);\nreturn Some(pos);\n");
                return Ok(None);
            }
            Insn::JustFail => {
                code.push_str("break 'fail;\n");
                return Ok(None);
            }
            &Insn::StartOfLine { multiline } => {
                if multiline {
                    code.push_str(
                        "if pos != 0 && !aot::is_line_terminator(u32::from(t[pos - 1])) { break 'fail; }\n",
                    );
                } else {
                    code.push_str("if pos != 0 { break 'fail; }\n");
                }
            }
            &Insn::EndOfLine { multiline } => {
                if multiline {
                    code.push_str(
                        "if pos != t.len() && !aot::is_line_terminator(u32::from(t[pos])) { break 'fail; }\n",
                    );
                } else {
                    code.push_str("if pos != t.len() { break 'fail; }\n");
                }
            }
            &Insn::WordBoundary { invert } => {
                writeln!(
                    code,
                    "if aot::word_boundary::<{utf16}>(t, pos) == {invert} {{ break 'fail; }}"
                )
                .unwrap();
            }
            &Insn::WordBoundaryUnicodeICase { invert } => {
                writeln!(
                    code,
                    "if aot::word_boundary_unicode_icase::<{utf16}>(t, pos) == {invert} {{ break 'fail; }}"
                )
                .unwrap();
            }
            &Insn::Jump { target } => {
                writeln!(code, "label = {};\ncontinue 'run;", label(target as usize)).unwrap();
                return Ok(None);
            }
            &Insn::Alt { secondary } => {
                let secondary = secondary as usize;
                if let Some(primary) = self.may_start(ip + 1, forward)? {
                    writeln!(
                        code,
                        "if !{primary} {{ label = {}; continue 'run; }}",
                        label(secondary)
                    )
                    .unwrap();
                }
                let push = format!(
                    "s.bts.push(aot::Bt::Position {{ label: {}, pos }});",
                    label(secondary)
                );
                match self.may_start(secondary, forward)? {
                    Some(test) => writeln!(code, "if {test} {{ {push} }}").unwrap(),
                    None => writeln!(code, "{push}").unwrap(),
                }
            }
            &Insn::BeginCaptureGroup(id) => {
                let bound = if forward { "start" } else { "end" };
                writeln!(
                    code,
                    "s.save_group(base, {id});\ns.groups[{id}].{bound} = pos;"
                )
                .unwrap();
            }
            &Insn::EndCaptureGroup(id) => {
                let bound = if forward { "end" } else { "start" };
                writeln!(code, "s.groups[{id}].{bound} = pos;").unwrap();
            }
            &Insn::ResetCaptureGroup(id) => {
                writeln!(
                    code,
                    "s.save_group(base, {id});\ns.groups[{id}] = aot::Group {{ start: aot::UNSET, end: aot::UNSET }};"
                )
                .unwrap();
            }
            &Insn::BackRef { group, icase } => {
                writeln!(
                    code,
                    "match s.backref::<{utf16}, {forward}>(t, pos, {group}, {icase}, {}) {{ Some(p) => pos = p, None => break 'fail }}",
                    re.flags.unicode_mode()
                )
                .unwrap();
            }
            &Insn::Lookahead {
                negate,
                start_group,
                end_group,
                continuation,
            }
            | &Insn::Lookbehind {
                negate,
                start_group,
                end_group,
                continuation,
            } => {
                let body = Body {
                    entry: ip + 1,
                    forward: matches!(insn, Insn::Lookahead { .. }),
                };
                let call = format!("{}(t, pos, s)", self.body_fn(body));
                let (first, count) = (start_group as usize, (end_group - start_group) as usize);
                if count == 0 {
                    let fails = if negate { "is_some" } else { "is_none" };
                    writeln!(code, "if {call}.{fails}() {{ break 'fail; }}").unwrap();
                } else {
                    let saved: Vec<String> = (first..first + count)
                        .map(|id| format!("s.groups[{id}]"))
                        .collect();
                    let range = format!("{first}..{}", first + count);
                    writeln!(code, "{{\nlet saved = [{}];", saved.join(", ")).unwrap();
                    writeln!(code, "let matched = {call}.is_some();").unwrap();
                    if negate {
                        writeln!(
                            code,
                            "s.groups[{range}].copy_from_slice(&saved);\nif matched {{ break 'fail; }}"
                        )
                        .unwrap();
                    } else {
                        code.push_str("if matched {\n");
                        for idx in 0..count {
                            writeln!(
                                code,
                                "s.bts.push(aot::Bt::Group {{ id: {}, data: saved[{idx}] }});",
                                first + idx
                            )
                            .unwrap();
                        }
                        writeln!(
                            code,
                            "}} else {{\ns.groups[{range}].copy_from_slice(&saved);\nbreak 'fail;\n}}"
                        )
                        .unwrap();
                    }
                    code.push_str("}\n");
                }
                return Ok(Some(continuation as usize));
            }
            Insn::EnterLoop(fields) => {
                let enter = self.may_enter(fields, ip, forward)?;
                writeln!(
                    code,
                    "s.loops[{id}].iters = 0;\n{}",
                    run_loop(fields, ip, &enter, label),
                    id = fields.loop_id
                )
                .unwrap();
                return Ok(None);
            }
            &Insn::LoopAgain { begin } => {
                let Insn::EnterLoop(fields) = &re.insns[begin as usize] else {
                    unreachable!("LoopAgain must refer to an EnterLoop")
                };
                let enter = self.may_enter(fields, begin as usize, forward)?;
                code.push_str(&run_loop(fields, begin as usize, &enter, label));
                return Ok(None);
            }
            &Insn::Loop1CharBody {
                min_iters,
                max_iters,
                greedy,
                possessive,
            } => {
                let body = &re.insns[ip + 1];
                let bounded = max_iters != usize::MAX;
                let rest = max_iters - min_iters;
                code.push_str("{\n");
                match min_iters {
                    0 => {}
                    1 => code.push_str(&self.advance(body, forward, "pos", Fail::Backtrack)?),
                    n => {
                        let advance = self.advance(body, forward, "pos", Fail::Backtrack)?;
                        writeln!(code, "for _ in 0..{n} {{\n{advance}}}").unwrap();
                    }
                }
                let whole = char_set(re, body).is_some_and(|set| {
                    let all = if self.utf16 { CODE_POINT_MAX } else { UNIT_MAX };
                    set.intervals()
                        .first()
                        .is_some_and(|iv| iv.first == 0 && iv.last >= all)
                });
                let scan = |this: &mut Self, code: &mut String, pos: &str| -> Result<(), String> {
                    let advance = this.advance(body, forward, pos, Fail::Break)?;
                    if whole && !bounded {
                        let end = if forward { "t.len()" } else { "0" };
                        writeln!(code, "{pos} = {end};").unwrap();
                    } else if bounded {
                        writeln!(code, "for _ in 0..{rest} {{\n{advance}}}").unwrap();
                    } else {
                        writeln!(code, "loop {{\n{advance}}}").unwrap();
                    }
                    Ok(())
                };
                let continuation = ip + 2;
                let until = if forward && (greedy && !possessive || !greedy) && rest > 0 {
                    self.until(continuation)?
                } else {
                    None
                };
                let entry = |kind: &str, min: &str, max: &str| {
                    let target = label(continuation);
                    match &until {
                        Some(until) => format!(
                            "aot::Bt::{kind}Until {{ label: {target}, min: {min}, max: {max}, until: {until} }}"
                        ),
                        None => {
                            format!("aot::Bt::{kind} {{ label: {target}, min: {min}, max: {max} }}")
                        }
                    }
                };
                if greedy {
                    if possessive {
                        if rest > 0 {
                            scan(self, code, "pos")?;
                        }
                    } else if rest > 0 {
                        code.push_str("let min_pos = pos;\n");
                        scan(self, code, "pos")?;
                        let entry = entry("GreedyLoop1Char", "min_pos", "pos");
                        writeln!(code, "if min_pos != pos {{ s.bts.push({entry}); }}").unwrap();
                    }
                } else if rest > 0 {
                    if whole && !bounded {
                        let end = if forward { "t.len()" } else { "0" };
                        writeln!(code, "let max_pos = {end};").unwrap();
                    } else {
                        code.push_str("let mut max_pos = pos;\n");
                        scan(self, code, "max_pos")?;
                    }
                    let entry = entry("NonGreedyLoop1Char", "pos", "max_pos");
                    writeln!(code, "if pos != max_pos {{ s.bts.push({entry}); }}").unwrap();
                }
                code.push_str("}\n");
                return Ok(Some(continuation));
            }
            _ => return Err(format!("unsupported instruction {insn:?}")),
        }
        Ok(Some(ip + 1))
    }
}

/// \return code that decides, as `run_loop` does, whether the loop \p fields entered at \p ip
/// runs its body again, and goes to the label it decides on. \p enter is an expression of
/// whether the body may match at `pos`.
fn run_loop(
    fields: &crate::insn::LoopFields,
    ip: usize,
    enter: &str,
    label: &dyn Fn(usize) -> u32,
) -> String {
    let max = if fields.max_iters == usize::MAX {
        "usize::MAX".to_string()
    } else {
        fields.max_iters.to_string()
    };
    format!(
        "match s.run_loop(base, {}, {}, {max}, {}, pos, {}, {}, {enter}) {{ Some(l) => {{ label = l; continue 'run; }} None => break 'fail }}\n",
        fields.loop_id,
        fields.min_iters,
        fields.greedy,
        label(ip + 1),
        label(fields.exit as usize)
    )
}

impl Codegen {
    pub fn new() -> Self {
        Self::default()
    }

    /// \return the Rust source of the matcher \p name for \p re: a function of type
    /// `aot::Attempt` and the functions it calls, named after it, which refer to the module
    /// `aot` and to the class tables `classes` emits. \p utf16 is whether the matcher reads
    /// the input `find_from_utf16` takes, decoding surrogate pairs, or that of
    /// `find_from_ucs2`. On error, \return why the regex has no matcher.
    pub fn matcher(&mut self, re: &Regex, name: &str, utf16: bool) -> Result<String, String> {
        let re = &re.cr;
        let mut generator = Gen {
            re,
            name,
            utf16,
            classes: &mut self.classes,
            bodies: bodies(re),
            first: firsts(re),
            untils: 0,
            out: String::new(),
        };
        let (g, l) = (re.groups, re.loops);
        writeln!(
            generator.out,
            "fn {name}(t: &[u16], pos: usize, bts: &mut Vec<aot::Bt>, steps: &mut aot::Steps, captures: &mut [Option<core::ops::Range<usize>>]) -> Option<usize> {{\n\
             let mut s = aot::State::<{g}, {l}>::new(bts, steps);\n\
             let end = {name}_0(t, pos, &mut s)?;\n\
             s.captures(captures);\n\
             Some(end)\n}}\n"
        )
        .unwrap();
        for body in generator.bodies.clone() {
            generator.function(body)?;
        }
        Ok(generator.out)
    }

    /// \return the bytes of an `aot::Factors` holding the factor strings of \p re, which
    /// `FactorSource::Given` takes in place of the analysis; None if \p re has none.
    pub fn factors(re: &Regex) -> Option<Vec<u8>> {
        re.cr
            .utf16
            .factors
            .as_ref()
            .map(crate::utf16factors::Factors::to_aot)
    }

    /// \return the Rust source of the class tables the matchers generated so far refer to.
    pub fn classes(&self) -> String {
        let mut classes: Vec<(&Vec<(u32, u32)>, usize)> = self
            .classes
            .iter()
            .map(|(ranges, &idx)| (ranges, idx))
            .collect();
        classes.sort_by_key(|&(_, idx)| idx);
        let mut out = String::new();
        for (ranges, idx) in classes {
            let mut dense = [0u64; 32];
            for &(first, last) in ranges.iter() {
                for cp in first..=last.min(0x7FF) {
                    dense[cp as usize / 64] |= 1 << (cp % 64);
                }
            }
            let dense: Vec<String> = dense.iter().map(|w| format!("{w:#x}")).collect();
            let sparse: Vec<String> = ranges
                .iter()
                .filter(|r| r.1 >= 0x800)
                .map(|&(first, last)| format!("({:#x}, {last:#x})", first.max(0x800)))
                .collect();
            writeln!(
                out,
                "static CLASS_{idx}: aot::Class = aot::Class {{ dense: [{}], ranges: &[{}] }};",
                dense.join(", "),
                sparse.join(", ")
            )
            .unwrap();
        }
        out
    }
}

/// The most instructions one walk of `sample` runs.
const SAMPLE_STEPS: usize = 10_000;

/// \return the UTF-16 units of one random walk of \p re's instructions from its start to its
/// goal, \p seed choosing each alternative, loop count and character; or None if the walk fails
/// or runs out of steps. Assertions are passed over, so the string often but not always matches:
/// it is input for comparing matchers, not a member of the language.
pub fn sample(re: &Regex, seed: u64) -> Option<Vec<u16>> {
    let re = &re.cr;
    let mut state = seed | 1;
    let mut below = move |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n.max(1) as u64) as usize
    };
    let mut out = Vec::new();
    let mut groups = vec![(usize::MAX, usize::MAX); re.groups as usize];
    let mut iters = vec![0usize; re.loops as usize];
    let mut ip = 0;
    for _ in 0..SAMPLE_STEPS {
        let insn = &re.insns[ip];
        if let Some(set) = char_set(re, insn) {
            let cp = pick(&set, &mut below)?;
            push_cp(&mut out, cp);
            ip += 1;
            continue;
        }
        ip = match *insn {
            Insn::Goal => return Some(out),
            Insn::Jump { target } => target as usize,
            Insn::Alt { secondary } if below(2) == 0 => secondary as usize,
            Insn::EnterLoop(ref fields) => {
                iters[fields.loop_id as usize] = 0;
                next_iteration(fields, ip, 0, &mut below)
            }
            Insn::LoopAgain { begin } => match &re.insns[begin as usize] {
                Insn::EnterLoop(fields) => {
                    let iteration = &mut iters[fields.loop_id as usize];
                    *iteration += 1;
                    next_iteration(fields, begin as usize, *iteration, &mut below)
                }
                _ => return None,
            },
            Insn::Loop1CharBody {
                min_iters,
                max_iters,
                ..
            } => {
                let set = char_set(re, &re.insns[ip + 1])?;
                let count = min_iters + below(max_iters.saturating_sub(min_iters).min(4) + 1);
                for _ in 0..count {
                    let cp = pick(&set, &mut below)?;
                    push_cp(&mut out, cp);
                }
                ip + 2
            }
            Insn::BeginCaptureGroup(id) => {
                groups[id as usize].0 = out.len();
                ip + 1
            }
            Insn::EndCaptureGroup(id) => {
                groups[id as usize].1 = out.len();
                ip + 1
            }
            Insn::ResetCaptureGroup(id) => {
                groups[id as usize] = (usize::MAX, usize::MAX);
                ip + 1
            }
            Insn::BackRef { group, .. } => {
                let (start, end) = groups[group as usize];
                if start <= end && end <= out.len() {
                    out.extend_from_within(start..end);
                }
                ip + 1
            }
            Insn::Lookahead { continuation, .. } | Insn::Lookbehind { continuation, .. } => {
                continuation as usize
            }
            Insn::Alt { .. }
            | Insn::StartOfLine { .. }
            | Insn::EndOfLine { .. }
            | Insn::WordBoundary { .. }
            | Insn::WordBoundaryUnicodeICase { .. } => ip + 1,
            _ => return None,
        };
    }
    None
}

/// \return where a walk goes after iteration \p iteration of the loop of \p fields at \p begin:
/// into its body while the minimum is not met, then into it or out of it at random, a few times
/// at most.
fn next_iteration(
    fields: &crate::insn::LoopFields,
    begin: usize,
    iteration: usize,
    below: &mut impl FnMut(usize) -> usize,
) -> usize {
    let more = iteration < fields.max_iters && iteration < fields.min_iters + 3 && below(2) == 0;
    if iteration < fields.min_iters || more {
        begin + 1
    } else {
        fields.exit as usize
    }
}

/// \return a character of \p set: often the first or last of one of its ranges, which tests
/// the bounds of a class, otherwise any of them.
fn pick(set: &CodePointSet, below: &mut impl FnMut(usize) -> usize) -> Option<u32> {
    let ivs = set.intervals();
    let iv = ivs.get(below(ivs.len()))?;
    let (first, last) = (iv.first, iv.last);
    Some(match below(4) {
        0 => first,
        1 => last,
        _ => first + below((last - first).min(0x3FF) as usize + 1) as u32,
    })
}

/// Append \p cp to \p out as UTF-16: a pair above U+FFFF, a unit otherwise.
fn push_cp(out: &mut Vec<u16>, cp: u32) {
    match char::from_u32(cp) {
        Some(c) if cp > UNIT_MAX => out.extend_from_slice(c.encode_utf16(&mut [0; 2])),
        _ => out.push(cp as u16),
    }
}
