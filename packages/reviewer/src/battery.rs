//! The calibrated half of a review: a fixed battery of narrow questions, and
//! the arithmetic that turns their answers into a score.
//!
//! A model that answers "how reasonable is this code, out of a hundred" is
//! giving an opinion about a number. A model that answers thirteen narrow
//! questions, each with a probability, is doing something a reader can argue
//! with one question at a time. The weights and the sum live here, in ordinary
//! code with tests, so the score is computed rather than asserted: publish the
//! battery and the formula, and anyone with the same answers gets the same
//! score.
//!
//! Nothing in this module calls anything. It is pure, so the part of a review
//! that depends on arithmetic can be checked without a network or a key.

use serde::{Deserialize, Serialize};

/// The battery this image is pinned to. It is hashed into every review that
/// uses it, so a review says which questions produced it.
pub const BATTERY: &str = include_str!("../review/battery.json");

/// What a clean strategy scores. Not a hundred: no reader sees everything, and
/// a full mark would claim otherwise.
const CEILING: f64 = 95.0;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Battery {
    pub version: String,
    #[serde(default)]
    pub note: Vec<String>,
    pub questions: Vec<Question>,
}

/// One question. `good` says which way it points: for most, a high probability
/// is a worse strategy; for a few, it is a better one.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Question {
    pub id: String,
    pub category: String,
    pub weight: f64,
    #[serde(default)]
    pub good: bool,
    pub instructions: String,
}

/// What the model said about one question.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Answer {
    pub id: String,
    /// The model's probability that the answer is yes, from 0 to 1.
    pub probability: f64,
    /// How sure it is of that, where the model reports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// A score, and the working that produced it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Calibrated {
    pub score: u8,
    /// Which model answered, exactly, as the model itself reported it.
    pub model: String,
    pub answers: Vec<Answer>,
    /// What each answer did to the score, in the order the battery asks them.
    pub working: Vec<Contribution>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Contribution {
    pub id: String,
    pub probability: f64,
    pub weight: f64,
    /// Negative takes the score down, positive puts it up.
    pub points: f64,
}

impl Battery {
    pub fn pinned() -> Self {
        serde_json::from_str(BATTERY).expect("the battery this image ships with parses")
    }

    /// The score those answers come to.
    ///
    /// Start at the ceiling. Every question that points at a problem takes off
    /// its weight times the probability that the problem is there. Every
    /// question that points at good practice puts back its weight times the
    /// probability that it holds, having first taken that weight off, so a
    /// strategy is not rewarded for a question nobody answered.
    ///
    /// An answer the model did not give counts as nothing either way rather
    /// than as an accusation.
    pub fn score(&self, answers: &[Answer]) -> (u8, Vec<Contribution>) {
        let mut score = CEILING;
        let mut working = Vec::with_capacity(self.questions.len());

        for question in &self.questions {
            let Some(answer) = answers.iter().find(|a| a.id == question.id) else {
                continue;
            };
            let probability = answer.probability.clamp(0.0, 1.0);
            let points = if question.good {
                question.weight * (probability - 1.0)
            } else {
                -question.weight * probability
            };
            score += points;
            working.push(Contribution {
                id: question.id.clone(),
                probability,
                weight: question.weight,
                points,
            });
        }

        (score.clamp(0.0, 100.0).round() as u8, working)
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

    /// Every question the battery asks has to be one the review can attribute,
    /// or a finding could point at a category the platform does not have.
    #[test]
    fn every_question_names_a_category_the_platform_knows() {
        // A category's slug is how it serialises, which is what a report
        // carries and what the schema's enum lists.
        let known: Vec<String> = shared::FindingCategory::all()
            .iter()
            .map(|category| {
                serde_json::to_value(category)
                    .expect("a category serialises")
                    .as_str()
                    .expect("as a string")
                    .to_string()
            })
            .collect();
        for question in Battery::pinned().questions {
            assert!(
                known.contains(&question.category),
                "{} names {}, which is not a finding category",
                question.id,
                question.category
            );
        }
    }

    #[test]
    fn the_battery_asks_each_question_once() {
        let battery = Battery::pinned();
        let mut ids: Vec<&str> = battery.questions.iter().map(|q| q.id.as_str()).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "a question is asked twice");
    }

    #[test]
    fn a_strategy_with_nothing_wrong_scores_below_a_hundred() {
        let battery = Battery::pinned();
        let answers: Vec<Answer> = battery
            .questions
            .iter()
            .map(|q| answer(&q.id, if q.good { 1.0 } else { 0.0 }))
            .collect();
        let (score, _) = battery.score(&answers);
        assert_eq!(score, CEILING as u8);
    }

    #[test]
    fn a_strategy_with_everything_wrong_scores_nothing() {
        let battery = Battery::pinned();
        let answers: Vec<Answer> = battery
            .questions
            .iter()
            .map(|q| answer(&q.id, if q.good { 0.0 } else { 1.0 }))
            .collect();
        let (score, _) = battery.score(&answers);
        assert_eq!(score, 0);
    }

    /// The weights have to be able to reach zero, or the worst strategy
    /// imaginable would still score respectably.
    #[test]
    fn the_weights_can_reach_the_floor() {
        let battery = Battery::pinned();
        let reachable: f64 = battery.questions.iter().map(|q| q.weight).sum();
        assert!(
            reachable >= CEILING,
            "the worst possible answers only take {reachable} off a ceiling of {CEILING}"
        );
    }

    /// One serious problem should be visible in the score without sinking it,
    /// because a review is a score and not a gate.
    #[test]
    fn one_serious_problem_costs_a_grade_rather_than_everything() {
        let battery = Battery::pinned();
        let mut answers: Vec<Answer> = battery
            .questions
            .iter()
            .map(|q| answer(&q.id, if q.good { 1.0 } else { 0.0 }))
            .collect();
        answers
            .iter_mut()
            .find(|a| a.id == "unbounded-risk")
            .expect("the battery asks about unbounded risk")
            .probability = 1.0;

        let (score, _) = battery.score(&answers);
        assert!(
            (60..=85).contains(&score),
            "one certain serious problem scored {score}"
        );
    }

    #[test]
    fn an_unanswered_question_counts_neither_way() {
        let battery = Battery::pinned();
        let (score, working) = battery.score(&[]);
        assert_eq!(score, CEILING as u8);
        assert!(working.is_empty());
    }

    #[test]
    fn the_working_adds_up_to_the_score() {
        let battery = Battery::pinned();
        let answers: Vec<Answer> = battery
            .questions
            .iter()
            .enumerate()
            .map(|(n, q)| answer(&q.id, (n as f64 % 4.0) / 4.0))
            .collect();
        let (score, working) = battery.score(&answers);
        let summed: f64 = CEILING + working.iter().map(|c| c.points).sum::<f64>();
        assert_eq!(score, summed.clamp(0.0, 100.0).round() as u8);
    }
}
