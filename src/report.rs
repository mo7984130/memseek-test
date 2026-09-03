use std::{
    collections::HashMap,
    fmt::{Display, Formatter, Result},
    time::Duration,
};

use crate::recorder::Recorder;

#[derive(Debug)]
pub struct ScenarioReport {
    pub name: &'static str,
    pub times: u64,
    pub total: Duration,
    pub avg: Duration,
    pub min: Duration,
    pub max: Duration,
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,

    pub validate_success: u64,
    pub validate_failures: u64,

    pub success: u64,
    pub failures: u64,
    pub error_map: HashMap<&'static str, u64>,
}

impl ScenarioReport {
    pub fn from_recorder(name: &'static str, mut recorder: Recorder) -> Self {
        Self {
            name: name,
            times: recorder.times(),
            total: recorder.total(),
            avg: recorder.avg(),
            min: recorder.min(),
            max: recorder.max(),
            p50: recorder.p50(),
            p95: recorder.p95(),
            p99: recorder.p99(),

            validate_success: recorder.validate_success,
            validate_failures: recorder.validate_failures,

            success: recorder.success,
            failures: recorder.failures,
            error_map: recorder.error_map,
        }
    }
}

impl Display for ScenarioReport {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        writeln!(f, "Scenario: {}", self.name)?;
        writeln!(f, "  Times: {}", self.times)?;
        writeln!(f, "  Total: {:?}", self.total)?;
        writeln!(f, "  Min:  {:?}", self.min)?;
        writeln!(f, "  Avg:  {:?}", self.avg)?;
        writeln!(f, "  Max:  {:?}", self.max)?;
        writeln!(f, "  P50:  {:?}", self.p50)?;
        writeln!(f, "  P95:  {:?}", self.p95)?;
        writeln!(f, "  P99:  {:?}", self.p99)?;

        writeln!(f, "  Validate Success:  {}", self.validate_success)?;
        writeln!(f, "  Validate Failures: {}", self.validate_failures)?;

        writeln!(f, "  Success: {}", self.success)?;
        writeln!(f, "  Failures: {}", self.failures)?;

        if !self.error_map.is_empty() {
            writeln!(f, "  Errors:")?;

            for (kind, count) in &self.error_map {
                writeln!(f, "    {kind}: {count}")?;
            }
        }

        Ok(())
    }
}
