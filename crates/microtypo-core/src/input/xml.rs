use std::borrow::Cow;
use std::sync::LazyLock;

use serde_json::{Value, json};

use crate::errors::{Error, Result};
use crate::js::{self, JsString, Lit, Re};
use crate::lit;
use crate::protect::blocks::{CheckBudget, Range};
use crate::shared::strings::is_flow_space;
use crate::shared::tag_name::is_tag_name_start;

pub const XML_BLOCKS: [(&str, &str, &str); 3] =
    [("xml-comment", "<!--", "-->"), ("xml-cdata", "<![CDATA[", "]]>"), ("xml-pi", "<?", "?>")];

static BLOCKS: LazyLock<[(JsString, JsString); 3]> =
    LazyLock::new(|| XML_BLOCKS.map(|(_, open, close)| (js::u(open), js::u(close))));

static DOCTYPE: Lit = lit!("<!DOCTYPE");
static XML_SPACE: Lit = lit!("xml:space");

fn is_name_body(c: u16) -> bool {
    !js::is_js_space(c) && c != 0x2F && c != 0x3E
}

fn is_attr_name(c: u16) -> bool {
    !js::is_js_space(c) && !matches!(c, 0x3D | 0x2F | 0x3C | 0x3E)
}

struct TagName<'t> {
    is_close: bool,
    name: &'t [u16],
    end: usize,
}

fn read_tag_name(masked: &[u16], i: usize, n: usize) -> Option<TagName<'_>> {
    let is_close = masked.get(i + 1) == Some(&0x2F);
    let name_start = if is_close { i + 2 } else { i + 1 };

    if name_start >= n || !is_tag_name_start(js::code_point_at(masked, name_start)) {
        return None;
    }

    let mut j = name_start + 1;

    while j < n && is_name_body(masked[j]) {
        j += 1;
    }

    Some(TagName { is_close, name: js::slice(masked, name_start, j), end: j })
}

static CHAR_REF_RE: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"&(?:#(\d+)|#x([\da-fA-F]+)|(amp|lt|gt|quot|apos));", "g"));

fn named_ref(name: &[u16]) -> &'static str {
    match js::lossy(name).as_str() {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        _ => "'",
    }
}

fn parse_digits(digits: &[u16], radix: u32) -> u64 {
    digits.iter().fold(0u64, |acc, &d| {
        let v = char::from_u32(u32::from(d)).and_then(|c| c.to_digit(radix)).unwrap_or(0);
        acc.saturating_mul(u64::from(radix)).saturating_add(u64::from(v))
    })
}

fn normalized_attr_value(raw: &[u16]) -> JsString {
    let spaced: JsString = raw.iter().map(|&c| if matches!(c, 0x09 | 0x0A | 0x0D) { 0x20 } else { c }).collect();
    CHAR_REF_RE.replace_with(&spaced, |c| {
        if let Some(named) = c.get(3) {
            return js::u(named_ref(named));
        }

        let code = match c.get(1) {
            Some(dec) => parse_digits(dec, 10),
            None => parse_digits(c.s(2), 16),
        };

        if code <= 0x10ffff { js::from_code_point(code as u32) } else { c.whole().to_vec() }
    })
}

fn skip_xml_space(masked: &[u16], i: usize, n: usize) -> usize {
    let mut j = i;

    while j < n && is_flow_space(masked.get(j).copied()) {
        j += 1;
    }

    j
}

struct TagAttributes {
    end: usize,
    preserve: bool,
    unterminated: bool,
}

fn read_tag_attributes(masked: &[u16], from: usize, n: usize) -> TagAttributes {
    let mut j = from;
    let mut preserve = false;

    while j < n {
        let ch = masked[j];

        if ch == 0x3C || ch == 0x3E {
            return TagAttributes { end: j, preserve, unterminated: false };
        }

        if !is_attr_name(ch) {
            j += 1;
            continue;
        }

        let name_start = j;

        while j < n && is_attr_name(masked[j]) {
            j += 1;
        }

        let name = js::slice(masked, name_start, j);

        j = skip_xml_space(masked, j, n);

        if masked.get(j) != Some(&0x3D) {
            continue;
        }

        j = skip_xml_space(masked, j + 1, n);

        let Some(&quote @ (0x22 | 0x27)) = masked.get(j) else { continue };

        let Some(close) = js::index_of_unit(masked, quote, j + 1) else {
            return TagAttributes { end: n, preserve, unterminated: true };
        };

        preserve = preserve
            || (name == XML_SPACE.as_slice()
                && normalized_attr_value(js::slice(masked, j + 1, close)) == js::u("preserve"));
        j = close + 1;
    }

    TagAttributes { end: n, preserve, unterminated: false }
}

