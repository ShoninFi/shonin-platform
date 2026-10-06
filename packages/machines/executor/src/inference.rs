//! The inference host: a bundle's models, run with ONNX Runtime on the CPU,
//! next to the agent.
//!
//! - **Loading.** Every model in the bundle's `models/` directory is loaded
//!   (and so checked) before the agent starts: a model that won't load stops
//!   the start, not a trade. A model is named by its file name without
//!   `.onnx`.
//! - **Operators.** No custom operator library is ever registered, so a model
//!   can use only the runtime's own operators: a custom operator would be
//!   arbitrary native code.
//! - **Threads.** One thread per call, so a call's latency is predictable and
//!   one agent can't take every core.
//! - **Budgets.** Each call may run for at most `per_call`, and the agent for
//!   `per_minute` of inference in any minute (its `inference_ms_per_min`
//!   allowance). A call past either is `quota-exceeded`: the agent's error to
//!   handle, never a fault.
//! - **Watchdog.** A separate thread terminates a call that overruns its
//!   budget, so the budget holds even if a model hangs.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ort::session::{RunOptions, Session};
use ort::value::{DynValue, Tensor, ValueType};

/// A tensor as the agent sees it: a shape, row-major data of one type.
#[derive(Clone, Debug, PartialEq)]
pub enum Data {
    F32(Vec<f32>),
    F64(Vec<f64>),
    I32(Vec<i32>),
    I64(Vec<i64>),
    U8(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Named {
    pub name: String,
    pub shape: Vec<u32>,
    pub data: Data,
}

/// Why a call returned nothing.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    UnknownModel(String),
    InvalidInput(String),
    QuotaExceeded(String),
    Backend(String),
}

/// What a vault's agent may spend on inference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub per_call: Duration,
    pub per_minute: Duration,
}

impl Budget {
    /// From the agent's `inference_ms_per_min` allowance: a call may take at
    /// most the whole minute's allowance.
    pub fn from_allowance(ms_per_min: u32) -> Self {
        let per_minute = Duration::from_millis(u64::from(ms_per_min));
        Self {
            per_call: per_minute,
            per_minute,
        }
    }
}

/// The time used in the current minute.
#[derive(Debug)]
struct Spent {
    since: Instant,
    used: Duration,
}

/// A bundle's models, and what its agent has spent on them.
pub struct Inference {
    models: BTreeMap<String, Arc<Mutex<Session>>>,
    budget: Budget,
    spent: Mutex<Spent>,
}

impl Inference {
    /// Load every `.onnx` file in `dir`. No directory is no models.
    pub fn load(dir: &Path, budget: Budget) -> Result<Self> {
        let mut models = BTreeMap::new();
        if dir.is_dir() {
            let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<std::io::Result<_>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("onnx") {
                    continue;
                }
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .context("a model's name")?
                    .to_string();
                let bytes =
                    std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
                models.insert(
                    name.clone(),
                    Arc::new(Mutex::new(
                        session(&bytes).with_context(|| format!("loading model {name}"))?,
                    )),
                );
            }
        }
        Ok(Self::with(models, budget))
    }

    /// From models already in memory, by name.
    #[cfg(test)]
    pub fn from_bytes(models: &[(&str, &[u8])], budget: Budget) -> Result<Self> {
        let mut loaded = BTreeMap::new();
        for (name, bytes) in models {
            loaded.insert(
                name.to_string(),
                Arc::new(Mutex::new(
                    session(bytes).with_context(|| format!("loading model {name}"))?,
                )),
            );
        }
        Ok(Self::with(loaded, budget))
    }

    fn with(models: BTreeMap<String, Arc<Mutex<Session>>>, budget: Budget) -> Self {
        Self {
            models,
            budget,
            spent: Mutex::new(Spent {
                since: Instant::now(),
                used: Duration::ZERO,
            }),
        }
    }

    #[cfg(test)]
    pub fn names(&self) -> Vec<String> {
        self.models.keys().cloned().collect()
    }

    /// What is left of this minute's allowance, refusing when nothing is.
    fn allowed(&self) -> Result<Duration, Refusal> {
        let mut spent = self
            .spent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if spent.since.elapsed() >= Duration::from_secs(60) {
            spent.since = Instant::now();
            spent.used = Duration::ZERO;
        }
        let left = self.budget.per_minute.saturating_sub(spent.used);
        if left.is_zero() {
            return Err(Refusal::QuotaExceeded(
                "this minute's inference allowance is spent".into(),
            ));
        }
        Ok(left.min(self.budget.per_call))
    }

    fn charge(&self, took: Duration) {
        let mut spent = self
            .spent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        spent.used += took;
    }

    /// Run `model` on `inputs`, within the budget.
    pub async fn infer(&self, model: &str, inputs: Vec<Named>) -> Result<Vec<Named>, Refusal> {
        let session = self
            .models
            .get(model)
            .cloned()
            .ok_or_else(|| Refusal::UnknownModel(model.to_string()))?;
        let limit = self.allowed()?;
        let options = Arc::new(RunOptions::new().map_err(backend)?);
        // The watchdog: once the budget has passed, the call is terminated
        // wherever it is.
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let watched = options.clone();
        std::thread::spawn(move || {
            if done_rx.recv_timeout(limit).is_err() {
                let _ = watched.terminate();
            }
        });
        let began = Instant::now();
        let outcome = tokio::task::spawn_blocking(move || run(&session, inputs, &options))
            .await
            .map_err(|error| Refusal::Backend(error.to_string()))?;
        let _ = done_tx.send(());
        let took = began.elapsed();
        self.charge(took);
        match outcome {
            Err(_) if took >= limit => Err(Refusal::QuotaExceeded(format!(
                "the call ran past its budget of {} ms",
                limit.as_millis()
            ))),
            other => other,
        }
    }
}

