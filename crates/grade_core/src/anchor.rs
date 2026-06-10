//! Absolute anchors for hybrid cohort normalization.

use serde::{Deserialize, Serialize};

/// Absolute floor/ceiling for hybrid cohort normalization of one raw metric.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricAnchor {
    pub floor: f64,
    pub ceiling: f64,
}
