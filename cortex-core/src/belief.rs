use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::storage::traits::StorageBackend;
use crate::CortexError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Belief {
    pub id: Uuid,
    pub key: String,
    pub probability: f32,
    pub observations: Vec<Observation>,
    pub last_updated: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub timestamp: DateTime<Utc>,
    pub evidence: Evidence,
    pub prior: f32,
    pub posterior: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Evidence {
    Supports(f32),
    Contradicts(f32),
}

/// Single-step Bayesian update: compute posterior from prior + evidence.
/// P(H|E) = P(E|H)*P(H) / [P(E|H)*P(H) + P(E|~H)*P(~H)]
pub fn bayesian_update(prior: f32, evidence: &Evidence) -> f32 {
    let strength = match evidence {
        Evidence::Supports(s) | Evidence::Contradicts(s) => *s,
    };
    let likelihood_h = match evidence {
        Evidence::Supports(_) => 0.5 + strength * 0.5,
        Evidence::Contradicts(_) => 0.5 - strength * 0.5,
    };
    let likelihood_not_h = 1.0 - likelihood_h;
    let numerator = likelihood_h * prior;
    let denominator = numerator + likelihood_not_h * (1.0 - prior);
    if denominator > 0.0 {
        (numerator / denominator).clamp(0.001, 0.999)
    } else {
        prior
    }
}

/// Bayesian belief update system.
pub struct BeliefEngine<'a> {
    storage: &'a dyn StorageBackend,
}

impl<'a> BeliefEngine<'a> {
    pub fn new(storage: &'a dyn StorageBackend) -> Self {
        Self { storage }
    }

    /// Observe evidence for a belief key. Creates the belief if it doesn't exist.
    /// Uses Bayesian update: posterior = (likelihood * prior) / evidence
    pub fn observe(&self, key: &str, evidence: Evidence) -> Result<Belief, CortexError> {
        let mut belief = match self.storage.get_belief(key)? {
            Some(b) => b,
            None => {
                let b = Belief {
                    id: Uuid::new_v4(),
                    key: key.to_string(),
                    probability: 0.5, // uninformative prior
                    observations: Vec::new(),
                    last_updated: Utc::now(),
                };
                self.storage.store_belief(&b)?;
                b
            }
        };

        let prior = belief.probability;
        let posterior = bayesian_update(prior, &evidence);

        let observation = Observation {
            timestamp: Utc::now(),
            evidence,
            prior,
            posterior,
        };

        belief.probability = posterior;
        belief.observations.push(observation);
        belief.last_updated = Utc::now();

        self.storage.update_belief(&belief)?;
        Ok(belief)
    }

    /// Get current belief state for a key.
    pub fn get_belief(&self, key: &str) -> Result<Option<Belief>, CortexError> {
        self.storage.get_belief(key)
    }

    /// Get all beliefs above a confidence threshold.
    /// "Confident" means probability is either very high (>threshold) or very low (<1-threshold).
    pub fn get_confident_beliefs(&self, threshold: f32) -> Result<Vec<Belief>, CortexError> {
        self.storage.list_beliefs_confident(threshold)
    }
}
