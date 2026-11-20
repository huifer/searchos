//! RFC-042: billing / invoice read model over the append-only cost ledger.
//!
//! This is an INVOICE VIEW, not a payment system: every amount comes from
//! `cost_events`, estimated amounts are never rewritten, and an event stays
//! `unreconciled` until a real provider bill is imported. Invoices are grouped
//! by currency so totals are never added across currencies.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One provider + operation line on an invoice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BillingLine {
    pub provider: String,
    pub operation: String,
    pub events: u32,
    /// Events with no real bill imported yet.
    pub unreconciled_events: u32,
    pub estimated_micros: i64,
    /// Only real provider bills; zero when nothing was reconciled.
    pub actual_micros: i64,
    pub currency: String,
}

/// All lines for one currency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BillingInvoice {
    pub currency: String,
    pub lines: Vec<BillingLine>,
    pub total_estimated_micros: i64,
    pub total_actual_micros: i64,
    pub events: u32,
    pub unreconciled_events: u32,
    /// True only when every event on the invoice has a real bill.
    pub fully_reconciled: bool,
    pub generated_at: DateTime<Utc>,
}

impl BillingInvoice {
    pub fn new(currency: impl Into<String>, lines: Vec<BillingLine>) -> Self {
        let currency = currency.into();
        let mut events = 0u32;
        let mut unreconciled_events = 0u32;
        let mut total_estimated_micros = 0i64;
        let mut total_actual_micros = 0i64;
        for line in &lines {
            events = events.saturating_add(line.events);
            unreconciled_events = unreconciled_events.saturating_add(line.unreconciled_events);
            total_estimated_micros = total_estimated_micros.saturating_add(line.estimated_micros);
            total_actual_micros = total_actual_micros.saturating_add(line.actual_micros);
        }
        Self {
            currency,
            lines,
            total_estimated_micros,
            total_actual_micros,
            events,
            unreconciled_events,
            fully_reconciled: events > 0 && unreconciled_events == 0,
            generated_at: Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(estimated: i64, actual: i64, unreconciled: u32) -> BillingLine {
        BillingLine {
            provider: "tavily".into(),
            operation: "web_search".into(),
            events: 1,
            unreconciled_events: unreconciled,
            estimated_micros: estimated,
            actual_micros: actual,
            currency: "USD".into(),
        }
    }

    #[test]
    fn totals_are_integer_micros_and_never_cross_currencies_here() {
        let invoice = BillingInvoice::new("USD", vec![line(100, 0, 1), line(50, 50, 0)]);
        assert_eq!(invoice.total_estimated_micros, 150);
        assert_eq!(invoice.total_actual_micros, 50);
        assert_eq!(invoice.events, 2);
        assert_eq!(invoice.unreconciled_events, 1);
        assert!(!invoice.fully_reconciled);
    }

    #[test]
    fn fully_reconciled_requires_at_least_one_event_and_no_unbilled_rows() {
        assert!(!BillingInvoice::new("USD", vec![]).fully_reconciled);
        assert!(BillingInvoice::new("USD", vec![line(100, 100, 0)]).fully_reconciled);
    }
}
