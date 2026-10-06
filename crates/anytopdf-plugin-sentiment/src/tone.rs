//! Rule-based tone tags for English text: `question`, `urgent` and
//! `complaint`. They describe what a segment says, never how its speaker or
//! writer feels.

/// Words that open a question even when a transcript drops the question mark.
const QUESTION_OPENERS: &[&str] = &[
    "who", "what", "when", "where", "why", "how", "which", "whose",
];

const URGENT: &[&str] = &[
    "urgent",
    "urgently",
    "asap",
    "as soon as possible",
    "immediately",
    "emergency",
    "right away",
    "right now",
    "time-sensitive",
    "time sensitive",
    "act now",
    "at once",
    "hurry",
    "deadline",
    "critical",
];

const COMPLAINT: &[&str] = &[
    "complain",
    "complaint",
    "complaints",
    "refund",
    "unacceptable",
    "disappointed",
    "disappointing",
    "frustrated",
    "frustrating",
    "broken",
    "doesn't work",
    "does not work",
    "didn't work",
    "did not work",
    "not working",
    "stopped working",
    "worst",
    "terrible",
    "awful",
    "horrible",
    "rip-off",
    "ripoff",
    "rip off",
    "useless",
    "waste of",
    "still waiting",
    "never arrived",
    "no response",
    "never answered",
    "faulty",
    "defective",
    "overcharged",
    "poor service",
];

/// Lowercased words with surrounding punctuation removed, so phrase matching
/// works on word boundaries.
fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| c.is_ascii_punctuation() && c != '\'')
                .to_lowercase()
                .replace('’', "'")
        })
        .filter(|w| !w.is_empty())
        .collect()
}

fn contains_phrase(words: &[String], phrase: &str) -> bool {
    let needle: Vec<&str> = phrase.split(' ').collect();
    words
        .windows(needle.len())
        .any(|window| window.iter().zip(&needle).all(|(w, n)| w == n))
}

/// Tone tags for a segment whose sentiment label is already known.
pub fn tags(text: &str, label: &str) -> Vec<&'static str> {
    let words = words(text);
    let mut tags = Vec::new();
    let trimmed = text.trim_end();
    if trimmed.ends_with('?')
        || trimmed.contains("? ")
        || (words.len() >= 3 && QUESTION_OPENERS.contains(&words[0].as_str()))
    {
        tags.push("question");
    }
    if URGENT.iter().any(|p| contains_phrase(&words, p)) || text.matches('!').count() >= 3 {
        tags.push("urgent");
    }
    if label == "negative" && COMPLAINT.iter().any(|p| contains_phrase(&words, p)) {
        tags.push("complaint");
    }
    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_are_found_with_or_without_a_question_mark() {
        assert_eq!(tags("Can you send it today?", "neutral"), ["question"]);
        assert_eq!(
            tags("how do we fix the login page", "neutral"),
            ["question"]
        );
        assert!(tags("How nice.", "positive").is_empty());
        assert!(tags("We shipped it.", "neutral").is_empty());
    }

    #[test]
    fn urgency_matches_whole_words_and_phrases() {
        assert_eq!(tags("Please reply ASAP.", "neutral"), ["urgent"]);
        assert_eq!(tags("We need this right now", "neutral"), ["urgent"]);
        assert_eq!(tags("Stop!!! Now!", "neutral"), ["urgent"]);
        assert!(tags("The criticalness of hurrying", "neutral").is_empty());
    }

    #[test]
    fn complaints_need_negative_text_and_a_complaint_cue() {
        assert_eq!(
            tags("This is the worst service, I want a refund.", "negative"),
            ["complaint"]
        );
        assert_eq!(tags("It still doesn’t work.", "negative"), ["complaint"]);
        assert!(tags("The worst is behind us and we won", "positive").is_empty());
        assert!(tags("It rained all day.", "negative").is_empty());
    }
}
