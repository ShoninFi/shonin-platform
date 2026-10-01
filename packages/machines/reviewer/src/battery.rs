//! The calibrated half of a review: a fixed battery of narrow questions, and
//! the arithmetic that turns their answers into six ratings.
//!
//! A model asked "how does this code read" gives an opinion. A model that
//! answers narrow questions, each with a probability, gives something a
//! reader can argue with one question at a time. The thresholds and the
//! arithmetic live here, in ordinary code with tests, so a rating is computed
//! rather than asserted: publish the battery and the rule, and anyone with the
//! same answers gets the same ratings.
//!
//! Nothing in this module calls anything. It is pure, so the part of a review
//! that depends on arithmetic can be checked without a network or a key.

use serde::{Deserialize, Serialize};
use shared::{Area, Rating};

/// The battery this image is pinned to. It is hashed into every review that
/// uses it, so a review says which questions produced it.
pub const BATTERY: &str = include_str!("../review/battery.json");

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Battery {
    pub version: String,
    #[serde(default)]
    pub note: Vec<String>,
    pub thresholds: Thresholds,
    pub questions: Vec<Question>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct Thresholds {
    pub some: f64,
    pub serious: f64,
    pub confident: f64,
    pub refuse: f64,
}

/// One question. `good` says which way it points: for most, a high probability
/// is a worse agent; for a few, a better one.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Question {
    pub id: String,
    pub area: Area,
    #[serde(default)]
    pub good: bool,
    /// A yes refuses the build: the code is malicious.
    #[serde(default)]
    pub refuses: bool,
    pub instructions: String,
}

/// What a model said about one question.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Answer {
    pub id: String,
    /// The model's probability that the answer is yes, from 0 to 1.
    pub probability: f64,
    /// How sure it is of that, where the model reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// The answers, which model gave them, and what they come to.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Rated {
    pub model: String,
    pub answers: Vec<Answer>,
    pub ratings: [Rating; 6],
    /// The code was found malicious, and is refused.
    pub refused: bool,
}

impl Battery {
    pub fn pinned() -> Self {
        serde_json::from_str(BATTERY).expect("the battery this image ships with parses")
    }

    /// The ratings those answers come to, and whether they refuse the build.
    ///
    /// An area's concern is its worst answer: the probability of a problem,
    /// or for a question about good practice, the probability it is missing.
    /// An answer the model did not give counts as nothing either way, and an
    /// area with no confident answer at all reads couldn't tell.
    pub fn rate(&self, answers: &[Answer]) -> ([Rating; 6], bool) {
        let t = self.thresholds;
        let mut ratings = [Rating::CouldntTell; 6];
        let mut refused = false;
        for area in Area::ALL {
            let mut worst: Option<f64> = None;
            for question in self.questions.iter().filter(|q| q.area == area) {
                let Some(answer) = answers.iter().find(|a| a.id == question.id) else {
                    continue;
                };
                let probability = answer.probability.clamp(0.0, 1.0);
                if question.refuses && probability >= t.refuse {
                    refused = true;
                }
                if answer
                    .confidence
                    .is_some_and(|confidence| confidence < t.confident)
                {
                    continue;
                }
                let concern = if question.good {
                    1.0 - probability
                } else {
                    probability
                };
                worst = Some(worst.map_or(concern, |w: f64| w.max(concern)));
            }
            ratings[area.index()] = match worst {
                None => Rating::CouldntTell,
                Some(concern) if concern >= t.serious => Rating::SeriousConcerns,
                Some(concern) if concern >= t.some => Rating::SomeConcerns,
                Some(_) => Rating::LooksFine,
            };
        }
        if refused {
            ratings[Area::Security.index()] = Rating::SeriousConcerns;
        }
        (ratings, refused)
    }

    /// The questions as the prompt lists them.
    pub fn as_prompt(&self) -> String {
        self.questions
            .iter()
            .map(|question| {
                format!(
                    "- `{}` ({}): {}",
                    question.id,
                    question.area.label(),
                    question.instructions
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(id: &str, probability: f64) -> Answer {
        Answer {
            id: id.to_string(),
            probability,
            confidence: None,
        }
    }

    fn all_clean() -> Vec<Answer> {
        Battery::pinned()
            .questions
            .iter()
            .map(|q| answer(&q.id, if q.good { 0.95 } else { 0.05 }))
            .collect()
    }

    #[test]
    fn every_area_has_questions_and_ids_are_unique() {
        let battery = Battery::pinned();
        for area in Area::ALL {
            assert!(
                battery.questions.iter().any(|q| q.area == area),
                "{area:?} has no question"
            );
        }
        let mut ids: Vec<&str> = battery.questions.iter().map(|q| q.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), battery.questions.len());
    }

    #[test]
    fn clean_answers_look_fine_everywhere() {
        let (ratings, refused) = Battery::pinned().rate(&all_clean());
        assert_eq!(ratings, [Rating::LooksFine; 6]);
        assert!(!refused);
    }

    #[test]
    fn the_worst_answer_sets_its_area_and_no_other() {
        let mut answers = all_clean();
        answers.retain(|a| a.id != "unbounded-risk");
        answers.push(answer("unbounded-risk", 0.9));
        answers.retain(|a| a.id != "restart-unsafe");
        answers.push(answer("restart-unsafe", 0.4));
        let (ratings, _) = Battery::pinned().rate(&answers);
        assert_eq!(
            ratings[Area::FinancialRisk.index()],
            Rating::SeriousConcerns
        );
        assert_eq!(ratings[Area::Reliability.index()], Rating::SomeConcerns);
        assert_eq!(ratings[Area::Security.index()], Rating::LooksFine);
    }

    #[test]
    fn a_claim_the_code_does_not_keep_counts_against_its_description() {
        let mut answers = all_clean();
        answers.retain(|a| a.id != "limits-enforced");
        answers.push(answer("limits-enforced", 0.1));
        let (ratings, _) = Battery::pinned().rate(&answers);
        assert_eq!(
            ratings[Area::AccurateDescription.index()],
            Rating::SeriousConcerns
        );
    }

    #[test]
    fn an_area_nobody_answered_confidently_reads_couldnt_tell() {
        let mut answers = all_clean();
        for answer in answers.iter_mut() {
            if answer.id == "unstable-build" {
                answer.confidence = Some(0.1);
            }
        }
        let (ratings, _) = Battery::pinned().rate(&answers);
        assert_eq!(ratings[Area::Reproducibility.index()], Rating::CouldntTell);
        let (nothing, refused) = Battery::pinned().rate(&[]);
        assert_eq!(nothing, [Rating::CouldntTell; 6]);
        assert!(!refused);
    }

    #[test]
    fn malicious_code_is_refused() {
        let mut answers = all_clean();
        answers.retain(|a| a.id != "malicious");
        answers.push(answer("malicious", 0.85));
        let (ratings, refused) = Battery::pinned().rate(&answers);
        assert!(refused);
        assert_eq!(ratings[Area::Security.index()], Rating::SeriousConcerns);
        // A suspicion short of the threshold is a concern, not a refusal.
        answers.retain(|a| a.id != "malicious");
        answers.push(answer("malicious", 0.75));
        assert!(!Battery::pinned().rate(&answers).1);
    }
}
