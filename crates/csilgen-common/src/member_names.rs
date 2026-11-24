//! Identifiers for the members of a text enum (`"calendars.read" / "a-b c"`).
//!
//! A text enum value is any string, but a member name must be an identifier in the
//! target language. Each generator once made its own name from the raw value, and six
//! of them kept the dot in `calendars.read`, which does not compile. This module
//! splits a value into words in one way for every generator; the generator only
//! chooses the case and says which names are legal.

use std::collections::HashSet;

/// The words of a text enum value: runs of ASCII letters and digits. Every other
/// character (dot, hyphen, space, underscore, non-ASCII) separates words. A value
/// with no letter or digit gives the one word `value`.
pub fn member_words(value: &str) -> Vec<String> {
    let words: Vec<String> = value
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect();
    if words.is_empty() {
        vec!["value".to_string()]
    } else {
        words
    }
}

/// One identifier for each value, in order. `render` joins words in the target's case
/// (`["calendars", "read"]` to `CalendarsRead` or `CALENDARS_READ`). When `legal`
/// refuses the result (a keyword, or a leading digit such as `2fa`), the word `v` is
/// put first (`V2fa`). Two values that give one name (`a.b` and `a-b`) are told apart
/// by a number word at the end of the later one (`AB2`), so the names stay unique and
/// the first value keeps the plain name.
pub fn member_idents(
    values: &[&str],
    render: impl Fn(&[String]) -> String,
    legal: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut taken = HashSet::new();
    values
        .iter()
        .map(|value| {
            let mut words = member_words(value);
            if !legal(&render(&words)) {
                words.insert(0, "v".to_string());
            }
            let mut name = render(&words);
            let mut n = 2;
            while !taken.insert(name.clone()) {
                let mut numbered = words.clone();
                numbered.push(n.to_string());
                name = render(&numbered);
                n += 1;
            }
            name
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pascal(words: &[String]) -> String {
        words
            .iter()
            .map(|word| {
                let mut chars = word.chars();
                chars
                    .next()
                    .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())
                    .unwrap_or_default()
            })
            .collect()
    }

    fn legal(name: &str) -> bool {
        !name.starts_with(|c: char| c.is_ascii_digit()) && name != "Class"
    }

    #[test]
    fn values_that_are_not_identifiers_become_legal_unique_names() {
        let names = member_idents(
            &[
                "calendars.read",
                "events.write",
                "a-b c",
                "2fa",
                "class",
                "a.b.c",
                "...",
            ],
            pascal,
            legal,
        );
        assert_eq!(
            names,
            vec![
                "CalendarsRead",
                "EventsWrite",
                "ABC",
                "V2fa",
                "VClass",
                "ABC2",
                "Value"
            ]
        );
    }

    #[test]
    fn words_split_on_every_non_alphanumeric_character() {
        assert_eq!(member_words("calendars.read"), vec!["calendars", "read"]);
        assert_eq!(member_words("snake_case"), vec!["snake", "case"]);
        assert_eq!(member_words("é"), vec!["value"]);
    }
}