fn unterminated(construct: &str, start: usize) -> Error {
    Error::input(
        format!("Unterminated XML {construct} construct at offset {start}"),
        json!({ "offset": start, "construct": construct }),
    )
}

fn block_end(text: &[u16], block: usize, start: usize, check_budget: CheckBudget<'_>) -> Result<usize> {
    let (id, open_str, _) = XML_BLOCKS[block];
    let (open, close) = &BLOCKS[block];
    let n = text.len();
    let mut i = start + open.len();
    let mut next_check = i + 0x4000;

    while i + close.len() <= n {
        if i >= next_check {
            check_budget(&format!("{id}-scan"))?;
            next_check = i + 0x4000;
        }

        if js::starts_with_at(text, close, i) {
            return Ok(i + close.len());
        }

        i += 1;
    }

    Err(unterminated(open_str, start))
}

pub fn doctype_end(text: &[u16], start: usize, check_budget: CheckBudget<'_>) -> Result<usize> {
    let n = text.len();
    let mut j = start + DOCTYPE.len();
    let mut depth = 0usize;
    let mut quote: Option<u16> = None;
    let mut next_check = j + 0x4000;

    while j < n {
        if j >= next_check {
            check_budget("xml-doctype-scan")?;
            next_check = j + 0x4000;
        }

        let c = text[j];

        if let Some(q) = quote {
            if c == q {
                quote = None;
            }

            j += 1;
            continue;
        }

        if c == 0x22 || c == 0x27 {
            quote = Some(c);
            j += 1;
            continue;
        }

        if c == 0x3C {
            if js::starts_with_at(text, &BLOCKS[0].0, j) {
                j = block_end(text, 0, j, check_budget)?;
                continue;
            }

            if js::starts_with_at(text, &BLOCKS[2].0, j) {
                j = block_end(text, 2, j, check_budget)?;
                continue;
            }
        }

        if c == 0x5B {
            depth += 1;
            j += 1;
            continue;
        }

        if c == 0x5D {
            depth = depth.saturating_sub(1);
            j += 1;
            continue;
        }

        if c == 0x3E && depth == 0 {
            return Ok(j + 1);
        }

        j += 1;
    }

    Err(unterminated("<!DOCTYPE", start))
}

pub fn scan_doctype(text: &[u16], check_budget: CheckBudget<'_>) -> Result<Vec<Range>> {
    if !js::includes(text, &DOCTYPE) {
        return Ok(Vec::new());
    }

    let n = text.len();
    let mut spans = Vec::new();
    let mut i = 0;
    let mut next_check = 0;

    while i < n {
        if i >= next_check {
            check_budget("xml-doctype-scan")?;
            next_check = i + 0x4000;
        }

        if text[i] == 0x3C && js::starts_with_at(text, &DOCTYPE, i) {
            let end = doctype_end(text, i, check_budget)?;
            spans.push((i, end));
            i = end;
            continue;
        }

        i += 1;
    }

    Ok(spans)
}

fn mask_xml_constructs<'t>(text: &'t [u16], check_budget: CheckBudget<'_>) -> Result<Cow<'t, [u16]>> {
    if !js::includes(text, &js::u("<!")) && !js::includes(text, &js::u("<?")) {
        return Ok(Cow::Borrowed(text));
    }

    let n = text.len();
    let mut out: Option<JsString> = None;
    let mut i = 0;
    let mut chunk_start = 0;
    let mut next_check = 0;

    while i < n {
        if i >= next_check {
            check_budget("xml-construct-scan")?;
            next_check = i + 0x4000;
        }

        if text[i] != 0x3C {
            i += 1;
            continue;
        }

        let end = if js::starts_with_at(text, &DOCTYPE, i) {
            Some(doctype_end(text, i, check_budget)?)
        } else {
            match BLOCKS.iter().position(|(open, _)| js::starts_with_at(text, open, i)) {
                Some(block) => Some(block_end(text, block, i, check_budget)?),
                None => None,
            }
        };

        let Some(end) = end else {
            i += 1;
            continue;
        };

        let chunks = out.get_or_insert_with(Vec::new);
        chunks.extend_from_slice(&text[chunk_start..i]);
        chunks.resize(chunks.len() + (end - i), 0x20);
        chunk_start = end;
        i = end;
    }

    let Some(mut chunks) = out else {
        return Ok(Cow::Borrowed(text));
    };

    chunks.extend_from_slice(js::from(text, chunk_start));

    Ok(Cow::Owned(chunks))
}

