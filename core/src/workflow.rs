use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fmt, path::Path, str::FromStr};

const EMBEDDED_WORKFLOW_DEFINITIONS: &str = include_str!("../../config/workflow_definitions.json");
const SUPPORTED_COMPLETION_EVALUATORS: [&str; 9] = [
    "solicitation_approved",
    "artifact_approved",
    "required_sections_approved",
    "investigator_interview_complete",
    "clinical_design_ready",
    "competitive_intelligence_ready",
    "sponsor_compliance_ready",
    "collaboration_routing_complete",
    "view_only",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowStepDefinition {
    pub key: String,
    pub title: String,
    pub description: String,
    pub placement: String,
    pub output: String,
    pub artifact_type: Option<String>,
    pub ui_surface: String,
    pub completion_evaluator: String,
    #[serde(default)]
    pub prerequisites: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowModuleDefinition {
    #[serde(flatten)]
    pub step: WorkflowStepDefinition,
    pub gate_default: bool,
    #[serde(default)]
    pub gate_configurable: bool,
    pub runtime_implication: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowPreset {
    pub key: String,
    pub title: String,
    #[serde(default)]
    pub enabled_modules: Vec<String>,
    #[serde(default)]
    pub required_modules: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewModeDefinition {
    pub key: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewerArchetypeDefinition {
    pub key: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub criterion_terms: Vec<String>,
    #[serde(default)]
    pub grant_types: Vec<String>,
    #[serde(default)]
    pub always_include: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowRegistry {
    pub schema_version: u32,
    pub definition_version: u32,
    pub default_preset_key: String,
    pub legacy_preset_key: String,
    pub review_module_key: String,
    #[serde(default)]
    pub model_routing_modes: Vec<String>,
    #[serde(default)]
    pub gate_tokens: Vec<String>,
    pub core_steps: Vec<WorkflowStepDefinition>,
    pub optional_modules: Vec<WorkflowModuleDefinition>,
    pub presets: Vec<WorkflowPreset>,
    pub review_modes: Vec<ReviewModeDefinition>,
    #[serde(default)]
    pub reviewer_archetypes: Vec<ReviewerArchetypeDefinition>,
}

impl WorkflowRegistry {
    pub fn load() -> Result<Self> {
        let configured = std::env::var("WORKFLOW_DEFINITIONS_PATH").ok();
        let raw = if let Some(path) = configured {
            std::fs::read_to_string(&path)
                .with_context(|| format!("read workflow definitions {path}"))?
        } else {
            let deployed = Path::new("/app/config/workflow_definitions.json");
            if deployed.exists() {
                std::fs::read_to_string(deployed)
                    .with_context(|| format!("read workflow definitions {}", deployed.display()))?
            } else {
                EMBEDDED_WORKFLOW_DEFINITIONS.to_owned()
            }
        };
        let registry: Self = serde_json::from_str(&raw).context("parse workflow definitions")?;
        registry.validate()?;
        Ok(registry)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            bail!(
                "unsupported workflow registry schema version {}",
                self.schema_version
            );
        }
        if self.definition_version == 0 {
            bail!("workflow definition version must be positive");
        }
        if self.core_steps.len() != 5 {
            bail!("workflow registry must contain exactly five mandatory core steps");
        }
        if self.model_routing_modes.is_empty() {
            bail!("workflow registry has no model routing modes");
        }
        if self.reviewer_archetypes.is_empty() {
            bail!("workflow registry has no reviewer archetypes");
        }
        let mut reviewer_keys = BTreeSet::new();
        for reviewer in &self.reviewer_archetypes {
            if reviewer.key.trim().is_empty()
                || reviewer.title.trim().is_empty()
                || reviewer.description.trim().is_empty()
                || !reviewer_keys.insert(reviewer.key.as_str())
            {
                bail!("reviewer archetypes require unique non-empty keys, titles, and descriptions");
            }
        }

        let mut all_keys = BTreeSet::new();
        let known_evaluators: BTreeSet<&str> =
            SUPPORTED_COMPLETION_EVALUATORS.into_iter().collect();
        for step in &self.core_steps {
            validate_definition(step, &known_evaluators)?;
            if !all_keys.insert(step.key.as_str()) {
                bail!("duplicate workflow key: {}", step.key);
            }
        }
        for module in &self.optional_modules {
            validate_definition(&module.step, &known_evaluators)?;
            if !all_keys.insert(module.step.key.as_str()) {
                bail!("duplicate workflow key: {}", module.step.key);
            }
        }
        let known_refs: BTreeSet<&str> = all_keys
            .iter()
            .copied()
            .chain(self.gate_tokens.iter().map(String::as_str))
            .collect();
        for step in self
            .core_steps
            .iter()
            .chain(self.optional_modules.iter().map(|m| &m.step))
        {
            for prerequisite in &step.prerequisites {
                if !known_refs.contains(prerequisite.as_str()) {
                    bail!(
                        "workflow step {} has unknown prerequisite {prerequisite}",
                        step.key
                    );
                }
                if prerequisite == &step.key {
                    bail!("workflow step {} depends on itself", step.key);
                }
            }
        }
        for (index, step) in self.core_steps.iter().enumerate() {
            let earlier: BTreeSet<&str> = self.core_steps[..index]
                .iter()
                .map(|s| s.key.as_str())
                .collect();
            for prerequisite in &step.prerequisites {
                if self.core_step(prerequisite).is_some()
                    && !earlier.contains(prerequisite.as_str())
                {
                    bail!(
                        "core workflow prerequisite {prerequisite} must precede {}",
                        step.key
                    );
                }
            }
        }

        let module_keys: BTreeSet<&str> = self
            .optional_modules
            .iter()
            .map(|m| m.step.key.as_str())
            .collect();
        let mut preset_keys = BTreeSet::new();
        for preset in &self.presets {
            if preset.key.trim().is_empty() || preset.title.trim().is_empty() {
                bail!("workflow preset keys and titles are required");
            }
            if !preset_keys.insert(preset.key.as_str()) {
                bail!("duplicate workflow preset key: {}", preset.key);
            }
            for key in preset
                .enabled_modules
                .iter()
                .chain(&preset.required_modules)
            {
                if !module_keys.contains(key.as_str()) {
                    bail!("preset {} references unknown module {key}", preset.key);
                }
            }
            let enabled: BTreeSet<&str> =
                preset.enabled_modules.iter().map(String::as_str).collect();
            for key in &preset.required_modules {
                if !enabled.contains(key.as_str()) {
                    bail!("preset {} requires disabled module {key}", preset.key);
                }
                if !self
                    .module(key)
                    .is_some_and(|module| module.gate_configurable)
                {
                    bail!(
                        "preset {} requires module {key}, but that module has no supported completion gate",
                        preset.key
                    );
                }
            }
        }
        if self.preset(&self.default_preset_key).is_none() {
            bail!("default workflow preset is missing");
        }
        if self.preset(&self.legacy_preset_key).is_none() {
            bail!("legacy workflow preset is missing");
        }
        if !module_keys.contains(self.review_module_key.as_str()) {
            bail!("review module key is not an optional module");
        }

        let mut review_keys = BTreeSet::new();
        for mode in &self.review_modes {
            if mode.key.trim().is_empty() || mode.title.trim().is_empty() {
                bail!("review mode keys and titles are required");
            }
            if !review_keys.insert(mode.key.as_str()) {
                bail!("duplicate review mode key: {}", mode.key);
            }
        }
        Ok(())
    }

    pub fn core_step(&self, key: &str) -> Option<&WorkflowStepDefinition> {
        self.core_steps.iter().find(|step| step.key == key)
    }

    pub fn module(&self, key: &str) -> Option<&WorkflowModuleDefinition> {
        self.optional_modules
            .iter()
            .find(|module| module.step.key == key)
    }

    pub fn preset(&self, key: &str) -> Option<&WorkflowPreset> {
        self.presets.iter().find(|preset| preset.key == key)
    }

    pub fn definition_sha256(&self) -> Result<String> {
        let bytes = serde_json::to_vec(self)?;
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Ok(hex::encode(hasher.finalize()))
    }

    pub fn as_json(&self) -> Result<Value> {
        Ok(serde_json::to_value(self)?)
    }

    pub fn default_config(&self) -> Result<WorkflowConfig> {
        WorkflowConfig::from_preset(self, &self.default_preset_key)
    }

    pub fn legacy_config(&self) -> Result<WorkflowConfig> {
        WorkflowConfig::from_preset(self, &self.legacy_preset_key)
    }
}

fn validate_definition(step: &WorkflowStepDefinition, evaluators: &BTreeSet<&str>) -> Result<()> {
    if step.key.trim().is_empty()
        || step.title.trim().is_empty()
        || step.description.trim().is_empty()
        || step.output.trim().is_empty()
        || step.ui_surface.trim().is_empty()
    {
        bail!("workflow definitions require non-empty keys, titles, descriptions, outputs, and UI surfaces");
    }
    if !evaluators.contains(step.completion_evaluator.as_str()) {
        bail!(
            "workflow step {} uses unsupported completion evaluator {}",
            step.key,
            step.completion_evaluator
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowConfig {
    #[serde(default = "workflow_schema_version")]
    pub schema_version: u32,
    pub definition_version: u32,
    pub template: String,
    #[serde(default)]
    pub enabled_modules: Vec<String>,
    #[serde(default)]
    pub required_modules: Vec<String>,
    #[serde(default)]
    pub review_mode: Option<String>,
    #[serde(default)]
    pub review_required: bool,
    #[serde(default)]
    pub grant_type: Option<String>,
    #[serde(default)]
    pub target_deadline: Option<String>,
    #[serde(default)]
    pub model_routing_mode: Option<String>,
    #[serde(default)]
    pub local_model_provider: Option<String>,
    #[serde(default)]
    pub local_model: Option<String>,
    #[serde(default)]
    pub cloud_model: Option<String>,
    #[serde(default)]
    pub cloud_task_kinds: Vec<String>,
}

fn workflow_schema_version() -> u32 {
    1
}

impl WorkflowConfig {
    pub fn from_preset(registry: &WorkflowRegistry, preset_key: &str) -> Result<Self> {
        let preset = registry
            .preset(preset_key)
            .with_context(|| format!("workflow preset not found: {preset_key}"))?;
        Ok(Self {
            schema_version: registry.schema_version,
            definition_version: registry.definition_version,
            template: preset.key.clone(),
            enabled_modules: preset.enabled_modules.clone(),
            required_modules: preset.required_modules.clone(),
            review_mode: None,
            review_required: false,
            grant_type: None,
            target_deadline: None,
            model_routing_mode: None,
            local_model_provider: None,
            local_model: None,
            cloud_model: None,
            cloud_task_kinds: Vec::new(),
        })
    }

    pub fn validate(&self, registry: &WorkflowRegistry) -> Result<()> {
        if self.schema_version != registry.schema_version {
            bail!(
                "unsupported workflow schema version {}",
                self.schema_version
            );
        }
        if self.definition_version != registry.definition_version {
            bail!(
                "workflow definition version {} does not match active version {}",
                self.definition_version,
                registry.definition_version
            );
        }
        if registry.preset(&self.template).is_none() {
            bail!("unknown workflow template: {}", self.template);
        }
        let allowed: BTreeSet<&str> = registry
            .optional_modules
            .iter()
            .map(|m| m.step.key.as_str())
            .collect();
        for key in self
            .enabled_modules
            .iter()
            .chain(self.required_modules.iter())
        {
            if !allowed.contains(key.as_str()) {
                bail!("unknown workflow module: {key}");
            }
        }
        let enabled: BTreeSet<&str> = self.enabled_modules.iter().map(String::as_str).collect();
        for key in &self.required_modules {
            if !enabled.contains(key.as_str()) {
                bail!("required module is not enabled: {key}");
            }
            if !registry
                .module(key)
                .is_some_and(|module| module.gate_configurable)
            {
                bail!(
                    "module {key} cannot be required because it has no supported completion gate"
                );
            }
        }
        if self.review_required && !enabled.contains(registry.review_module_key.as_str()) {
            bail!(
                "{} must be enabled when a passing simulated review is required",
                registry.review_module_key
            );
        }
        if let Some(mode) = self.review_mode.as_deref() {
            if !registry.review_modes.iter().any(|item| item.key == mode) {
                bail!("invalid review mode: {mode}");
            }
        }
        if let Some(mode) = self.model_routing_mode.as_deref() {
            if !registry.model_routing_modes.iter().any(|item| item == mode) {
                bail!("invalid model routing mode: {mode}");
            }
        }
        Ok(())
    }

    pub fn enabled(&self, key: &str) -> bool {
        self.enabled_modules.iter().any(|x| x == key)
    }
    pub fn required(&self, registry: &WorkflowRegistry, key: &str) -> bool {
        self.required_modules.iter().any(|x| x == key)
            || (key == registry.review_module_key && self.review_required)
    }
}

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

    pub fn at_least(self, minimum: Stage) -> bool {
        self >= minimum
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Stage {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        for stage in Self::ALL {
            if stage.as_str() == s {
                return Ok(stage);
            }
        }
        bail!("invalid workflow stage: {s}")
    }
}

pub fn require_at_least(current: Stage, minimum: Stage, operation: &str) -> Result<()> {
    if !current.at_least(minimum) {
        bail!(
            "workflow gate: {operation} requires stage '{}' or later; current stage is '{}'",
            minimum,
            current
        );
    }
    Ok(())
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    #[test]
    fn registry_and_every_optional_module_combination_validate() -> Result<()> {
        let registry = WorkflowRegistry::load()?;
        let count = registry.optional_modules.len();
        assert!(count < usize::BITS as usize);
        for mask in 0..(1usize << count) {
            let mut config = registry.default_config()?;
            config.enabled_modules = registry
                .optional_modules
                .iter()
                .enumerate()
                .filter(|(index, _)| mask & (1usize << index) != 0)
                .map(|(_, module)| module.step.key.clone())
                .collect();
            config.required_modules = registry
                .optional_modules
                .iter()
                .filter(|module| module.gate_default && config.enabled(&module.step.key))
                .map(|module| module.step.key.clone())
                .collect();
            config.validate(&registry)?;
        }
        Ok(())
    }

    #[test]
    fn required_disabled_module_is_rejected() -> Result<()> {
        let registry = WorkflowRegistry::load()?;
        let module = registry
            .optional_modules
            .first()
            .context("registry has no optional modules")?;
        let mut config = registry.default_config()?;
        config.required_modules.push(module.step.key.clone());
        assert!(config.validate(&registry).is_err());
        Ok(())
    }

    #[test]
    fn default_workflow_has_only_five_core_outcomes_and_no_optional_gates() -> Result<()> {
        let registry = WorkflowRegistry::load()?;
        let config = registry.default_config()?;
        assert_eq!(registry.core_steps.len(), 5);
        assert!(config.enabled_modules.is_empty());
        assert!(config.required_modules.is_empty());
        assert!(!config.review_required);
        Ok(())
    }

    #[test]
    fn module_without_a_completion_surface_cannot_be_required() -> Result<()> {
        let registry = WorkflowRegistry::load()?;
        let module = registry
            .optional_modules
            .iter()
            .find(|module| !module.gate_configurable)
            .context("registry has no advisory-only module")?;
        let mut config = registry.default_config()?;
        config.enabled_modules.push(module.step.key.clone());
        config.required_modules.push(module.step.key.clone());
        assert!(config.validate(&registry).is_err());
        Ok(())
    }
}
