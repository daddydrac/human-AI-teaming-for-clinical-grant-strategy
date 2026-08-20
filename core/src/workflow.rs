use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Intake,
    Documents,
    Requirements,
    Interview,
    Research,
    Science,
    Strategy,
    Writing,
    Review,
    Export,
}

impl Stage {
    pub const ALL: [Stage; 10] = [
        Stage::Intake,
        Stage::Documents,
        Stage::Requirements,
        Stage::Interview,
        Stage::Research,
        Stage::Science,
        Stage::Strategy,
        Stage::Writing,
        Stage::Review,
        Stage::Export,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Intake => "intake",
            Stage::Documents => "documents",
            Stage::Requirements => "requirements",
            Stage::Interview => "interview",
            Stage::Research => "research",
            Stage::Strategy => "strategy",
            Stage::Science => "science",
            Stage::Writing => "writing",
            Stage::Review => "review",
            Stage::Export => "export",
        }
    }

    pub fn at_least(self, minimum: Stage) -> bool { self >= minimum }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
}

impl FromStr for Stage {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        for stage in Self::ALL {
            if stage.as_str() == s { return Ok(stage); }
        }
        bail!("invalid workflow stage: {s}")
    }
}

pub fn require_at_least(current: Stage, minimum: Stage, operation: &str) -> Result<()> {
    if !current.at_least(minimum) {
        bail!("workflow gate: {operation} requires stage '{}' or later; current stage is '{}'", minimum, current);
    }
    Ok(())
}
