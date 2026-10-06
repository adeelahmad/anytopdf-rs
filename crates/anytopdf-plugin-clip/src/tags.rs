//! Zero-shot scene tags: compare an image embedding with text prompts.

use crate::onnx::cosine;

/// CLIP's learned logit scale (100) turns cosine similarity into softmax logits.
const LOGIT_SCALE: f32 = 100.0;

/// One searchable label and the prompt CLIP compares the image with.
#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    pub label: String,
    pub prompt: String,
}

/// Labels that compete with each other: the image gets at most one per group.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub name: String,
    pub tags: Vec<Tag>,
    /// Only scored when the `kind` group picked `photo` (an "outdoors"
    /// screenshot is noise).
    pub photos_only: bool,
}

fn group(name: &str, photos_only: bool, tags: &[(&str, &str)]) -> Group {
    Group {
        name: name.into(),
        photos_only,
        tags: tags
            .iter()
            .map(|(label, prompt)| Tag {
                label: (*label).into(),
                prompt: (*prompt).into(),
            })
            .collect(),
    }
}

/// Neutral content and setting labels. Nothing here describes a person's
/// traits; faces and identities belong to the face plugins.
pub fn default_groups() -> Vec<Group> {
    vec![
        group(
            "kind",
            false,
            &[
                ("photo", "a photo"),
                ("document", "a scanned page of a text document"),
                ("screenshot", "a screenshot of a computer screen"),
                ("slide", "a presentation slide"),
                ("chart", "a chart or diagram"),
                ("whiteboard", "handwriting on a whiteboard"),
                ("illustration", "a drawing or illustration"),
                ("map", "a map"),
            ],
        ),
        group(
            "setting",
            true,
            &[
                ("indoors", "a photo taken indoors"),
                ("outdoors", "a photo taken outdoors"),
            ],
        ),
        group(
            "scene",
            true,
            &[
                ("landscape", "a photo of a natural landscape"),
                ("beach", "a photo of a beach"),
                ("mountains", "a photo of mountains"),
                ("forest", "a photo of a forest"),
                ("snow", "a photo of snow"),
                ("city street", "a photo of a city street"),
                ("building", "a photo of a building"),
                ("office", "a photo of an office"),
                ("meeting", "a photo of people in a meeting"),
                ("classroom", "a photo of a classroom"),
                ("kitchen", "a photo of a kitchen"),
                ("restaurant", "a photo of a restaurant"),
                ("living room", "a photo of a living room"),
                ("stage", "a photo of a concert or stage"),
                ("sports", "a photo of people playing sports"),
                ("vehicle", "a photo of a car or vehicle"),
                ("food", "a photo of food"),
                ("animal", "a photo of an animal"),
                ("crowd", "a photo of a crowd of people"),
                ("portrait", "a portrait photo of a person"),
                ("night", "a photo taken at night"),
            ],
        ),
    ]
}

/// Parses `ANYTOPDF_CLIP_TAGS`: `off` disables tagging, otherwise
/// `label=prompt` entries separated by `;` form one custom group (a bare
/// label uses the prompt `a photo of <label>`).
pub fn parse_groups(spec: Option<&str>) -> Vec<Group> {
    let Some(spec) = spec.map(str::trim).filter(|s| !s.is_empty()) else {
        return default_groups();
    };
    if spec.eq_ignore_ascii_case("off") {
        return Vec::new();
    }
    let tags: Vec<Tag> = spec
        .split(';')
        .filter_map(|entry| {
            let (label, prompt) = match entry.split_once('=') {
                Some((label, prompt)) => (label.trim(), prompt.trim().to_string()),
                None => (entry.trim(), format!("a photo of {}", entry.trim())),
            };
            (!label.is_empty() && !prompt.is_empty()).then(|| Tag {
                label: label.into(),
                prompt,
            })
        })
        .collect();
    if tags.is_empty() {
        return Vec::new();
    }
    vec![Group {
        name: "custom".into(),
        tags,
        photos_only: false,
    }]
}

/// Softmax over scaled cosine similarities.
pub fn probabilities(image: &[f32], prompts: &[Vec<f32>]) -> Vec<f32> {
    let logits: Vec<f32> = prompts
        .iter()
        .map(|p| cosine(image, p) * LOGIT_SCALE)
        .collect();
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let exp: Vec<f32> = logits.iter().map(|l| (l - max).exp()).collect();
    let sum: f32 = exp.iter().sum();
    exp.iter().map(|e| e / sum).collect()
}

