use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{CapabilityId, ProfileId, SkillId, TrackId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPolicy {
    Disabled,
    Session,
    Project,
    LongTermScoped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    DenyEffects,
    RequireExplicit,
    AllowTaskGrant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ConcurrencyPolicy {
    pub default_workers: u8,
    pub maximum_workers: u8,
    pub recursive_workers: bool,
    pub shared_workspace_writers: u8,
}

impl ConcurrencyPolicy {
    #[must_use]
    pub const fn single() -> Self {
        Self {
            default_workers: 0,
            maximum_workers: 0,
            recursive_workers: false,
            shared_workspace_writers: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProfileManifest {
    pub id: ProfileId,
    pub display_name: String,
    pub description: String,
    #[serde(default)]
    pub allowed_tracks: BTreeSet<TrackId>,
    #[serde(default)]
    pub capabilities: BTreeSet<CapabilityId>,
    #[serde(default)]
    pub default_skills: BTreeSet<SkillId>,
    #[serde(default)]
    pub allowed_skill_prefixes: BTreeSet<String>,
    pub memory_policy: MemoryPolicy,
    pub approval_policy: ApprovalPolicy,
    pub concurrency: ConcurrencyPolicy,
}

impl ProfileManifest {
    pub fn validate(&self) -> Result<(), String> {
        if self.display_name.trim().is_empty() || self.description.trim().is_empty() {
            return Err("profile display name and description are required".to_owned());
        }
        if self.concurrency.default_workers > self.concurrency.maximum_workers {
            return Err("profile default worker count exceeds maximum".to_owned());
        }
        if self.concurrency.maximum_workers > 64 {
            return Err("profile maximum worker count exceeds 64".to_owned());
        }
        if self.concurrency.recursive_workers {
            return Err("recursive worker spawning is prohibited".to_owned());
        }
        if self.concurrency.shared_workspace_writers > 1 {
            return Err("at most one shared-workspace writer is allowed".to_owned());
        }
        if self
            .allowed_skill_prefixes
            .iter()
            .any(|prefix| !valid_skill_prefix(prefix))
        {
            return Err("profile skill prefixes must be bounded namespace prefixes".to_owned());
        }
        Ok(())
    }

    #[must_use]
    pub fn allows_track(&self, track: Option<&TrackId>) -> bool {
        match track {
            None => self.allowed_tracks.is_empty(),
            Some(value) => self.allowed_tracks.contains(value),
        }
    }

    #[must_use]
    pub fn allows_skill(&self, skill: &SkillId) -> bool {
        self.default_skills.contains(skill)
            || self
                .allowed_skill_prefixes
                .iter()
                .any(|prefix| skill.as_str().starts_with(prefix))
    }
}

fn valid_skill_prefix(prefix: &str) -> bool {
    let Some(namespace) = prefix.strip_suffix('/') else {
        return false;
    };
    !namespace.is_empty()
        && prefix.len() <= 160
        && !namespace.starts_with('/')
        && namespace.split('/').all(|segment| {
            !segment.is_empty()
                && !matches!(segment, "." | "..")
                && segment.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(prefix: &str) -> ProfileManifest {
        ProfileManifest {
            id: ProfileId::creator(),
            display_name: "Creator".to_owned(),
            description: "Test profile".to_owned(),
            allowed_tracks: BTreeSet::new(),
            capabilities: BTreeSet::new(),
            default_skills: BTreeSet::new(),
            allowed_skill_prefixes: BTreeSet::from([prefix.to_owned()]),
            memory_policy: MemoryPolicy::Session,
            approval_policy: ApprovalPolicy::DenyEffects,
            concurrency: ConcurrencyPolicy::single(),
        }
    }

    #[test]
    fn skill_prefixes_are_closed_namespaces() {
        assert!(manifest("creator/").validate().is_ok());
        for invalid in [
            "",
            "creator",
            "/creator/",
            "creator//nested/",
            "creator/../",
            "./creator/",
        ] {
            assert!(manifest(invalid).validate().is_err(), "accepted {invalid}");
        }
    }
}
