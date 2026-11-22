use serde_json::{Map, Value};

use crate::errors::{Result, usage};
use crate::js;
use crate::util::{coerce_scalar, set_path};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Boolean,
    Text,
    List,
}

const OPTIONS: [(&str, Kind, Option<char>); 18] = [
    ("html", Kind::Boolean, None),
    ("entities", Kind::Boolean, None),
    ("input", Kind::Text, None),
    ("stdin-filepath", Kind::Text, None),
    ("check", Kind::Boolean, None),
    ("diff", Kind::Boolean, None),
    ("explain", Kind::Boolean, None),
    ("hanging", Kind::Boolean, None),
    ("max-input", Kind::Text, None),
    ("max-ms", Kind::Text, None),
    ("rule", Kind::List, None),
    ("render", Kind::List, None),
    ("set", Kind::List, None),
    ("config", Kind::Text, None),
    ("write", Kind::Boolean, Some('w')),
    ("output-file", Kind::Text, Some('o')),
    ("help", Kind::Boolean, Some('h')),
    ("version", Kind::Boolean, None),
];

fn kind_of(name: &str) -> Option<Kind> {
    OPTIONS.iter().find(|(n, ..)| *n == name).map(|&(_, kind, _)| kind)
}

fn takes_value(name: &str) -> bool {
    matches!(kind_of(name), Some(Kind::Text | Kind::List))
}

fn long_for_short(short: char) -> String {
    OPTIONS.iter().find(|(.., s)| *s == Some(short)).map_or_else(|| short.to_string(), |(n, ..)| (*n).to_owned())
}

fn short_of(name: &str) -> Option<char> {
    OPTIONS.iter().find(|(n, ..)| *n == name).and_then(|&(.., s)| s)
}

struct Token {
    name: String,
    raw_name: String,
    raw_json: Option<String>,
    value: Option<String>,
    inline: bool,
}

enum Item {
    Option(Token),
    Positional(String),
}

fn second(arg: &str) -> Option<char> {
    arg.chars().nth(1)
}

fn lone_short(raw_name: String) -> Token {
    let name = long_for_short(second(&raw_name).unwrap_or_default());
    Token { name, raw_name, raw_json: None, value: None, inline: false }
}

fn tokens(args: &[String]) -> Vec<Item> {
    let mut items = Vec::new();
    let mut rest: std::collections::VecDeque<String> = args.iter().cloned().collect();
    while let Some(arg) = rest.pop_front() {
        let length = arg.encode_utf16().count();
        let dash = arg.starts_with('-');
        if arg == "--" {
            items.extend(rest.drain(..).map(Item::Positional));
            break;
        }
        if length == 2 && dash && second(&arg) != Some('-') {
            let mut token = lone_short(arg);
            if takes_value(&token.name) {
                token.value = rest.pop_front();
            }
            items.push(Item::Option(token));
            continue;
        }
        if length > 2 && dash && second(&arg) != Some('-') {
            let short = second(&arg).unwrap_or_default();
            let name = long_for_short(short);
            if takes_value(&name) {
                let value = arg.chars().skip(2).collect();
                items.push(Item::Option(Token {
                    name,
                    raw_name: format!("-{short}"),
                    raw_json: None,
                    value: Some(value),
                    inline: true,
                }));
            } else {
                let chars: Vec<char> = arg.chars().skip(1).collect();
                let mut expanded = Vec::new();
                for (i, &c) in chars.iter().enumerate() {
                    if c.len_utf16() == 2 {
                        items.extend(expanded.drain(..).map(|short| Item::Option(lone_short(short))));
                        let mut units = [0; 2];
                        items.extend(c.encode_utf16(&mut units).iter().map(|unit| {
                            Item::Option(Token {
                                name: "\u{FFFD}".to_owned(),
                                raw_name: "-\u{FFFD}".to_owned(),
                                raw_json: Some(format!("\"-\\u{unit:04x}\"")),
                                value: None,
                                inline: false,
                            })
                        }));
                    } else if !takes_value(&long_for_short(c)) || i == chars.len() - 1 {
                        expanded.push(format!("-{c}"));
                    } else {
                        expanded.push(format!("-{}", chars[i..].iter().collect::<String>()));
                        break;
                    }
                }
                for item in expanded.into_iter().rev() {
                    rest.push_front(item);
                }
            }
            continue;
        }
        if length > 2 && arg.starts_with("--") {
            match arg.find('=') {
                Some(eq) if arg.chars().skip(3).any(|c| c == '=') => {
                    let name = arg[2..eq].to_owned();
                    items.push(Item::Option(Token {
                        raw_name: format!("--{name}"),
                        name,
                        raw_json: None,
                        value: Some(arg[eq + 1..].to_owned()),
                        inline: true,
                    }));
                }
                _ => {
                    let name = arg[2..].to_owned();
                    let value = if takes_value(&name) { rest.pop_front() } else { None };
                    items.push(Item::Option(Token { name, raw_name: arg, raw_json: None, value, inline: false }));
                }
            }
            continue;
        }
        items.push(Item::Positional(arg));
    }
    items
}

fn unknown(token: &Token) -> String {
    format!(
        "Unknown option '{}'. To specify a positional argument starting with a '-', place it at the end of the command after '--', as in '-- {}",
        token.raw_name,
        token.raw_json.clone().unwrap_or_else(|| js::stringify(&token.raw_name))
    )
}