fn backend(error: impl std::fmt::Display) -> Refusal {
    Refusal::Backend(error.to_string())
}

/// A session on one thread, from the model's bytes.
fn session(bytes: &[u8]) -> Result<Session> {
    let failed = |error: &dyn std::fmt::Display| anyhow::anyhow!("{error}");
    Session::builder()
        .map_err(|e| failed(&e))?
        .with_intra_threads(1)
        .map_err(|e| failed(&e))?
        .with_inter_threads(1)
        .map_err(|e| failed(&e))?
        .commit_from_memory(bytes)
        .map_err(|e| failed(&e))
}

fn run(
    session: &Mutex<Session>,
    inputs: Vec<Named>,
    options: &RunOptions,
) -> Result<Vec<Named>, Refusal> {
    let mut values: Vec<(String, DynValue)> = Vec::with_capacity(inputs.len());
    for input in inputs {
        let shape: Vec<i64> = input.shape.iter().map(|&d| i64::from(d)).collect();
        let count: usize = input.shape.iter().map(|&d| d as usize).product();
        let length = match &input.data {
            Data::F32(v) => v.len(),
            Data::F64(v) => v.len(),
            Data::I32(v) => v.len(),
            Data::I64(v) => v.len(),
            Data::U8(v) => v.len(),
        };
        if count != length {
            return Err(Refusal::InvalidInput(format!(
                "{} has shape {:?} but {length} values",
                input.name, input.shape
            )));
        }
        let invalid = |error: ort::Error| Refusal::InvalidInput(format!("{}: {error}", input.name));
        let value: DynValue = match input.data {
            Data::F32(v) => Tensor::from_array((shape, v)).map_err(invalid)?.into_dyn(),
            Data::F64(v) => Tensor::from_array((shape, v)).map_err(invalid)?.into_dyn(),
            Data::I32(v) => Tensor::from_array((shape, v)).map_err(invalid)?.into_dyn(),
            Data::I64(v) => Tensor::from_array((shape, v)).map_err(invalid)?.into_dyn(),
            Data::U8(v) => Tensor::from_array((shape, v)).map_err(invalid)?.into_dyn(),
        };
        values.push((input.name, value));
    }
    let mut session = session
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let outputs = session
        .run_with_options(values, options)
        .map_err(|error| Refusal::InvalidInput(error.to_string()))?;
    let mut named = Vec::new();
    for (name, value) in outputs.iter() {
        let ValueType::Tensor { ty, .. } = value.dtype() else {
            return Err(Refusal::Backend(format!("output {name} is not a tensor")));
        };
        use ort::value::TensorElementType as T;
        let (shape, data) = match ty {
            T::Float32 => {
                let (shape, data) = value.try_extract_tensor::<f32>().map_err(backend)?;
                (shape.to_vec(), Data::F32(data.to_vec()))
            }
            T::Float64 => {
                let (shape, data) = value.try_extract_tensor::<f64>().map_err(backend)?;
                (shape.to_vec(), Data::F64(data.to_vec()))
            }
            T::Int32 => {
                let (shape, data) = value.try_extract_tensor::<i32>().map_err(backend)?;
                (shape.to_vec(), Data::I32(data.to_vec()))
            }
            T::Int64 => {
                let (shape, data) = value.try_extract_tensor::<i64>().map_err(backend)?;
                (shape.to_vec(), Data::I64(data.to_vec()))
            }
            T::Uint8 => {
                let (shape, data) = value.try_extract_tensor::<u8>().map_err(backend)?;
                (shape.to_vec(), Data::U8(data.to_vec()))
            }
            other => {
                return Err(Refusal::Backend(format!(
                    "output {name} has type {other:?}, which agents can't read"
                )));
            }
        };
        named.push(Named {
            name: name.to_string(),
            shape: shape.iter().map(|&d| d.max(0) as u32).collect(),
            data,
        });
    }
    Ok(named)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// score = sigmoid(features · w + b), features of shape [1, 8]: the
    /// placeholder scorer `catalyst` carries.
    const PLACEHOLDER: &[u8] = include_bytes!("../tests/fixtures/placeholder-scorer.onnx");

    fn budget(ms: u64) -> Budget {
        Budget {
            per_call: Duration::from_millis(ms),
            per_minute: Duration::from_millis(ms),
        }
    }

    fn features(values: Vec<f32>) -> Vec<Named> {
        vec![Named {
            name: "features".into(),
            shape: vec![1, 8],
            data: Data::F32(values),
        }]
    }

    #[tokio::test]
    async fn a_bundled_model_scores_its_input() {
        let host = Inference::from_bytes(&[("scorer", PLACEHOLDER)], budget(5_000)).unwrap();
        let out = host.infer("scorer", features(vec![0.0; 8])).await.unwrap();
        assert_eq!(out[0].name, "score");
        assert_eq!(out[0].shape, vec![1, 1]);
        let Data::F32(score) = &out[0].data else {
            panic!()
        };
        // sigmoid(-0.1)
        assert!((score[0] - 0.475_02).abs() < 1e-4, "{score:?}");
    }

    #[tokio::test]
    async fn an_unknown_model_or_a_misshapen_input_is_the_agents_error() {
        let host = Inference::from_bytes(&[("scorer", PLACEHOLDER)], budget(5_000)).unwrap();
        assert!(matches!(
            host.infer("other", features(vec![0.0; 8])).await,
            Err(Refusal::UnknownModel(_))
        ));
        assert!(matches!(
            host.infer("scorer", features(vec![0.0; 7])).await,
            Err(Refusal::InvalidInput(_))
        ));
    }

    #[tokio::test]
    async fn past_the_minutes_allowance_a_call_is_refused_as_over_quota() {
        let host = Inference::from_bytes(&[("scorer", PLACEHOLDER)], budget(5_000)).unwrap();
        host.charge(Duration::from_millis(5_000));
        assert!(matches!(
            host.infer("scorer", features(vec![0.0; 8])).await,
            Err(Refusal::QuotaExceeded(_))
        ));
    }

    #[test]
    fn a_model_that_will_not_load_stops_the_load() {
        assert!(Inference::from_bytes(&[("broken", b"not a model")], budget(1_000)).is_err());
    }

    #[test]
    fn loading_reads_every_onnx_file_in_the_bundles_models_directory() {
        let dir = std::env::temp_dir().join(format!("shonin-models-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("scorer.onnx"), PLACEHOLDER).unwrap();
        std::fs::write(dir.join("notes.txt"), b"ignored").unwrap();
        let host = Inference::load(&dir, budget(1_000)).unwrap();
        assert_eq!(host.names(), vec!["scorer".to_string()]);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(
            Inference::load(&dir, budget(1_000))
                .unwrap()
                .names()
                .is_empty()
        );
    }
}
