use std::collections::HashMap;
use std::fmt;
use std::sync::LazyLock;

use crate::js::{self, JsString, Re};
use crate::protect::placeholders::{BREAKLINE, PARAGRAPH_CLOSE, PARAGRAPH_OPEN, PLACEHOLDER_TOKEN};

pub static PLACEHOLDER_TOKEN_RE: LazyLock<Re> = LazyLock::new(|| Re::new(&PLACEHOLDER_TOKEN, "gu"));

static PARAGRAPH_TOKEN_RE: LazyLock<Re> = LazyLock::new(|| {
    let source = [&*PARAGRAPH_OPEN, &*PARAGRAPH_CLOSE, &*BREAKLINE].map(|t| js::regex::escape(t)).join("|");
    Re::new(&source, "g")
});

fn count_into(text: &[u16], re: &Re, into: &mut HashMap<JsString, usize>) {
    for m in re.find_all(text) {
        *into.entry(m.whole().to_vec()).or_insert(0) += 1;
    }
}

pub struct TokenBudget {
    before: JsString,
    allowed: Option<HashMap<JsString, usize>>,
    used: HashMap<JsString, usize>,
    sequence_regexes: Vec<Re>,
    touched: bool,
}

impl TokenBudget {
    pub fn new(before: JsString, sequence_regexes: Vec<Re>) -> Self {
        TokenBudget { before, allowed: None, used: HashMap::new(), sequence_regexes, touched: false }
    }

    pub fn untouched(&self) -> bool {
        !self.touched
    }

    fn budget(&mut self) -> &mut HashMap<JsString, usize> {
        if self.allowed.is_none() {
            let mut allowed = HashMap::new();
            count_into(&self.before, &PLACEHOLDER_TOKEN_RE, &mut allowed);
            count_into(&self.before, &PARAGRAPH_TOKEN_RE, &mut allowed);
            for re in &self.sequence_regexes {
                count_into(&self.before, re, &mut allowed);
            }
            self.allowed = Some(allowed);
        }
        self.allowed.as_mut().expect("budget counted")
    }

    pub fn mint(&mut self, token: &[u16]) {
        self.touched = true;
        *self.budget().entry(token.to_vec()).or_insert(0) += 1;
    }

    pub fn scrub(&mut self, text: &[u16]) -> JsString {
        self.touched = true;
        self.budget();
        let allowed = self.allowed.as_ref().expect("budget counted");
        let used = &mut self.used;
        let mut consume = |m: &[u16]| -> JsString {
            let cap = allowed.get(m).copied().unwrap_or(0);
            let seen = used.entry(m.to_vec()).or_insert(0);
            if *seen < cap {
                *seen += 1;
                m.to_vec()
            } else {
                Vec::new()
            }
        };
        let mut result = PLACEHOLDER_TOKEN_RE.replace_with(text, |c| consume(c.whole()));
        result = PARAGRAPH_TOKEN_RE.replace_with(&result, |c| consume(c.whole()));
        for re in &self.sequence_regexes {
            result = re.replace_with(&result, |c| consume(c.whole()));
        }
        result
    }
}

impl fmt::Debug for TokenBudget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenBudget").field("touched", &self.touched).finish_non_exhaustive()
    }
}