fn check(token: &Token) -> Result<String> {
    let mut name = token.name.as_str();
    if kind_of(name).is_none() {
        match name.strip_prefix("no-") {
            Some(base) if kind_of(base) == Some(Kind::Boolean) => name = base,
            _ => return Err(usage(unknown(token))),
        }
    }
    let both = match short_of(name) {
        Some(s) => format!("-{s}, --{name}"),
        None => format!("--{name}"),
    };
    let kind = kind_of(name).expect("checked above");
    if kind != Kind::Boolean && token.value.is_none() {
        return Err(usage(format!("Option '{both} <value>' argument missing")));
    }
    if kind == Kind::Boolean && token.value.is_some() {
        return Err(usage(format!("Option '{both}' does not take an argument")));
    }
    if let Some(value) = &token.value
        && !token.inline
        && value.encode_utf16().count() > 1
        && value.starts_with('-')
    {
        let raw = &token.raw_name;
        let example = if raw.starts_with("--") {
            format!("'{raw}=-XYZ'")
        } else {
            format!("'--{}=-XYZ' or '{raw}-XYZ'", token.name)
        };
        return Err(usage(format!(
            "Option '{raw}' argument is ambiguous.\nDid you forget to specify the option argument for '{raw}'?\nTo specify an option argument starting with a dash use {example}."
        )));
    }
    Ok(name.to_owned())
}

#[derive(Default)]
struct Values {
    flags: Vec<(String, bool)>,
    text: Vec<(String, String)>,
    lists: Vec<(String, String)>,
}

impl Values {
    fn flag(&self, name: &str) -> Option<bool> {
        self.flags.iter().rev().find(|(n, _)| n == name).map(|&(_, v)| v)
    }

    fn text(&self, name: &str) -> Option<&str> {
        self.text.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    fn list(&self, name: &str) -> impl Iterator<Item = &str> {
        self.lists.iter().filter(move |(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

pub struct Options {
    pub help: bool,
    pub version: bool,
    pub write: bool,
    pub check: bool,
    pub diff: bool,
    pub explain: bool,
    pub stdin_filepath: Option<String>,
    pub output_file: Option<String>,
    pub config_path: Option<String>,
    pub files: Vec<String>,
    pub config: Map<String, Value>,
}

pub fn parse(args: &[String]) -> Result<Options> {
    let mut values = Values::default();
    let mut files = Vec::new();
    for item in tokens(args) {
        match item {
            Item::Positional(p) => files.push(p),
            Item::Option(token) => {
                let name = check(&token)?;
                match (kind_of(&name), token.value) {
                    (Some(Kind::Boolean), _) => {
                        let on = token.name == name;
                        values.flags.push((name, on));
                    }
                    (Some(Kind::List), Some(v)) => values.lists.push((name, v)),
                    (_, Some(v)) => values.text.push((name, v)),
                    (_, None) => {}
                }
            }
        }
    }
    Ok(Options {
        help: values.flag("help") == Some(true),
        version: values.flag("version") == Some(true),
        write: values.flag("write") == Some(true),
        check: values.flag("check") == Some(true),
        diff: values.flag("diff") == Some(true),
        explain: values.flag("explain") == Some(true),
        stdin_filepath: values.text("stdin-filepath").map(str::to_owned),
        output_file: values.text("output-file").map(str::to_owned),
        config_path: values.text("config").map(str::to_owned),
        config: build_config(&values)?,
        files,
    })
}

fn build_config(values: &Values) -> Result<Map<String, Value>> {
    let mut config = Map::new();
    for key in ["html", "entities"] {
        if let Some(v) = values.flag(key) {
            config.insert(key.to_owned(), Value::Bool(v));
        }
    }
    if let Some(v) = values.text("input") {
        config.insert("input".to_owned(), Value::String(v.to_owned()));
    }
    if let Some(v) = values.text("max-input") {
        config.insert("maxInputLength".to_owned(), js::number(js::to_number(v)));
    }
    if let Some(v) = values.text("max-ms") {
        config.insert("maxProcessingMs".to_owned(), js::number(js::to_number(v)));
    }

    let mut rules = Map::new();
    if let Some(v) = values.flag("hanging") {
        rules.insert("hanging.quote".to_owned(), Value::Bool(v));
        rules.insert("hanging.bracket".to_owned(), Value::Bool(v));
    }
    for entry in values.list("rule") {
        let (id, raw) = split_pair(entry)?;
        if id != "__proto__" {
            rules.insert(id.to_owned(), coerce_scalar(raw));
        }
    }
    if !rules.is_empty() {
        config.insert("rules".to_owned(), Value::Object(rules));
    }

    let mut render = Map::new();
    for entry in values.list("render") {
        let (key, raw) = split_pair(entry)?;
        if key != "__proto__" {
            render.insert(key.to_owned(), coerce_scalar(raw));
        }
    }
    if !render.is_empty() {
        config.insert("render".to_owned(), Value::Object(render));
    }

    for entry in values.list("set") {
        let (path, raw) = split_pair(entry)?;
        set_path(&mut config, path, coerce_scalar(raw))?;
    }
    Ok(config)
}

fn split_pair(entry: &str) -> Result<(&str, &str)> {
    entry.split_once('=').ok_or_else(|| usage(format!("expected key=value, got: {entry}")))
}
