use std::{
    collections::HashMap,
    fmt::{Display, Formatter},
};

use tracing::warn;

use crate::error::ScenarioError;

pub struct ErrorRecorder {
    pub success: u64,
    pub failures: u64,
    pub error_map: HashMap<&'static str, u64>,
}

impl Display for ErrorRecorder {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let total = self.success + self.failures;

        let success_rate = if total == 0 {
            0.0
        } else {
            self.success as f64 / total as f64 * 100.0
        };

        let error_rate = if total == 0 {
            0.0
        } else {
            self.failures as f64 / total as f64 * 100.0
        };

        writeln!(f, "Requests: {total}")?;
        writeln!(f, "Success: {} ({success_rate:.2}%)", self.success)?;
        writeln!(f, "Error: {} ({error_rate:.2}%)", self.failures)?;

        if !self.error_map.is_empty() {
            writeln!(f)?;
            writeln!(f, "Error Breakdown:")?;

            for (kind, count) in &self.error_map {
                writeln!(f, "  {kind}: {count}")?;
            }
        }

        Ok(())
    }
}

impl ErrorRecorder {
    pub fn new() -> Self {
        Self {
            success: 0,
            failures: 0,
            error_map: HashMap::new(),
        }
    }

    pub fn record<T, E>(&mut self, result: &std::result::Result<T, E>)
    where
        E: ScenarioError,
    {
        match result {
            Ok(_) => self.success += 1,
            Err(err) => {
                warn!("{:?}", err);
                self.failures += 1;
                self.error_map
                    .entry(err.kind())
                    .and_modify(|count| *count += 1)
                    .or_insert(1);
            }
        }
    }

    pub fn merge(&mut self, other: Self) {
        self.success += other.success;
        self.failures += other.failures;
        for (kind, count) in other.error_map {
            *self.error_map.entry(kind).or_insert(0) += count;
        }
    }
}
