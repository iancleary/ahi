//! Session token, cost, and context figures.
//!
//! `None` means the figure is unknown and must be omitted. Zero is a known
//! value and is shown. The backend fills these figures. The status row renders them.

/// Session figures shown on the left of the session row.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SessionMeters {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) cached_input_tokens: Option<u64>,
    pub(crate) cost_usd: Option<f64>,
    pub(crate) subscription_sign_in: bool,
    pub(crate) context_tokens: Option<u64>,
    pub(crate) context_window: Option<u64>,
    pub(crate) automatic_compaction: bool,
}

impl SessionMeters {
    /// Add one usage report. Context used becomes this report's input count.
    pub(crate) fn record_usage(
        &mut self,
        input_tokens: u64,
        output_tokens: u64,
        cached_input_tokens: Option<u64>,
        cost_usd: Option<f64>,
    ) {
        add_count(&mut self.input_tokens, input_tokens);
        add_count(&mut self.output_tokens, output_tokens);
        if let Some(cached) = cached_input_tokens {
            add_count(&mut self.cached_input_tokens, cached);
        }
        if let Some(cost) = cost_usd {
            self.cost_usd = Some(self.cost_usd.unwrap_or(0.0) + cost);
        }
        self.context_tokens = Some(input_tokens);
    }

    pub(crate) fn set_context_window(&mut self, window: u64) {
        self.context_window = Some(window);
    }

    /// Drop token and cost totals. Subscription and compaction flags stay.
    pub(crate) fn clear_counts(&mut self) {
        *self = Self {
            subscription_sign_in: self.subscription_sign_in,
            automatic_compaction: self.automatic_compaction,
            ..Self::default()
        };
    }
}

fn add_count(slot: &mut Option<u64>, value: u64) {
    *slot = Some(slot.unwrap_or(0).saturating_add(value));
}
