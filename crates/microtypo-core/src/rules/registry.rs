use std::sync::LazyLock;

use crate::rule_runner::RuleGroupDef;

pub fn default_groups() -> Vec<(&'static str, RuleGroupDef)> {
    DEFAULT_GROUPS.clone()
}

static DEFAULT_GROUPS: LazyLock<Vec<(&'static str, RuleGroupDef)>> = LazyLock::new(|| {
    vec![
        ("quote", super::quote::quote_group()),
        ("dash", super::dash::dash_group()),
        ("symbol", super::symbol::symbol_group()),
        ("punctuation", super::punctmark::punctmark_group()),
        ("number", super::number::number_group()),
        ("space", super::space::space_group()),
        ("abbr", super::abbr::abbr_group()),
        ("nobr", super::nobr::nobr_group()),
        ("date", super::date::date_group()),
        ("hanging", super::opt_align::opt_align_group()),
        ("other", super::etc::etc_group()),
        ("text", super::text::text_group()),
    ]
});

static RULE_IDS: LazyLock<Vec<(&'static str, Vec<String>)>> = LazyLock::new(|| {
    default_groups().into_iter().map(|(name, def)| (name, def.rules.into_iter().map(|r| r.id).collect())).collect()
});

pub fn default_rule_ids(group: &str) -> &'static [String] {
    RULE_IDS.iter().find(|(n, _)| *n == group).map_or(&[], |(_, ids)| ids.as_slice())
}
