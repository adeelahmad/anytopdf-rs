//! A Rust port of VADER (Hutto & Gilbert, ICWSM 2014), a rule-based sentiment
//! scorer tuned for short, informal English text such as transcripts and
//! captions. The lexicon in `data/vader_lexicon.tsv` is the upstream
//! `vader_lexicon.txt` reduced to its word and mean-valence columns (MIT
//! licence in `data/VADER-LICENSE`).
//!
//! The scoring follows the reference implementation, with one deliberate
//! difference: "but" scales every word before it by 0.5 and after it by 1.5 by
//! position, where the reference looks words up by value and so mis-scales
//! repeated valences.

use std::{collections::HashMap, sync::OnceLock};

const B_INCR: f64 = 0.293;
const B_DECR: f64 = -0.293;
const C_INCR: f64 = 0.733;
const N_SCALAR: f64 = -0.74;

const NEGATE: &[&str] = &[
    "aint",
    "arent",
    "cannot",
    "cant",
    "couldnt",
    "darent",
    "didnt",
    "doesnt",
    "ain't",
    "aren't",
    "can't",
    "couldn't",
    "daren't",
    "didn't",
    "doesn't",
    "dont",
    "hadnt",
    "hasnt",
    "havent",
    "isnt",
    "mightnt",
    "mustnt",
    "neither",
    "don't",
    "hadn't",
    "hasn't",
    "haven't",
    "isn't",
    "mightn't",
    "mustn't",
    "neednt",
    "needn't",
    "never",
    "none",
    "nope",
    "nor",
    "not",
    "nothing",
    "nowhere",
    "oughtnt",
    "shant",
    "shouldnt",
    "uhuh",
    "wasnt",
    "werent",
    "oughtn't",
    "shan't",
    "shouldn't",
    "uh-uh",
    "wasn't",
    "weren't",
    "without",
    "wont",
    "wouldnt",
    "won't",
    "wouldn't",
    "rarely",
    "seldom",
    "despite",
];

const BOOSTERS: &[(&str, f64)] = &[
    ("absolutely", B_INCR),
    ("amazingly", B_INCR),
    ("awfully", B_INCR),
    ("completely", B_INCR),
    ("considerable", B_INCR),
    ("considerably", B_INCR),
    ("decidedly", B_INCR),
    ("deeply", B_INCR),
    ("effing", B_INCR),
    ("enormous", B_INCR),
    ("enormously", B_INCR),
    ("entirely", B_INCR),
    ("especially", B_INCR),
    ("exceptional", B_INCR),
    ("exceptionally", B_INCR),
    ("extreme", B_INCR),
    ("extremely", B_INCR),
    ("fabulously", B_INCR),
    ("flipping", B_INCR),
    ("flippin", B_INCR),
    ("frackin", B_INCR),
    ("fracking", B_INCR),
    ("fricking", B_INCR),
    ("frickin", B_INCR),
    ("frigging", B_INCR),
    ("friggin", B_INCR),
    ("fully", B_INCR),
    ("fuckin", B_INCR),
    ("fucking", B_INCR),
    ("fuggin", B_INCR),
    ("fugging", B_INCR),
    ("greatly", B_INCR),
    ("hella", B_INCR),
    ("highly", B_INCR),
    ("hugely", B_INCR),
    ("incredible", B_INCR),
    ("incredibly", B_INCR),
    ("intensely", B_INCR),
    ("major", B_INCR),
    ("majorly", B_INCR),
    ("more", B_INCR),
    ("most", B_INCR),
    ("particularly", B_INCR),
    ("purely", B_INCR),
    ("quite", B_INCR),
    ("really", B_INCR),
    ("remarkably", B_INCR),
    ("so", B_INCR),
    ("substantially", B_INCR),
    ("thoroughly", B_INCR),
    ("total", B_INCR),
    ("totally", B_INCR),
    ("tremendous", B_INCR),
    ("tremendously", B_INCR),
    ("uber", B_INCR),
    ("unbelievably", B_INCR),
    ("unusually", B_INCR),
    ("utter", B_INCR),
    ("utterly", B_INCR),
    ("very", B_INCR),
    ("almost", B_DECR),
    ("barely", B_DECR),
    ("hardly", B_DECR),
    ("just enough", B_DECR),
    ("kind of", B_DECR),
    ("kinda", B_DECR),
    ("kindof", B_DECR),
    ("kind-of", B_DECR),
    ("less", B_DECR),
    ("little", B_DECR),
    ("marginal", B_DECR),
    ("marginally", B_DECR),
    ("occasional", B_DECR),
    ("occasionally", B_DECR),
    ("partly", B_DECR),
    ("scarce", B_DECR),
    ("scarcely", B_DECR),
    ("slight", B_DECR),
    ("slightly", B_DECR),
    ("somewhat", B_DECR),
    ("sort of", B_DECR),
    ("sorta", B_DECR),
    ("sortof", B_DECR),
    ("sort-of", B_DECR),
];

