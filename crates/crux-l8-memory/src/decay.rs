//! Per-kind decay configuration loader + scoring helper.
//!
//! `decay_config` rows are seeded by migration 003. We re-read them at
//! engine init so user overrides via `INSERT OR REPLACE INTO decay_config`
//! take effect immediately without code changes.

use std::collections::HashMap;

use rusqlite::Connection;

use crux_core::error::Result;

use crate::types::ObservationKind;

#[derive(Debug, Clone, Copy)]
pub struct DecayParams {
    pub decay_rate: f64,
    pub min_score: f64,
    pub boost_on_access: f64,
}

impl DecayParams {
    fn default_for(kind: ObservationKind) -> Self {
        // Mirror of migration 003 defaults — used as a fallback if the
        // row is missing.
        match kind {
            ObservationKind::Guardrail => DecayParams {
                decay_rate: 1.0,
                min_score: 1.0,
                boost_on_access: 0.0,
            },
            ObservationKind::User => DecayParams {
                decay_rate: 1.0,
                min_score: 0.8,
                boost_on_access: 0.0,
            },
            ObservationKind::Convention => DecayParams {
                decay_rate: 1.0,
                min_score: 0.8,
                boost_on_access: 0.0,
            },
            ObservationKind::Feedback => DecayParams {
                decay_rate: 0.999,
                min_score: 0.5,
                boost_on_access: 0.1,
            },
            ObservationKind::Decision => DecayParams {
                decay_rate: 0.998,
                min_score: 0.3,
                boost_on_access: 0.1,
            },
            ObservationKind::ErrorPattern => DecayParams {
                decay_rate: 0.997,
                min_score: 0.2,
                boost_on_access: 0.15,
            },
            ObservationKind::Reference => DecayParams {
                decay_rate: 0.995,
                min_score: 0.2,
                boost_on_access: 0.1,
            },
            ObservationKind::Project => DecayParams {
                decay_rate: 0.99,
                min_score: 0.1,
                boost_on_access: 0.2,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct DecayTable {
    by_kind: HashMap<String, DecayParams>,
}

impl DecayTable {
    pub fn load(conn: &Connection) -> Result<Self> {
        let mut by_kind = HashMap::new();
        let mut stmt =
            conn.prepare("SELECT kind, decay_rate, min_score, boost_on_access FROM decay_config")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                DecayParams {
                    decay_rate: row.get::<_, f64>(1)?,
                    min_score: row.get::<_, f64>(2)?,
                    boost_on_access: row.get::<_, f64>(3)?,
                },
            ))
        })?;
        for r in rows {
            let (k, p) = r?;
            by_kind.insert(k, p);
        }
        Ok(Self { by_kind })
    }

    pub fn params(&self, kind: ObservationKind) -> DecayParams {
        self.by_kind
            .get(kind.as_str())
            .copied()
            .unwrap_or_else(|| DecayParams::default_for(kind))
    }
}

/// Apply decay to a stored relevance score.
///
/// Formula: `max(stored * decay_rate^days_since_access, min_score)`.
/// Days are computed from the supplied `now_epoch` and
/// `last_accessed_epoch`; if the latter is `None` we use
/// `created_at_epoch`.
///
/// `boost_on_access` is added once each time the observation is read; it
/// is applied at recall time, not here, so the persisted value is the
/// one users see in `crux memory dump`.
pub fn decayed_score(
    params: DecayParams,
    stored_score: f64,
    last_seen_epoch: i64,
    now_epoch: i64,
) -> f64 {
    if params.decay_rate >= 1.0 {
        return stored_score.max(params.min_score);
    }
    let days = ((now_epoch - last_seen_epoch).max(0) as f64) / 86_400.0;
    let scaled = stored_score * params.decay_rate.powf(days);
    scaled.max(params.min_score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guardrail_never_drops_below_one() {
        let p = DecayParams::default_for(ObservationKind::Guardrail);
        let s = decayed_score(p, 1.0, 0, 86_400 * 1000);
        assert_eq!(s, 1.0);
    }

    #[test]
    fn project_decays_over_time() {
        let p = DecayParams::default_for(ObservationKind::Project);
        // 30 days at rate 0.99 → ~0.74
        let s = decayed_score(p, 1.0, 0, 86_400 * 30);
        assert!(s < 1.0 && s > 0.7, "got {s}");
    }

    #[test]
    fn floor_clamps_long_term() {
        let p = DecayParams::default_for(ObservationKind::Reference);
        let s = decayed_score(p, 1.0, 0, 86_400 * 10_000);
        assert!((s - p.min_score).abs() < 1e-9);
    }
}