/// A chosen tag: its group, label and probability within the group.
#[derive(Debug, Clone, PartialEq)]
pub struct Chosen {
    pub group: String,
    pub label: String,
    pub prompt: String,
    pub probability: f32,
}

/// The best tag of each group whose probability reaches `threshold`, or 1.5
/// times a uniform guess when that is higher, so a two-way group such as
/// indoors/outdoors needs a clear 75% win.
/// `prompts[g][t]` is the embedding of `groups[g].tags[t].prompt`.
pub fn choose(
    groups: &[Group],
    prompts: &[Vec<Vec<f32>>],
    image: &[f32],
    threshold: f32,
) -> Vec<Chosen> {
    let mut chosen: Vec<Chosen> = Vec::new();
    for (group, vectors) in groups.iter().zip(prompts) {
        if group.photos_only && !chosen.iter().any(|c| c.label == "photo") {
            continue;
        }
        let probs = probabilities(image, vectors);
        let best = probs.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1));
        let needed = threshold.max((1.5 / vectors.len() as f32).min(0.95));
        if let Some((index, &probability)) = best
            && probability >= needed
        {
            let tag = &group.tags[index];
            chosen.push(Chosen {
                group: group.name.clone(),
                label: tag.label.clone(),
                prompt: tag.prompt.clone(),
                probability,
            });
        }
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_groups() -> (Vec<Group>, Vec<Vec<Vec<f32>>>) {
        let groups = vec![
            group("kind", false, &[("photo", "p"), ("document", "d")]),
            group("setting", true, &[("indoors", "i"), ("outdoors", "o")]),
        ];
        let prompts = vec![
            vec![vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]],
            vec![vec![1.0, 0.0, 1.0], vec![1.0, 0.0, -1.0]],
        ];
        (groups, prompts)
    }

    #[test]
    fn picks_the_best_label_of_each_group() {
        let (groups, prompts) = two_groups();
        let chosen = choose(&groups, &prompts, &[1.0, 0.0, 0.05], 0.5);
        let labels: Vec<_> = chosen.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["photo", "indoors"]);
        assert!(chosen[0].probability > 0.99);
    }

    #[test]
    fn photo_only_groups_are_skipped_for_documents() {
        let (groups, prompts) = two_groups();
        let chosen = choose(&groups, &prompts, &[0.0, 1.0, 0.2], 0.5);
        let labels: Vec<_> = chosen.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["document"]);
    }

    #[test]
    fn uncertain_groups_emit_nothing() {
        let (groups, prompts) = two_groups();
        // Equally close to both kinds: 50% each, below the threshold.
        assert!(choose(&groups, &prompts, &[1.0, 1.0, 0.0], 0.6).is_empty());
    }

    #[test]
    fn two_way_groups_need_a_clear_win() {
        let (groups, prompts) = two_groups();
        // About 67% indoors: above the 0.5 threshold, below the 75% two-way bar.
        let chosen = choose(&groups, &prompts, &[1.0, 0.0, 0.005], 0.5);
        let labels: Vec<_> = chosen.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["photo"]);
    }

    #[test]
    fn tag_spec_parses_custom_labels_and_off() {
        assert!(parse_groups(Some("off")).is_empty());
        assert_eq!(parse_groups(None), default_groups());
        let custom = parse_groups(Some("cat; logo = a company logo ;;"));
        assert_eq!(custom.len(), 1);
        assert_eq!(custom[0].tags[0].prompt, "a photo of cat");
        assert_eq!(custom[0].tags[1].label, "logo");
        assert_eq!(custom[0].tags[1].prompt, "a company logo");
    }

    #[test]
    fn default_labels_avoid_sensitive_traits() {
        let banned = [
            "gender", "male", "female", "man", "woman", "age", "emotion", "race",
        ];
        for group in default_groups() {
            for tag in group.tags {
                for word in tag.label.split(' ').chain(tag.prompt.split(' ')) {
                    assert!(!banned.contains(&word), "{word} in {}", tag.prompt);
                }
            }
        }
    }
}
