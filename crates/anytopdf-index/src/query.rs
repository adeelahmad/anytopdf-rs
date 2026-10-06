/// Turn a user's search text into an FTS5 query that cannot be a syntax error:
/// every word becomes a quoted term (all terms must match), text in double quotes
/// stays a phrase, and a trailing `*` on a word makes it a prefix search.
/// Returns `None` when the text holds no searchable term.
pub fn fts_query(text: &str) -> Option<String> {
    let mut terms = Vec::new();
    for (i, part) in text.split('"').enumerate() {
        if i % 2 == 1 {
            // Inside double quotes: one phrase.
            if let Some(phrase) = quoted(part) {
                terms.push(phrase);
            }
            continue;
        }
        for word in part.split_whitespace() {
            let (stem, prefix) = match word.strip_suffix('*') {
                Some(stem) => (stem, true),
                None => (word, false),
            };
            if let Some(term) = quoted(stem) {
                terms.push(if prefix { format!("{term}*") } else { term });
            }
        }
    }
    (!terms.is_empty()).then(|| terms.join(" "))
}

/// An FTS5 string literal, or `None` when the text has no letters or digits
/// (such a phrase matches nothing and only confuses the ranking).
fn quoted(text: &str) -> Option<String> {
    let text = text.trim();
    if !text.chars().any(char::is_alphanumeric) {
        return None;
    }
    Some(format!("\"{}\"", text.replace('"', "\"\"")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_become_quoted_terms_and_quotes_stay_phrases() {
        assert_eq!(fts_query("budget review").unwrap(), r#""budget" "review""#);
        assert_eq!(
            fts_query(r#"say "red car" now"#).unwrap(),
            r#""say" "red car" "now""#
        );
        assert_eq!(fts_query("invo*").unwrap(), r#""invo"*"#);
    }

    #[test]
    fn operators_and_punctuation_cannot_break_the_query() {
        assert_eq!(
            fts_query("NOT a-b OR (c) NEAR:").unwrap(),
            r#""NOT" "a-b" "OR" "(c)" "NEAR:""#
        );
        assert_eq!(
            fts_query(r#"unbalanced "quote"#).unwrap(),
            r#""unbalanced" "quote""#
        );
        assert_eq!(fts_query("  - * \"\" ()"), None);
        assert_eq!(fts_query(""), None);
    }
}