fn name_value(name: &[u16]) -> Value {
    Value::String(js::lossy(name))
}

pub fn validate_xml(text: &[u16], check_budget: CheckBudget<'_>) -> Result<()> {
    let masked = mask_xml_constructs(text, check_budget)?;
    let masked = masked.as_ref();
    let n = masked.len();
    let mut stack: Vec<&[u16]> = Vec::new();
    let mut i = 0;

    while i < n {
        if masked[i] != 0x3C {
            i += 1;
            continue;
        }

        let Some(tag) = read_tag_name(masked, i, n) else {
            i += 1;
            continue;
        };

        let TagName { is_close, name, .. } = tag;
        let attrs = read_tag_attributes(masked, tag.end, n);
        let j = attrs.end;

        if attrs.unterminated {
            return Err(Error::input(
                format!("Unterminated attribute quote in XML tag at offset {i}"),
                json!({ "offset": i }),
            ));
        }

        if masked.get(j) != Some(&0x3E) {
            i = j;
            continue;
        }

        let self_closing = masked[j - 1] == 0x2F;

        if is_close && skip_xml_space(masked, tag.end, n) != j {
            return Err(Error::input(
                format!("Malformed XML closing tag </{}> at offset {i}", js::lossy(name)),
                json!({ "offset": i, "name": name_value(name) }),
            ));
        }

        if is_close {
            let top = stack.pop();

            if top != Some(name) {
                return Err(Error::input(
                    format!("Mismatched XML closing tag </{}> at offset {i}", js::lossy(name)),
                    json!({ "offset": i, "name": name_value(name), "expected": top.map_or(Value::Null, name_value) }),
                ));
            }
        } else if !self_closing {
            stack.push(name);
        }

        i = j + 1;
    }

    if let Some(last) = stack.last() {
        return Err(Error::input(
            format!("Unclosed XML tag <{}> at end of document", js::lossy(last)),
            json!({ "name": name_value(last) }),
        ));
    }

    Ok(())
}

