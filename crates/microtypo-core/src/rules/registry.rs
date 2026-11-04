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

#[cfg(test)]
mod tests {
    use super::*;

    const REFERENCE: &[(&str, &str, &str)] = &[
        ("quote", "Кавычки", "around_link open open_cyrillic close fix_orphan_open nested"),
        (
            "dash",
            "Дефисы и тире",
            "em_double_hyphen en_range em em_line_start em_after_sentence compound_preposition indefinite_pronoun hyphenated_particle emphatic_particle",
        ),
        ("symbol", "Специальные символы", "trademark registered copyright apostrophe:c fahrenheit arrows"),
        (
            "punctuation",
            "Пунктуация и знаки препинания",
            "comma_before_conjunction:- repeat_limit collapse_repeated:c ellipsis mark_ellipsis swap_exclamation_question drop_terminal_garbage collapse_doubled trim_bracket_terminal_punct bracket_spaces space_before_bracket period_at_end:-",
        ),
        (
            "number",
            "Числа, дроби, математические знаки",
            "time en_range minus_range times:c sub:h sup:h fraction math thin_space_triads nbsp_numero nbsp_section",
        ),
        (
            "space",
            "Расстановка и удаление пробелов",
            "nbsp_two_letter trim_before_punctuation space_after_comma space_after_punctuation space_after_period space_after_ellipsis collapse_spaces strip_quote_padding trim_before_percent nbsp_before_quote nbsp_before_month trim_line_end no_space_after_opening_ellipsis trim_before_parenthetical_ellipsis space_after_year",
        ),
        (
            "abbr",
            "Сокращения",
            "nbsp_dpi nbsp_short_acronym nbsp_reference nbsp_location nbsp_unit nbsp_fraction_unit nbsp_weight_unit nbsp_volume_unit nbsp_time_unit nbsp_data_unit nbsp_frequency_unit nbsp_css_unit nbsp_volt nbsp_magnitude nowrap_postscript nowrap_etc:c nowrap_ie nowrap_era nbsp_honorific currency nbsp_money_magnitude nbsp_currency_prefix nbsp_organization nowrap_gost",
        ),
        (
            "nobr",
            "Неразрывные конструкции",
            "nbsp_short_word nbsp_final_word nowrap_phone nowrap_phone_alt nowrap_ip initials_dots:- nowrap_initials nbsp_particle nbsp_after_particle nbsp_namely nbsp_celsius nowrap_hyphen_short:-c nowrap_hyphen:-c",
        ),
        (
            "date",
            "Даты и дни",
            "year_range month_range day_month_range nowrap_day_range:- nowrap_date nbsp_year_suffix",
        ),
        ("hanging", "Оптическое выравнивание", "quote:-h bracket:-h"),
        (
            "other",
            "Прочее",
            "acute_accent sup_word:h roman_range_dash en_century_range en_time_range split_triads drop_nested_nowrap strip_nbsp_in_nowrap nbsp_in_nowrap:-",
        ),
        ("text", "Текст и абзацы", "autolink:h email:h deduplicate_words:-h paragraphs:h breakline:h"),
    ];

    #[test]
    fn groups_match_the_reference() {
        let groups = default_groups();
        assert_eq!(groups.len(), REFERENCE.len());

        for ((name, def), (ref_name, ref_title, ref_rules)) in groups.iter().zip(REFERENCE) {
            assert_eq!(name, ref_name);
            assert_eq!(def.title.as_deref(), Some(*ref_title), "{name}");

            let rules: Vec<String> = def
                .rules
                .iter()
                .map(|r| {
                    let flags = format!(
                        "{}{}{}",
                        if r.enabled { "" } else { "-" },
                        if r.html_only { "h" } else { "" },
                        if r.cycled { "c" } else { "" }
                    );
                    if flags.is_empty() { r.id.clone() } else { format!("{}:{flags}", r.id) }
                })
                .collect();
            assert_eq!(rules.join(" "), *ref_rules, "{name}");
        }
    }
}
