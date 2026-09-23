//! Asking the battery.
//!
//! One request carries the source and every question at once, and comes back
//! with a probability for each. The model is pinned by an exact version rather
//! than a moving alias, because a review commits which model answered it and
//! an alias that moved would make that claim false.
//!
//! The answers are the model's. The score is not: it is computed from them in
//! [`crate::battery`], with weights this repository publishes.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::battery::{Answer, Battery, Calibrated};

/// Where the questions go. Overridable so a test can point at a stub.
pub const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";

/// The exact version this image asks for. Never an alias: a review says which
/// model answered it, and an alias that moved would make that a lie.
pub const MODEL: &str = "jev-1.13.0";

/// How long to wait for the whole battery. The model answers in well under a
/// second when it is healthy; this is the point at which a review gives up and
/// goes out with prose alone.
const TIMEOUT: Duration = Duration::from_secs(30);

/// How many times a refusal to serve is retried before giving up.
const ATTEMPTS: u32 = 4;

/// The reviewer's half of the conversation.
#[derive(Serialize)]
struct Request<'a> {
    state: &'a str,
    model: &'a str,
    questions: BTreeMap<String, Question<'a>>,
}

#[derive(Serialize)]
struct Question<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    instructions: &'a str,
}

#[derive(Deserialize)]
struct Reply {
    /// The exact version that answered, which is what a review records. It can
    /// differ from what was asked for when an alias was used.
    model: String,
    answers: BTreeMap<String, Given>,
}

#[derive(Deserialize)]
struct Given {
    #[serde(default)]
    noul: Option<f64>,
    #[serde(default)]
    confidence: Option<f64>,
}

/// A client for the battery. Without a key there is nothing to talk to, and
/// [`Client::configured`] says so rather than failing later.
#[derive(Clone)]
pub struct Client {
    endpoint: String,
    key: String,
    model: String,
    http: reqwest::Client,
}

impl Client {
    /// A client, if this image was given a key for one.
    pub fn configured(
        key: Option<String>,
        endpoint: Option<String>,
        model: Option<String>,
    ) -> Option<Self> {
        let key = key?;
        if key.trim().is_empty() {
            return None;
        }
        Some(Self {
            endpoint: endpoint.unwrap_or_else(|| ENDPOINT.to_string()),
            key,
            model: model.unwrap_or_else(|| MODEL.to_string()),
            http: reqwest::Client::builder()
                .timeout(TIMEOUT)
                .build()
                .expect("an http client"),
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Put the whole battery to the model over one piece of source.
    pub async fn ask(&self, battery: &Battery, source: &str) -> Result<Calibrated> {
        let questions: BTreeMap<String, Question> = battery
            .questions
            .iter()
            .map(|question| {
                (
                    question.id.clone(),
                    Question {
                        // Every question is a yes or a no with a probability
                        // behind it. A rubric would put the judgement of how
                        // much a thing matters back into the model, and that
                        // judgement is the battery's weights.
                        kind: "noul",
                        instructions: &question.instructions,
                    },
                )
            })
            .collect();

        let request = Request {
            state: source,
            model: &self.model,
            questions,
        };

        let reply = self.send(&request).await?;

        let answers: Vec<Answer> = battery
            .questions
            .iter()
            .filter_map(|question| {
                let given = reply.answers.get(&question.id)?;
                Some(Answer {
                    id: question.id.clone(),
                    probability: given.noul?,
                    confidence: given.confidence,
                })
            })
            .collect();

        if answers.len() != battery.questions.len() {
            bail!(
                "the model answered {} of {} questions",
                answers.len(),
                battery.questions.len()
            );
        }

        let (score, working) = battery.score(&answers);
        Ok(Calibrated {
            score,
            model: reply.model,
            answers,
            working,
        })
    }

    /// Send it, waiting out a refusal to serve rather than failing the review.
    async fn send(&self, request: &Request<'_>) -> Result<Reply> {
        let mut wait = Duration::from_millis(400);
        let mut last = String::new();

        for attempt in 1..=ATTEMPTS {
            let sent = self
                .http
                .post(&self.endpoint)
                .bearer_auth(&self.key)
                .json(request)
                .send()
                .await;

            match sent {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        return response
                            .json::<Reply>()
                            .await
                            .context("the model's answers did not parse");
                    }
                    // Too many requests, or overloaded: the documented answer
                    // is to back off rather than to give up.
                    if status.as_u16() != 429 && status.as_u16() != 529 {
                        let body = response.text().await.unwrap_or_default();
                        bail!("the model answered {status}: {}", body.trim());
                    }
                    last = format!("the model answered {status}");
                }
                Err(error) => last = error.to_string(),
            }

            if attempt < ATTEMPTS {
                tokio::time::sleep(wait).await;
                wait *= 2;
            }
        }

        bail!("the model would not answer after {ATTEMPTS} attempts: {last}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_needs_a_key() {
        assert!(Client::configured(None, None, None).is_none());
        assert!(Client::configured(Some("  ".to_string()), None, None).is_none());
        assert!(Client::configured(Some("k".to_string()), None, None).is_some());
    }

    #[test]
    fn the_model_is_pinned_to_a_version_rather_than_an_alias() {
        assert!(
            !MODEL.contains("latest"),
            "a review records which model answered it, so the image pins a version"
        );
    }

    /// The request is the shape the endpoint documents: the source as `state`,
    /// the pinned model, and a map of named questions.
    #[test]
    fn a_request_carries_the_source_and_every_question() {
        let battery = Battery::pinned();
        let questions: BTreeMap<String, Question> = battery
            .questions
            .iter()
            .map(|q| {
                (
                    q.id.clone(),
                    Question {
                        kind: "noul",
                        instructions: &q.instructions,
                    },
                )
            })
            .collect();
        let body = serde_json::to_value(Request {
            state: "fn main() {}",
            model: MODEL,
            questions,
        })
        .expect("a request serialises");

        assert_eq!(body["state"], "fn main() {}");
        assert_eq!(body["model"], MODEL);
        assert_eq!(
            body["questions"]["unbounded-risk"]["type"], "noul",
            "every question is a yes or a no with a probability"
        );
        assert!(
            body["questions"]["unbounded-risk"]["instructions"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
        );
    }

    /// A reply is read by question id, and the version that actually answered
    /// is taken from the reply rather than assumed.
    #[test]
    fn a_reply_is_read_by_question() {
        let reply: Reply = serde_json::from_str(
            r#"{
                "model": "jev-1.13.0",
                "answers": {
                    "unbounded-risk": { "type": "noul", "noul": 0.93, "confidence": 0.8 },
                    "hidden-behaviour": { "type": "noul", "noul": 0.02 }
                },
                "usage": { "input_tokens": 296, "output_tokens": 20 }
            }"#,
        )
        .expect("a reply parses");

        assert_eq!(reply.model, "jev-1.13.0");
        assert_eq!(reply.answers["unbounded-risk"].noul, Some(0.93));
        assert_eq!(reply.answers["unbounded-risk"].confidence, Some(0.8));
        assert_eq!(reply.answers["hidden-behaviour"].confidence, None);
    }
}