pub fn scan_xml_space_preserve(text: &[u16], check_budget: CheckBudget<'_>) -> Result<Vec<Range>> {
    if !js::includes(text, &XML_SPACE) {
        return Ok(Vec::new());
    }

    let masked = mask_xml_constructs(text, check_budget)?;
    let masked = masked.as_ref();
    let n = masked.len();
    let mut stack: Vec<&[u16]> = Vec::new();
    let mut spans = Vec::new();
    let mut preserve_depth: Option<usize> = None;
    let mut preserve_start = 0;
    let mut i = 0;

    while i < n {
        if masked[i] != 0x3C {
            i += 1;
            continue;
        }

        let Some(tag) = read_tag_name(masked, i, n) else {
            i += 1;
            continue;
        };

        let TagName { is_close, name, .. } = tag;
        let attrs = read_tag_attributes(masked, tag.end, n);
        let j = attrs.end;

        if attrs.unterminated || masked.get(j) != Some(&0x3E) {
            i = j;
            continue;
        }

        let self_closing = masked[j - 1] == 0x2F;
        let tag_end = j + 1;

        if is_close {
            stack.pop();

            if preserve_depth == Some(stack.len()) {
                spans.push((preserve_start, i));
                preserve_depth = None;
            }
        } else if !self_closing {
            if preserve_depth.is_none() && attrs.preserve {
                preserve_depth = Some(stack.len());
                preserve_start = tag_end;
            }

            stack.push(name);
        }

        i = tag_end;
    }

    Ok(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: CheckBudget<'static> = &|_| Ok(());

    fn validate(src: &str) -> std::result::Result<(), (String, Value)> {
        validate_xml(&js::u(src), OK).map_err(|e| (e.message, e.details))
    }

    fn preserve(src: &str) -> Vec<Range> {
        scan_xml_space_preserve(&js::u(src), OK).unwrap()
    }

    #[test]
    fn blocks_in_reference_order() {
        assert_eq!(XML_BLOCKS[0], ("xml-comment", "<!--", "-->"));
        assert_eq!(XML_BLOCKS[1], ("xml-cdata", "<![CDATA[", "]]>"));
        assert_eq!(XML_BLOCKS[2], ("xml-pi", "<?", "?>"));
    }

    #[test]
    fn comments_cdata_and_pi_hide_their_markup() {
        assert_eq!(validate("<a><!-- <b> --><![CDATA[<c>]]><?pi <d>?></a>"), Ok(()));
        assert_eq!(validate("<a> 1 < 2 <> </a>"), Ok(()));
    }

    #[test]
    fn doctype_with_internal_subset() {
        let src = "<!DOCTYPE note [<!ENTITY x \"a>b\"> <!-- ] > --> <?p ]>?> ]><note/>";
        assert_eq!(scan_doctype(&js::u(src), OK).unwrap(), [(0, 58)]);
        assert_eq!(validate(src), Ok(()));
        assert_eq!(doctype_end(&js::u("x<!DOCTYPE a \"q>\" [ [ ] ] >tail"), 1, OK).unwrap(), 27);
        assert_eq!(scan_doctype(&js::u("<a/>"), OK).unwrap(), [] as [(usize, usize); 0]);
    }

    #[test]
    fn xml_space_preserve_bodies() {
        let src = "<r><p xml:space=\"preserve\">  a <b>x</b> </p><q xml:space='pre&#115;erve'>z</q><s note='xml:space=\"preserve\"'>n</s><t other-xml:space=\"preserve\">o</t></r>";
        assert_eq!(preserve(src), [(27, 40), (73, 74)]);
        assert_eq!(
            preserve(
                "<r xml:space=\"preserve\"><a xml:space=\"preserve\">x</a><b/></r><c xml:space = \"\tpreserve\">y</c>"
            ),
            [(24, 57)]
        );
        assert_eq!(
            preserve(
                "<r xml:space=\"&#x70;reserve\">1</r><s xml:space=\"&#1114112;\">2</s><t xml:space=\"&amp;\">3</t>"
            ),
            [(29, 30)]
        );
        assert_eq!(preserve("<r><!-- xml:space=\"preserve\" --></r>"), [] as [(usize, usize); 0]);
    }

    #[test]
    fn astral_tag_names_and_spaced_closers() {
        assert_eq!(validate("<\u{1D49C}>x</\u{1D49C}>"), Ok(()));
        assert_eq!(validate("<a></a >"), Ok(()));
    }

    #[test]
    fn mismatched_and_unclosed_tags() {
        assert_eq!(
            validate("<a><b></a>"),
            Err((
                "Mismatched XML closing tag </a> at offset 6".into(),
                json!({ "offset": 6, "name": "a", "expected": "b" })
            ))
        );
        assert_eq!(
            validate("</a>"),
            Err((
                "Mismatched XML closing tag </a> at offset 0".into(),
                json!({ "offset": 0, "name": "a", "expected": null })
            ))
        );
        assert_eq!(
            validate("<a><b/>"),
            Err(("Unclosed XML tag <a> at end of document".into(), json!({ "name": "a" })))
        );
    }

    #[test]
    fn malformed_tags() {
        assert_eq!(
            validate("<a b=\"x>"),
            Err(("Unterminated attribute quote in XML tag at offset 0".into(), json!({ "offset": 0 })))
        );
        assert_eq!(
            validate("<a></a b>"),
            Err(("Malformed XML closing tag </a> at offset 3".into(), json!({ "offset": 3, "name": "a" })))
        );
    }

    #[test]
    fn unterminated_constructs() {
        assert_eq!(
            validate("<a><!-- x"),
            Err(("Unterminated XML <!-- construct at offset 3".into(), json!({ "offset": 3, "construct": "<!--" })))
        );
        let doctype = Err((
            "Unterminated XML <!DOCTYPE construct at offset 0".into(),
            json!({ "offset": 0, "construct": "<!DOCTYPE" }),
        ));
        assert_eq!(validate("<!DOCTYPE x [ > "), doctype);
        assert_eq!(
            scan_doctype(&js::u("<!DOCTYPE x [ > "), OK).map_err(|e| (e.message, e.details)).map(|_| ()),
            doctype
        );
    }
}
