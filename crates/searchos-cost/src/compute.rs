//! Local compute metering (RFC-008 8.2).
//!
//! For resources that cannot be directly monetized, both raw resource
//! metrics and converted cost are retained (MVP precision: wall/cpu proxy
//! time; hardware depreciation amortization is disabled in MVP per RFC).

use searchos_domain::cost::{CostOperation, Money};
use serde::{Deserialize, Serialize};

/// Raw + converted metrics of one local operation (RFC-008 8.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalComputeRecord {
    pub operation: CostOperation,
    /// Wall duration of the operation in ms.
    pub duration_ms: u64,
    /// CPU-time estimate in ms (MVP proxy: measured wall time of the
    /// synchronous section; refine with procfs/getrusage later).
    pub cpu_millis: u64,
    /// Bytes read (I/O).
    pub io_read_bytes: u64,
    /// Bytes written (I/O).
    pub io_write_bytes: u64,
    pub estimated_cost: Money,
    pub pricing_model_version: String,
}

/// Measures and converts local compute usage.
#[derive(Debug, Clone)]
pub struct LocalComputeMeter {
    estimator: crate::estimator::CostEstimator,
}

impl LocalComputeMeter {
    pub fn new(estimator: crate::estimator::CostEstimator) -> Self {
        Self { estimator }
    }

    /// Records a timed local operation and converts it to money.
    pub fn record(
        &self,
        operation: CostOperation,
        duration_ms: u64,
        io_read_bytes: u64,
        io_write_bytes: u64,
    ) -> LocalComputeRecord {
        let cpu_cost = self.estimator.estimate_cpu_millis(duration_ms);
        let io_cost = self.estimator.estimate_storage_bytes(io_write_bytes);
        let transfer = self
            .estimator
            .estimate_transfer_bytes(io_read_bytes + io_write_bytes);
        let total = cpu_cost
            .checked_add(&io_cost)
            .and_then(|m| m.checked_add(&transfer))
            .unwrap_or(cpu_cost);
        LocalComputeRecord {
            operation,
            duration_ms,
            cpu_millis: duration_ms, // MVP proxy, documented
            io_read_bytes,
            io_write_bytes,
            estimated_cost: total,
            pricing_model_version: self.estimator.rates().version.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::estimator::CostEstimator;
    use crate::model::CostModelConfig;

    #[test]
    fn records_raw_and_converted() {
        let meter = LocalComputeMeter::new(CostEstimator::from_config(
            &CostModelConfig::default_config(),
        ));
        let rec = meter.record(CostOperation::LocalBm25, 200, 1024, 4096);
        assert_eq!(rec.duration_ms, 200);
        assert!(rec.estimated_cost.amount_micros > 0);
        assert_eq!(rec.pricing_model_version, "1");
    }
}
