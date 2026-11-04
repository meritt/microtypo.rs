pub const BLOCK_TAG_NAMES: &[&str] = &[
    "blockquote",
    "ul",
    "ol",
    "li",
    "dl",
    "dt",
    "dd",
    "table",
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "td",
    "th",
    "caption",
    "figure",
    "figcaption",
    "aside",
    "section",
    "article",
    "header",
    "footer",
    "nav",
    "main",
    "menu",
    "dialog",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "div",
    "form",
    "fieldset",
    "address",
    "details",
    "summary",
    "pre",
];

pub const VOID_BLOCK_TAG_NAMES: &[&str] = &["hr"];

pub const VOID_TAG_NAMES: &[&str] =
    &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];

pub fn implied_end_tags(name: &str) -> Option<&'static [&'static str]> {
    const CELLS: &[&str] = &["td", "th"];
    const DEFS: &[&str] = &["dt", "dd"];
    const SECTIONS: &[&str] = &["td", "th", "tr", "thead", "tbody", "tfoot"];
    match name {
        "li" => Some(&["li"]),
        "dt" | "dd" => Some(DEFS),
        "td" | "th" => Some(CELLS),
        "tr" => Some(&["td", "th", "tr"]),
        "thead" | "tbody" | "tfoot" => Some(SECTIONS),
        _ => None,
    }
}
