use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, PathBuf};

use serde::{Deserialize, Serialize};

// This module contains pure application publication authority state.

pub(super) const GENERATION_AUTHORITY_FILE: &str = ".perfectpixel-generation.json";
pub(super) const GENERATION_AUTHORITY_SCHEMA: &str = "perfectpixel.generation-authority/1";
// The generic artifact transaction admits 4,096 touched paths and 512 MiB of
// writes. A generation replacement can touch every old path, every new path,
// and the authority file, so the product authority is deliberately capped at
// half that path budget and reserves 1 MiB for its serialized authority.
pub(super) const MAX_GENERATION_ARTIFACTS: usize = 2_047;
pub(super) const MAX_GENERATION_BYTES: u64 = 511 * 1024 * 1024;
const MAX_GENERATION_PATH_BYTES: usize = 1_024;
const MAX_GENERATION_PATH_DEPTH: usize = 16;
const MAX_WORKFLOW_RECORDS: usize = 4;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum GenerationWorkflow {
    Normalize,
    Bundle,
    MotionScaffold,
    MotionBuild,
}

impl GenerationWorkflow {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Normalize => "normalize",
            Self::Bundle => "bundle",
            Self::MotionScaffold => "motion-scaffold",
            Self::MotionBuild => "motion-build",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct GenerationArtifact {
    pub relative_path: String,
    pub sha256: String,
    pub byte_count: u64,
}

impl GenerationArtifact {
    pub(super) fn new(
        relative_path: String,
        sha256: String,
        byte_count: u64,
    ) -> Result<Self, String> {
        validate_relative_generation_path(&relative_path)?;
        validate_sha256(&sha256)?;
        Ok(Self {
            relative_path,
            sha256,
            byte_count,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct GenerationRecord {
    pub workflow: GenerationWorkflow,
    pub artifacts: Vec<GenerationArtifact>,
}

impl GenerationRecord {
    pub(super) fn new(
        workflow: GenerationWorkflow,
        mut artifacts: Vec<GenerationArtifact>,
    ) -> Result<Self, String> {
        artifacts.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        validate_generation_artifacts(&artifacts)?;
        Ok(Self {
            workflow,
            artifacts,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct GenerationAuthority {
    pub schema: String,
    pub generations: Vec<GenerationRecord>,
}

impl GenerationAuthority {
    pub(super) fn new(mut generations: Vec<GenerationRecord>) -> Result<Self, String> {
        for generation in &mut generations {
            generation
                .artifacts
                .sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        }
        generations.sort_by_key(|generation| generation.workflow);
        let authority = Self {
            schema: GENERATION_AUTHORITY_SCHEMA.to_string(),
            generations,
        };
        authority.validate()?;
        Ok(authority)
    }

    pub(super) fn from_slice(bytes: &[u8]) -> Result<Self, String> {
        let parsed: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        if parsed.schema != GENERATION_AUTHORITY_SCHEMA {
            return Err(format!(
                "unsupported generation authority schema '{}'",
                parsed.schema
            ));
        }
        Self::new(parsed.generations)
    }

    pub(super) fn to_pretty_json(&self) -> Result<String, String> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(|error| error.to_string())
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema != GENERATION_AUTHORITY_SCHEMA {
            return Err(format!(
                "unsupported generation authority schema '{}'",
                self.schema
            ));
        }
        if self.generations.len() > MAX_WORKFLOW_RECORDS {
            return Err("generation authority has too many workflow records".to_string());
        }
        let mut workflows = BTreeSet::new();
        let mut all_paths = BTreeSet::new();
        let mut total_artifacts = 0usize;
        let mut total_bytes = 0u64;
        for generation in &self.generations {
            if !workflows.insert(generation.workflow) {
                return Err(format!(
                    "generation authority repeats workflow '{}'",
                    generation.workflow.as_str()
                ));
            }
            validate_generation_artifacts(&generation.artifacts)?;
            total_artifacts = total_artifacts
                .checked_add(generation.artifacts.len())
                .ok_or_else(|| "generation authority artifact count overflow".to_string())?;
            if total_artifacts > MAX_GENERATION_ARTIFACTS {
                return Err("generation authority exceeds artifact limit".to_string());
            }
            for artifact in &generation.artifacts {
                total_bytes = total_bytes
                    .checked_add(artifact.byte_count)
                    .ok_or_else(|| "generation authority byte count overflow".to_string())?;
                if total_bytes > MAX_GENERATION_BYTES {
                    return Err("generation authority exceeds byte limit".to_string());
                }
                if !all_paths.insert(artifact.relative_path.clone()) {
                    return Err(format!(
                        "generation authority path '{}' is owned by more than one workflow",
                        artifact.relative_path
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum GenerationAuthorityState {
    Absent,
    Published(GenerationAuthority),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum GenerationAuthorityEvent {
    Replace {
        workflow: GenerationWorkflow,
        artifacts: Vec<GenerationArtifact>,
        invalidates: Vec<GenerationWorkflow>,
    },
}

impl GenerationAuthorityEvent {
    pub(super) fn replace(
        workflow: GenerationWorkflow,
        mut artifacts: Vec<GenerationArtifact>,
        invalidates: Vec<GenerationWorkflow>,
    ) -> Result<Self, String> {
        artifacts.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        validate_generation_artifacts(&artifacts)?;
        if invalidates.contains(&workflow) {
            return Err(format!(
                "workflow '{}' cannot invalidate itself",
                workflow.as_str()
            ));
        }
        let mut seen_invalidations = BTreeSet::new();
        for invalidated in &invalidates {
            if !seen_invalidations.insert(*invalidated) {
                return Err(format!(
                    "workflow '{}' is invalidated more than once",
                    invalidated.as_str()
                ));
            }
        }
        Ok(Self::Replace {
            workflow,
            artifacts,
            invalidates,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct GenerationTransition {
    pub next_authority: GenerationAuthority,
    /// Artifacts whose workflow is preserved by this event. They must remain
    /// byte-identical because the next authority continues to claim them.
    pub retained_artifacts: Vec<GenerationArtifact>,
    /// Every artifact owned by the workflow being replaced or invalidated.
    /// Adapters use this immutable evidence to verify that managed outputs were
    /// not changed by a non-participating writer between planning and mutation.
    pub superseded_artifacts: Vec<GenerationArtifact>,
    pub stale_artifacts: Vec<GenerationArtifact>,
}

pub(super) fn reduce_generation_authority(
    state: GenerationAuthorityState,
    event: GenerationAuthorityEvent,
) -> Result<GenerationTransition, String> {
    let authority = match state {
        GenerationAuthorityState::Absent => GenerationAuthority::new(Vec::new())?,
        GenerationAuthorityState::Published(authority) => {
            authority.validate()?;
            authority
        }
    };
    let GenerationAuthorityEvent::Replace {
        workflow,
        artifacts,
        invalidates,
    } = event;

    let mut generations = authority
        .generations
        .into_iter()
        .map(|record| (record.workflow, record.artifacts))
        .collect::<BTreeMap<_, _>>();
    let current_paths = artifacts
        .iter()
        .map(|artifact| artifact.relative_path.clone())
        .collect::<BTreeSet<_>>();
    let invalidated = invalidates.iter().copied().collect::<BTreeSet<_>>();

    let mut retained_by_path = BTreeMap::<String, GenerationArtifact>::new();
    for (existing_workflow, existing_artifacts) in &generations {
        if *existing_workflow == workflow || invalidated.contains(existing_workflow) {
            continue;
        }
        for artifact in existing_artifacts {
            if current_paths.contains(&artifact.relative_path) {
                return Err(format!(
                    "artifact '{}' is already owned by workflow '{}'",
                    artifact.relative_path,
                    existing_workflow.as_str()
                ));
            }
            retained_by_path.insert(artifact.relative_path.clone(), artifact.clone());
        }
    }

    let mut superseded_by_path = BTreeMap::<String, GenerationArtifact>::new();
    let mut stale_by_path = BTreeMap::<String, GenerationArtifact>::new();
    for superseded_workflow in std::iter::once(workflow).chain(invalidates.iter().copied()) {
        if let Some(previous) = generations.remove(&superseded_workflow) {
            for artifact in previous {
                superseded_by_path.insert(artifact.relative_path.clone(), artifact.clone());
                if !current_paths.contains(&artifact.relative_path) {
                    stale_by_path.insert(artifact.relative_path.clone(), artifact);
                }
            }
        }
    }
    generations.insert(workflow, artifacts);

    let next_records = generations
        .into_iter()
        .map(|(workflow, artifacts)| GenerationRecord::new(workflow, artifacts))
        .collect::<Result<Vec<_>, _>>()?;
    let next_authority = GenerationAuthority::new(next_records)?;
    Ok(GenerationTransition {
        next_authority,
        retained_artifacts: retained_by_path.into_values().collect(),
        superseded_artifacts: superseded_by_path.into_values().collect(),
        stale_artifacts: stale_by_path.into_values().collect(),
    })
}

fn validate_generation_artifacts(artifacts: &[GenerationArtifact]) -> Result<(), String> {
    if artifacts.is_empty() {
        return Err("generation must own at least one artifact".to_string());
    }
    if artifacts.len() > MAX_GENERATION_ARTIFACTS {
        return Err("generation has too many artifacts".to_string());
    }
    let mut paths = BTreeSet::new();
    let mut total_bytes = 0u64;
    for artifact in artifacts {
        validate_relative_generation_path(&artifact.relative_path)?;
        validate_sha256(&artifact.sha256)?;
        total_bytes = total_bytes
            .checked_add(artifact.byte_count)
            .ok_or_else(|| "generation artifact bytes overflow".to_string())?;
        if artifact.byte_count > MAX_GENERATION_BYTES || total_bytes > MAX_GENERATION_BYTES {
            return Err("generation exceeds byte limit".to_string());
        }
        if !paths.insert(artifact.relative_path.clone()) {
            return Err(format!(
                "generation repeats artifact path '{}'",
                artifact.relative_path
            ));
        }
    }
    Ok(())
}

fn validate_relative_generation_path(path: &str) -> Result<(), String> {
    if path.trim().is_empty()
        || path != path.trim()
        || path.contains('\0')
        || path.contains('\\')
        || path.len() > MAX_GENERATION_PATH_BYTES
    {
        return Err(format!("generation artifact path '{path}' is not safe"));
    }
    if path == GENERATION_AUTHORITY_FILE {
        return Err("generation authority file is managed by the publisher".to_string());
    }
    let path_buf = PathBuf::from(path);
    if path_buf.is_absolute() {
        return Err(format!(
            "generation artifact path '{path}' must be relative"
        ));
    }
    let mut depth = 0usize;
    for component in path_buf.components() {
        match component {
            Component::Normal(_) => depth += 1,
            _ => {
                return Err(format!(
                    "generation artifact path '{path}' must be normalized"
                ))
            }
        }
    }
    if depth == 0 || depth > MAX_GENERATION_PATH_DEPTH {
        return Err(format!(
            "generation artifact path '{path}' exceeds depth limit"
        ));
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), String> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err("generation artifact sha256 must be 64 lowercase hexadecimal characters".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(path: &str) -> GenerationArtifact {
        artifact_with_bytes(path, 1)
    }

    fn artifact_with_bytes(path: &str, byte_count: u64) -> GenerationArtifact {
        GenerationArtifact::new(path.to_string(), "0".repeat(64), byte_count).expect("artifact")
    }

    #[test]
    fn replacement_is_pure_and_returns_stale_outputs() {
        let state = GenerationAuthorityState::Published(
            GenerationAuthority::new(vec![GenerationRecord::new(
                GenerationWorkflow::Normalize,
                vec![
                    artifact("normalize-report.json"),
                    artifact("sprite-request.json"),
                ],
            )
            .expect("record")])
            .expect("authority"),
        );
        let event = GenerationAuthorityEvent::replace(
            GenerationWorkflow::Normalize,
            vec![artifact("normalize-report.json")],
            Vec::new(),
        )
        .expect("event");

        let transition = reduce_generation_authority(state, event).expect("transition");

        assert!(transition.retained_artifacts.is_empty());
        assert_eq!(
            transition.superseded_artifacts,
            vec![
                artifact("normalize-report.json"),
                artifact("sprite-request.json")
            ]
        );
        assert_eq!(
            transition.stale_artifacts,
            vec![artifact("sprite-request.json")]
        );
        assert_eq!(transition.next_authority.generations.len(), 1);
    }

    #[test]
    fn scaffold_invalidates_previous_build_generation() {
        let state = GenerationAuthorityState::Published(
            GenerationAuthority::new(vec![
                GenerationRecord::new(
                    GenerationWorkflow::MotionScaffold,
                    vec![artifact("scene.svg")],
                )
                .expect("scaffold record"),
                GenerationRecord::new(
                    GenerationWorkflow::MotionBuild,
                    vec![artifact("animated.svg"), artifact("dotlottie/a/old.json")],
                )
                .expect("build record"),
            ])
            .expect("authority"),
        );
        let event = GenerationAuthorityEvent::replace(
            GenerationWorkflow::MotionScaffold,
            vec![artifact("scene.svg"), artifact("layers.json")],
            vec![GenerationWorkflow::MotionBuild],
        )
        .expect("event");

        let transition = reduce_generation_authority(state, event).expect("transition");

        assert_eq!(
            transition
                .stale_artifacts
                .iter()
                .map(|artifact| artifact.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["animated.svg", "dotlottie/a/old.json"]
        );
        assert!(transition
            .next_authority
            .generations
            .iter()
            .all(|record| record.workflow != GenerationWorkflow::MotionBuild));
    }

    #[test]
    fn replacement_exposes_every_preserved_artifact_as_retained_evidence() {
        let state = GenerationAuthorityState::Published(
            GenerationAuthority::new(vec![
                GenerationRecord::new(
                    GenerationWorkflow::Normalize,
                    vec![artifact("normalize-report.json")],
                )
                .expect("normalize record"),
                GenerationRecord::new(
                    GenerationWorkflow::Bundle,
                    vec![artifact("manifest.json"), artifact("sheets/main.png")],
                )
                .expect("bundle record"),
            ])
            .expect("authority"),
        );
        let event = GenerationAuthorityEvent::replace(
            GenerationWorkflow::Normalize,
            vec![artifact("normalize-report.json")],
            Vec::new(),
        )
        .expect("event");

        let transition = reduce_generation_authority(state, event).expect("transition");

        assert_eq!(
            transition.retained_artifacts,
            vec![artifact("manifest.json"), artifact("sheets/main.png")]
        );
        assert_eq!(
            transition.superseded_artifacts,
            vec![artifact("normalize-report.json")]
        );
    }

    #[test]
    fn preserved_generation_blocks_competing_output_authority() {
        let state = GenerationAuthorityState::Published(
            GenerationAuthority::new(vec![GenerationRecord::new(
                GenerationWorkflow::Bundle,
                vec![artifact("manifest.json")],
            )
            .expect("record")])
            .expect("authority"),
        );
        let event = GenerationAuthorityEvent::replace(
            GenerationWorkflow::Normalize,
            vec![artifact("manifest.json")],
            Vec::new(),
        )
        .expect("event");

        let error = reduce_generation_authority(state, event).expect_err("competing authority");

        assert!(error.contains("already owned"));
    }

    #[test]
    fn empty_generation_is_rejected() {
        assert!(GenerationRecord::new(GenerationWorkflow::Normalize, Vec::new()).is_err());
        assert!(GenerationAuthorityEvent::replace(
            GenerationWorkflow::Normalize,
            Vec::new(),
            Vec::new(),
        )
        .is_err());
    }

    #[test]
    fn authority_path_and_uppercase_digest_are_rejected() {
        assert!(
            GenerationArtifact::new(GENERATION_AUTHORITY_FILE.to_string(), "0".repeat(64), 1,)
                .is_err()
        );
        assert!(GenerationArtifact::new("output.json".to_string(), "A".repeat(64), 1,).is_err());
    }

    #[test]
    fn authority_enforces_aggregate_byte_budget_across_workflows() {
        let each = MAX_GENERATION_BYTES / 2 + 1;
        let normalize = GenerationRecord::new(
            GenerationWorkflow::Normalize,
            vec![artifact_with_bytes("normalize-report.json", each)],
        )
        .expect("individual normalize generation");
        let bundle = GenerationRecord::new(
            GenerationWorkflow::Bundle,
            vec![artifact_with_bytes("manifest.json", each)],
        )
        .expect("individual bundle generation");

        let error = GenerationAuthority::new(vec![normalize, bundle])
            .expect_err("aggregate authority byte budget");

        assert!(error.contains("authority exceeds byte limit"));
    }

    #[test]
    fn authority_json_is_deterministic() {
        let left = GenerationAuthority::new(vec![
            GenerationRecord::new(
                GenerationWorkflow::Bundle,
                vec![artifact("z.png"), artifact("a.json")],
            )
            .expect("bundle"),
            GenerationRecord::new(
                GenerationWorkflow::Normalize,
                vec![artifact("normalize-report.json")],
            )
            .expect("normalize"),
        ])
        .expect("left");
        let right = GenerationAuthority::new(vec![
            GenerationRecord::new(
                GenerationWorkflow::Normalize,
                vec![artifact("normalize-report.json")],
            )
            .expect("normalize"),
            GenerationRecord::new(
                GenerationWorkflow::Bundle,
                vec![artifact("a.json"), artifact("z.png")],
            )
            .expect("bundle"),
        ])
        .expect("right");

        assert_eq!(
            left.to_pretty_json().expect("left json"),
            right.to_pretty_json().expect("right json")
        );
    }
}