const SPECIAL_CASES: &[(&str, f64)] = &[
    ("the shit", 3.0),
    ("the bomb", 3.0),
    ("bad ass", 1.5),
    ("badass", 1.5),
    ("bus stop", 0.0),
    ("yeah right", -2.0),
    ("kiss of death", -1.5),
    ("to die for", 3.0),
    ("beating heart", 3.1),
    ("broken heart", -2.9),
];

fn lexicon() -> &'static HashMap<&'static str, f64> {
    static LEXICON: OnceLock<HashMap<&'static str, f64>> = OnceLock::new();
    LEXICON.get_or_init(|| {
        include_str!("../data/vader_lexicon.tsv")
            .lines()
            .filter_map(|line| {
                let (word, valence) = line.split_once('\t')?;
                Some((word, valence.trim().parse().ok()?))
            })
            .collect()
    })
}

fn booster(word: &str) -> Option<f64> {
    BOOSTERS.iter().find(|(w, _)| *w == word).map(|(_, v)| *v)
}

fn special_case(phrase: &str) -> Option<f64> {
    SPECIAL_CASES
        .iter()
        .find(|(p, _)| *p == phrase)
        .map(|(_, v)| *v)
}

/// Python's `str.isupper`: at least one cased character, none lowercase.
fn is_upper(word: &str) -> bool {
    word.chars().any(char::is_uppercase) && !word.chars().any(char::is_lowercase)
}

fn negated(word: &str) -> bool {
    NEGATE.contains(&word) || word.contains("n't")
}

/// Splits on whitespace and strips surrounding punctuation from words, keeping
/// short tokens such as `:)` whole because they are probably emoticons.
fn tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|token| {
            let stripped = token.trim_matches(|c: char| c.is_ascii_punctuation());
            if stripped.chars().count() <= 2 {
                token.to_string()
            } else {
                stripped.to_string()
            }
        })
        .collect()
}

/// The VADER compound score of `text`, from -1 (most negative) to 1 (most
/// positive).
pub fn compound(text: &str) -> f64 {
    let words = tokens(text);
    if words.is_empty() {
        return 0.0;
    }
    let lower: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
    let all_caps = words.iter().filter(|w| is_upper(w)).count();
    let cap_diff = all_caps > 0 && all_caps < words.len();
    let lexicon = lexicon();

    let mut sentiments = Vec::with_capacity(words.len());
    for i in 0..words.len() {
        if booster(&lower[i]).is_some()
            || (lower[i] == "kind" && lower.get(i + 1).is_some_and(|w| w == "of"))
        {
            sentiments.push(0.0);
            continue;
        }
        sentiments.push(valence(&words, &lower, i, cap_diff, lexicon));
    }

    if let Some(bi) = lower.iter().position(|w| w == "but") {
        for (si, s) in sentiments.iter_mut().enumerate() {
            if si < bi {
                *s *= 0.5;
            } else if si > bi {
                *s *= 1.5;
            }
        }
    }
    score(&sentiments, text)
}

fn valence(
    words: &[String],
    lower: &[String],
    i: usize,
    cap_diff: bool,
    lexicon: &HashMap<&str, f64>,
) -> f64 {
    let Some(&base) = lexicon.get(lower[i].as_str()) else {
        return 0.0;
    };
    let in_lexicon = |w: &str| lexicon.contains_key(w);
    let mut valence = base;
    if lower[i] == "no" && lower.get(i + 1).is_some_and(|w| in_lexicon(w)) {
        // "no" as a determiner ("no problems") is not itself negative.
        valence = 0.0;
    }
    if (i > 0 && lower[i - 1] == "no")
        || (i > 1 && lower[i - 2] == "no")
        || (i > 2 && lower[i - 3] == "no" && (lower[i - 1] == "or" || lower[i - 1] == "nor"))
    {
        valence = base * N_SCALAR;
    }
    if is_upper(&words[i]) && cap_diff {
        valence += if valence > 0.0 { C_INCR } else { -C_INCR };
    }
    for start in 0..3 {
        if i > start && !in_lexicon(&lower[i - start - 1]) {
            let mut s = scalar(&words[i - start - 1], valence, cap_diff);
            if start == 1 {
                s *= 0.95;
            } else if start == 2 {
                s *= 0.9;
            }
            valence += s;
            valence = negation(valence, lower, start, i);
            if start == 2 {
                valence = idioms(valence, lower, i);
            }
        }
    }
    least(valence, lower, i, &in_lexicon)
}

fn scalar(word: &str, valence: f64, cap_diff: bool) -> f64 {
    let Some(mut s) = booster(&word.to_lowercase()) else {
        return 0.0;
    };
    if valence < 0.0 {
        s = -s;
    }
    if is_upper(word) && cap_diff {
        s += if valence > 0.0 { C_INCR } else { -C_INCR };
    }
    s
}

