//! Learned routing: a LinUCB contextual bandit over `tier:engine` arms.
//!
//! Ported from Threnody's `shared/bandit.py`. It runs in shadow mode by
//! default: every flight records what the bandit would have picked next to
//! what the heuristic picked, and the outcome of the arm that actually ran
//! trains it. Live mode acts on the bandit only once every candidate arm has
//! `min_updates` observations; an untrained arm scores on its exploration
//! bonus alone, so following it would be arbitrary, not learned.
//!
//! Unlike Threnody's version (which inverts only the diagonal of `A`), this
//! one inverts the full 13x13 matrix.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::router::{has_word, word_hits};

pub const FEATURE_DIM: usize = 13;

/// Each language is a set of alternative words.
const LANGS: [&[&str]; 7] = [
    &["python", "pytest", "flask", "django"],
    &["java", "javascript", "typescript", "node", "react"],
    &["rust", "cargo"],
    &["go", "golang"],
    &["c++", "cpp"],
    &["sql", "query", "database", "schema"],
    &["terraform", "kubernetes", "docker", "ci", "cd"],
];

const URGENCY: &[&str] = &[
    "critical",
    "urgent",
    "hotfix",
    "incident",
    "asap",
    "immediately",
    "production down",
];

const COMPLEXITY: &[&str] = &[
    "refactor",
    "rewrite",
    "architecture",
    "design",
    "migrate",
    "implement",
    "complex",
    "distributed",
    "reasoning",
    "consensus",
    "proof",
    "audit",
    "security",
    "concurrency",
];

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Distinct `\.[a-z]{1,5}\b` extensions in lower-cased text.
fn extensions(text: &str) -> usize {
    let mut seen = std::collections::BTreeSet::new();
    let b = text.as_bytes();
    for (i, _) in text.match_indices('.') {
        let mut j = i + 1;
        while j < b.len() && b[j].is_ascii_lowercase() && j - i <= 5 {
            j += 1;
        }
        let len = j - i - 1;
        let ends = text[j..]
            .chars()
            .next()
            .map(|c| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(true);
        if (1..=5).contains(&len) && ends {
            seen.insert(&text[i + 1..j]);
        }
    }
    seen.len()
}

/// The context a routing decision is made in.
///
/// 0 length, 1 urgency, 2 complexity word density, 3 multi-file,
/// 4 project bucket, 5 recent outcome mean, 6..13 languages.
pub fn features(task: &str, project: &str, recent: &[f64]) -> [f64; FEATURE_DIM] {
    let lower = task.to_lowercase();
    let mut f = [0.0; FEATURE_DIM];
    f[0] = (task.chars().count() as f64 / 500.0).min(1.0);
    f[1] = if URGENCY.iter().any(|w| has_word(&lower, w)) {
        1.0
    } else {
        0.0
    };
    let complexity: usize = COMPLEXITY.iter().map(|w| word_hits(&lower, w)).sum();
    f[2] = (complexity as f64 / 5.0).min(1.0);
    f[3] = (extensions(&lower) as f64 / 3.0).min(1.0);
    f[4] = if project.is_empty() {
        0.0
    } else {
        (fnv(project) % 8) as f64 / 7.0
    };
    let tail = &recent[recent.len().saturating_sub(10)..];
    if !tail.is_empty() {
        f[5] = (tail.iter().sum::<f64>() / tail.len() as f64).clamp(-1.0, 1.0);
    }
    for (i, words) in LANGS.iter().enumerate() {
        if words.iter().any(|w| has_word(&lower, w)) {
            f[6 + i] = 1.0;
        }
    }
    f
}

fn identity() -> Vec<Vec<f64>> {
    (0..FEATURE_DIM)
        .map(|i| {
            (0..FEATURE_DIM)
                .map(|j| if i == j { 1.0 } else { 0.0 })
                .collect()
        })
        .collect()
}

/// Gauss-Jordan inverse; `A = I + Σ x xᵀ` is positive definite, so it exists.
fn inverse(m: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let n = m.len();
    let mut a: Vec<Vec<f64>> = m.to_vec();
    let mut inv = identity();
    for c in 0..n {
        let p = (c..n)
            .max_by(|&x, &y| a[x][c].abs().total_cmp(&a[y][c].abs()))
            .unwrap_or(c);
        if a[p][c].abs() < 1e-12 {
            continue;
        }
        a.swap(c, p);
        inv.swap(c, p);
        let d = a[c][c];
        for j in 0..n {
            a[c][j] /= d;
            inv[c][j] /= d;
        }
        for r in 0..n {
            if r != c {
                let k = a[r][c];
                if k != 0.0 {
                    for j in 0..n {
                        a[r][j] -= k * a[c][j];
                        inv[r][j] -= k * inv[c][j];
                    }
                }
            }
        }
    }
    inv
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Arm {
    pub a: Vec<Vec<f64>>,
    pub b: Vec<f64>,
    pub n: u32,
}

impl Default for Arm {
    fn default() -> Self {
        Self {
            a: identity(),
            b: vec![0.0; FEATURE_DIM],
            n: 0,
        }
    }
}

impl Arm {
    fn valid(&self) -> bool {
        self.a.len() == FEATURE_DIM
            && self.a.iter().all(|r| r.len() == FEATURE_DIM)
            && self.b.len() == FEATURE_DIM
    }

    pub fn update(&mut self, x: &[f64; FEATURE_DIM], reward: f64) {
        for i in 0..FEATURE_DIM {
            for j in 0..FEATURE_DIM {
                self.a[i][j] += x[i] * x[j];
            }
            self.b[i] += reward * x[i];
        }
        self.n += 1;
    }

    pub fn ucb(&self, x: &[f64; FEATURE_DIM], alpha: f64) -> f64 {
        let inv = inverse(&self.a);
        let dot = |r: &Vec<f64>, v: &[f64]| r.iter().zip(v).map(|(p, q)| p * q).sum::<f64>();
        let theta: Vec<f64> = inv.iter().map(|r| dot(r, &self.b)).collect();
        let ax: Vec<f64> = inv.iter().map(|r| dot(r, x)).collect();
        let mean: f64 = theta.iter().zip(x).map(|(t, v)| t * v).sum();
        let var: f64 = x.iter().zip(&ax).map(|(p, q)| p * q).sum();
        mean + alpha * var.max(0.0).sqrt()
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Off,
    #[default]
    Shadow,
    Live,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    /// What the bandit scores highest.
    pub bandit: String,
    pub score: f64,
    /// What actually runs.
    pub chosen: String,
    /// `shadow`, `untrained` or `bandit`.
    pub why: &'static str,
}

/// All arms, persisted with Tower's state.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Bandit {
    #[serde(default)]
    pub arms: BTreeMap<String, Arm>,
    /// Rewards in the order they arrived (feature 5).
    #[serde(default)]
    pub recent: Vec<f64>,
}

impl Bandit {
    pub fn select(
        &self,
        x: &[f64; FEATURE_DIM],
        candidates: &[String],
        heuristic: &str,
        mode: Mode,
        alpha: f64,
        min_updates: u32,
    ) -> Pick {
        let fresh = Arm::default();
        let mut best = (heuristic.to_string(), f64::NEG_INFINITY);
        let mut trained = !candidates.is_empty();
        for arm in candidates {
            let model = self.arms.get(arm).filter(|a| a.valid()).unwrap_or(&fresh);
            if model.n < min_updates {
                trained = false;
            }
            let s = model.ucb(x, alpha);
            if s > best.1 {
                best = (arm.clone(), s);
            }
        }
        let why = match (mode, trained) {
            (Mode::Live, true) => "bandit",
            (Mode::Live, false) => "untrained",
            _ => "shadow",
        };
        Pick {
            chosen: if why == "bandit" {
                best.0.clone()
            } else {
                heuristic.to_string()
            },
            bandit: best.0,
            score: if best.1.is_finite() { best.1 } else { 0.0 },
            why,
        }
    }

    pub fn update(&mut self, arm: &str, x: &[f64; FEATURE_DIM], reward: f64) {
        let a = self.arms.entry(arm.to_string()).or_default();
        if !a.valid() {
            *a = Arm::default();
        }
        a.update(x, reward);
        self.recent.push(reward);
        let extra = self.recent.len().saturating_sub(50);
        self.recent.drain(..extra);
    }
}

/// How a flight went, as a reward in -1..1.
///
/// Landing is worth 1; each failed gate run and each escalation takes some of
/// that away, and bigger tiers pay a little for their cost, so a cheap arm
/// that lands as cleanly wins. A flight given up on scores -1.
pub fn reward(landed: bool, gate_failures: u32, escalations: u32, tier_rank: usize) -> f64 {
    if !landed {
        return -1.0;
    }
    let cost = [0.0, 0.1, 0.25][tier_rank.min(2)];
    (1.0 - 0.2 * gate_failures as f64 - 0.4 * escalations as f64 - cost).clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_have_threnodys_shape() {
        let f = features(
            "URGENT: refactor the rust parser in a.rs and b.toml",
            "/p",
            &[1.0, -1.0, 1.0],
        );
        assert_eq!(f.len(), FEATURE_DIM);
        assert_eq!(f[1], 1.0);
        assert!(f[2] > 0.0);
        assert!((f[3] - 2.0 / 3.0).abs() < 1e-9);
        assert!((f[5] - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(f[6 + 2], 1.0); // rust
        assert_eq!(f[6], 0.0);
        assert_eq!(features("x", "", &[])[4], 0.0);
    }

    #[test]
    fn inverse_is_an_inverse() {
        let mut arm = Arm::default();
        arm.update(&features("refactor rust code", "p", &[]), 1.0);
        arm.update(&features("fix python test", "q", &[]), 0.5);
        let inv = inverse(&arm.a);
        for i in 0..FEATURE_DIM {
            for j in 0..FEATURE_DIM {
                let v: f64 = (0..FEATURE_DIM).map(|k| arm.a[i][k] * inv[k][j]).sum();
                assert!((v - if i == j { 1.0 } else { 0.0 }).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn shadow_never_acts_and_live_waits_for_training() {
        let arms = vec!["low:qwen".to_string(), "high:opus".to_string()];
        let x = features("rename a variable in a.py", "p", &[]);
        let mut b = Bandit::default();
        for _ in 0..5 {
            b.update("low:qwen", &x, 1.0);
            b.update("high:opus", &x, -1.0);
        }
        let shadow = b.select(&x, &arms, "high:opus", Mode::Shadow, 1.0, 5);
        assert_eq!(shadow.bandit, "low:qwen");
        assert_eq!(
            (shadow.chosen.as_str(), shadow.why),
            ("high:opus", "shadow")
        );
        let early = b.select(&x, &arms, "high:opus", Mode::Live, 1.0, 50);
        assert_eq!(
            (early.chosen.as_str(), early.why),
            ("high:opus", "untrained")
        );
        let live = b.select(&x, &arms, "high:opus", Mode::Live, 1.0, 5);
        assert_eq!((live.chosen.as_str(), live.why), ("low:qwen", "bandit"));
        assert!(b.select(&x, &[], "h", Mode::Live, 1.0, 0).chosen == "h");
    }

    #[test]
    fn rewards() {
        assert_eq!(reward(false, 0, 0, 0), -1.0);
        assert_eq!(reward(true, 0, 0, 0), 1.0);
        assert!(reward(true, 0, 0, 2) < reward(true, 0, 0, 0));
        assert!(reward(true, 2, 1, 1) < reward(true, 1, 0, 1));
    }

    #[test]
    fn bad_persisted_arms_are_replaced() {
        let mut b = Bandit::default();
        b.arms.insert(
            "low:x".into(),
            Arm {
                a: vec![],
                b: vec![],
                n: 7,
            },
        );
        let x = features("x", "", &[]);
        b.update("low:x", &x, 1.0);
        assert_eq!(b.arms["low:x"].n, 1);
    }
}
