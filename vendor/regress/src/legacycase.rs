//! Case-insensitive matching for a pattern without the `u` flag (ECMA-262 Canonicalize with
//! `rer.[[IgnoreCase]]` true and `HasEitherUnicodeFlag(rer)` false).
//!
//! Patched into the vendored crate: upstream folds non-Unicode patterns with Unicode simple case
//! folding, which makes `/s/i` match `ſ` and `/[a-z]/i` match the Kelvin sign, while JavaScript
//! leaves every code unit whose uppercase would cross into ASCII as it is.

use crate::codepointset::CodePointSet;
use crate::legacycasetable::LEGACY_CANON;

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;

pub(crate) fn canonicalize(cu: u32) -> u32 {
    if cu > 0xFFFF {
        return cu;
    }
    match LEGACY_CANON.binary_search_by_key(&(cu as u16), |&(from, _)| from) {
        Ok(i) => u32::from(LEGACY_CANON[i].1),
        Err(_) => cu,
    }
}

// Every code unit whose canonical form is `target`.
fn preimage(target: u32, out: &mut Vec<u32>) {
    if canonicalize(target) == target {
        out.push(target);
    }
    for &(from, to) in LEGACY_CANON {
        if u32::from(to) == target {
            out.push(u32::from(from));
        }
    }
}

/// All code units a literal `c` matches.
pub(crate) fn unfold(c: u32) -> Vec<u32> {
    let mut res = Vec::from([c]);
    preimage(canonicalize(c), &mut res);
    res.sort_unstable();
    res.dedup();
    res
}

/// The set of code units whose canonical form is the canonical form of some member of `input`.
pub(crate) fn add_icase_code_points(input: CodePointSet) -> CodePointSet {
    // The canonical forms some member maps onto through the table, gathered in one pass so the
    // second pass asks each of them in O(log n) rather than rescanning the table.
    let mut mapped: Vec<u16> = LEGACY_CANON
        .iter()
        .filter(|&&(from, _)| input.contains(u32::from(from)))
        .map(|&(_, to)| to)
        .collect();
    mapped.sort_unstable();
    mapped.dedup();

    let mut result = input.clone();
    for &(from, to) in LEGACY_CANON {
        let (from, to32) = (u32::from(from), u32::from(to));
        let fixed = canonicalize(to32) == to32;
        // `from` is in the canonical image when its own form is reached by a member: through the
        // table, or by being that form itself.
        if mapped.binary_search(&to).is_ok() || (fixed && input.contains(to32)) {
            result.add_one(from);
            if fixed {
                result.add_one(to32);
            }
        }
    }
    result
}