fn negation(valence: f64, lower: &[String], start: usize, i: usize) -> f64 {
    let so_or_this = |w: &str| w == "so" || w == "this";
    match start {
        0 if negated(&lower[i - 1]) => valence * N_SCALAR,
        1 if lower[i - 2] == "never" && so_or_this(&lower[i - 1]) => valence * 1.25,
        1 if lower[i - 2] == "without" && lower[i - 1] == "doubt" => valence,
        1 if negated(&lower[i - 2]) => valence * N_SCALAR,
        2 if (lower[i - 3] == "never" && so_or_this(&lower[i - 2]))
            || so_or_this(&lower[i - 1]) =>
        {
            valence * 1.25
        }
        2 if lower[i - 3] == "without" && (lower[i - 2] == "doubt" || lower[i - 1] == "doubt") => {
            valence
        }
        2 if negated(&lower[i - 3]) => valence * N_SCALAR,
        _ => valence,
    }
}

/// Fixed phrases around word `i` (which has at least three words before it).
fn idioms(mut valence: f64, lower: &[String], i: usize) -> f64 {
    let join = |range: std::ops::Range<usize>| lower[range].join(" ");
    let one_zero = join(i - 1..i + 1);
    let two_one_zero = join(i - 2..i + 1);
    let two_one = join(i - 2..i);
    let three_two_one = join(i - 3..i);
    let three_two = join(i - 3..i - 1);
    if let Some(v) = [
        &one_zero,
        &two_one_zero,
        &two_one,
        &three_two_one,
        &three_two,
    ]
    .into_iter()
    .find_map(|p| special_case(p))
    {
        valence = v;
    }
    if lower.len() > i + 1
        && let Some(v) = special_case(&join(i..i + 2))
    {
        valence = v;
    }
    if lower.len() > i + 2
        && let Some(v) = special_case(&join(i..i + 3))
    {
        valence = v;
    }
    for phrase in [&three_two_one, &three_two, &two_one] {
        if let Some(b) = booster(phrase) {
            valence += b;
        }
    }
    valence
}

fn least(valence: f64, lower: &[String], i: usize, in_lexicon: &impl Fn(&str) -> bool) -> f64 {
    // "least good" flips; "at least good" and "very least good" do not.
    let flips = i > 0
        && lower[i - 1] == "least"
        && !in_lexicon(&lower[i - 1])
        && (i == 1 || (lower[i - 2] != "at" && lower[i - 2] != "very"));
    if flips { valence * N_SCALAR } else { valence }
}

fn score(sentiments: &[f64], text: &str) -> f64 {
    let mut sum: f64 = sentiments.iter().sum();
    let exclamations = text.matches('!').count().min(4) as f64 * 0.292;
    let questions = match text.matches('?').count() {
        0 | 1 => 0.0,
        n @ 2..=3 => n as f64 * 0.18,
        _ => 0.96,
    };
    let emphasis = exclamations + questions;
    if sum > 0.0 {
        sum += emphasis;
    } else if sum < 0.0 {
        sum -= emphasis;
    }
    (sum / (sum * sum + 15.0).sqrt()).clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compound scores from the reference Python implementation
    /// (vaderSentiment 3.3.2, emoji translation disabled), rounded to 4 places.
    const REFERENCE: &[(&str, f64)] = &[
        ("VADER is smart, handsome, and funny.", 0.8316),
        ("VADER is smart, handsome, and funny!", 0.8439),
        ("VADER is very smart, handsome, and funny.", 0.8545),
        ("VADER is VERY SMART, handsome, and FUNNY.", 0.9227),
        ("VADER is VERY SMART, handsome, and FUNNY!!!", 0.9342),
        (
            "VADER is VERY SMART, uber handsome, and FRIGGIN FUNNY!!!",
            0.9469,
        ),
        ("VADER is not smart, handsome, nor funny.", -0.7424),
        ("The book was good.", 0.4404),
        ("At least it isn't a horrible book.", 0.431),
        ("The book was only kind of good.", 0.3832),
        ("Today SUX!", -0.5461),
        ("Today only kinda sux! But I'll get by, lol", 0.5249),
        ("Make sure you :) or :D today!", 0.8633),
        ("Not bad at all", 0.431),
        (
            "The product stopped working and support never answered.",
            0.2023,
        ),
        ("I am never so happy as on a Friday", 0.7702),
        ("There were no problems with the delivery", 0.3089),
        ("This is the worst service I have ever had.", -0.6249),
        ("The food was great but the service was terrible", -0.3818),
        ("I can't believe how good this is", -0.3412),
        ("Without doubt the best day", 0.7438),
        ("He is the bomb", 0.6124),
        ("It was kind of okay", 0.1548),
        ("At very least it works", 0.0),
        ("", 0.0),
    ];

    #[test]
    fn compound_scores_match_the_reference_implementation() {
        for (text, expected) in REFERENCE {
            let got = compound(text);
            assert!(
                (got - expected).abs() < 5e-4,
                "{text:?}: got {got:.4}, reference {expected}"
            );
        }
    }

    #[test]
    fn lexicon_loads_every_entry() {
        // The upstream file repeats 11 words; the last entry wins, as upstream.
        assert_eq!(lexicon().len(), 7506);
        assert_eq!(lexicon().get("good"), Some(&1.9));
    }

    #[test]
    fn but_shifts_weight_to_the_clause_after_it() {
        assert!(compound("The food was great but the service was terrible") < 0.0);
        assert!(compound("The service was terrible but the food was great") > 0.0);
    }
}
