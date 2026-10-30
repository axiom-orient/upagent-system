#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[cfg(test)]
use agent_core::ProfileManifest;
use agent_core::{CapabilityId, ProfileId, SkillId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

mod data_extension;

pub use data_extension::{
    DataExtensionDocument, DataSkillEntry, DataSkillPackage, load_data_extension,
};

const MAX_SKILLS: usize = 1_024;
const MAX_SCAN_ENTRIES: usize = 100_000;
const MAX_SCAN_DEPTH: usize = 4;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_INSTRUCTION_BYTES: u64 = 256 * 1024;
const MAX_DATA_PACKAGE_BYTES: u64 = 512 * 1024;
const MAX_DATA_PACKAGE_SKILLS: usize = 64;

#[derive(Debug, Error)]
pub enum SkillError {
    #[error("skill I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("skill manifest is invalid: {0}")]
    InvalidManifest(String),
    #[error("skill catalog contains duplicate ID {0}")]
    Duplicate(SkillId),
    #[error("skill {0} was not found")]
    NotFound(SkillId),
    #[error("skill {skill} is not allowed for profile {profile}")]
    ProfileDenied { skill: SkillId, profile: ProfileId },
    #[error("skill path is outside its catalog root")]
    EscapedRoot,
    #[error("symbolic links are not accepted in skill catalogs")]
    Symlink,
    #[error("skill catalog exceeded its bounded size")]
    CatalogTooLarge,
    #[error("data extension package is invalid: {0}")]
    InvalidDataPackage(String),
    #[error("data extension package digest does not match the approved digest")]
    DataPackageDigestMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SkillManifest {
    pub id: SkillId,
    pub version: String,
    pub description: String,
    pub profiles: BTreeSet<ProfileId>,
    #[serde(default)]
    pub required_capabilities: BTreeSet<CapabilityId>,
    #[serde(default = "default_instruction_file")]
    pub instruction_file: String,
}

fn default_instruction_file() -> String {
    "SKILL.md".to_owned()
}

impl SkillManifest {
    fn validate(&self) -> Result<(), SkillError> {
        if self.version.trim().is_empty()
            || self.version.len() > 64
            || self.description.trim().is_empty()
            || self.description.len() > 4_096
            || self.profiles.is_empty()
            || self.instruction_file != "SKILL.md"
        {
            return Err(SkillError::InvalidManifest(
                "version, description, profiles, or instruction file is invalid".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct SkillEntry {
    root: PathBuf,
    directory: PathBuf,
    manifest: SkillManifest,
    manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkillDescriptor {
    pub manifest: SkillManifest,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDocument {
    pub descriptor: SkillDescriptor,
    pub instructions: String,
    pub instructions_sha256: String,
}

#[derive(Debug, Clone, Default)]
pub struct SkillCatalog {
    entries: BTreeMap<SkillId, SkillEntry>,
    data_entries: BTreeMap<SkillId, SkillDocument>,
}

impl SkillCatalog {
    pub fn discover(roots: impl IntoIterator<Item = PathBuf>) -> Result<Self, SkillError> {
        Self::discover_with_limit(roots, MAX_SCAN_ENTRIES)
    }

    fn discover_with_limit(
        roots: impl IntoIterator<Item = PathBuf>,
        maximum_scan_entries: usize,
    ) -> Result<Self, SkillError> {
        let mut entries = BTreeMap::new();
        let mut scanned_entries = 0_usize;
        for root in roots {
            discover_root(
                &root,
                &mut entries,
                &mut scanned_entries,
                maximum_scan_entries,
            )?;
        }
        Ok(Self {
            entries,
            data_entries: BTreeMap::new(),
        })
    }

    #[must_use]
    pub fn descriptors(&self) -> Vec<SkillDescriptor> {
        let mut descriptors = self
            .entries
            .values()
            .map(|entry| SkillDescriptor {
                manifest: entry.manifest.clone(),
                manifest_sha256: entry.manifest_sha256.clone(),
            })
            .collect::<Vec<_>>();
        descriptors.extend(
            self.data_entries
                .values()
                .map(|document| document.descriptor.clone()),
        );
        descriptors.sort_by(|left, right| left.manifest.id.cmp(&right.manifest.id));
        descriptors
    }

    pub fn load(&self, id: &SkillId, profile: &ProfileId) -> Result<SkillDocument, SkillError> {
        if let Some(document) = self.data_entries.get(id) {
            if !document.descriptor.manifest.profiles.contains(profile) {
                return Err(SkillError::ProfileDenied {
                    skill: id.clone(),
                    profile: profile.clone(),
                });
            }
            return Ok(document.clone());
        }
        let entry = self
            .entries
            .get(id)
            .ok_or_else(|| SkillError::NotFound(id.clone()))?;
        if !entry.manifest.profiles.contains(profile) {
            return Err(SkillError::ProfileDenied {
                skill: id.clone(),
                profile: profile.clone(),
            });
        }
        let instruction_path = entry.directory.join(&entry.manifest.instruction_file);
        ensure_regular_beneath(&entry.root, &instruction_path)?;
        let bytes = read_bounded(&instruction_path, MAX_INSTRUCTION_BYTES)?;
        let instructions = String::from_utf8(bytes.clone()).map_err(|_| {
            SkillError::InvalidManifest("skill instructions must be UTF-8".to_owned())
        })?;
        if instructions.trim().is_empty() {
            return Err(SkillError::InvalidManifest(
                "skill instructions must not be empty".to_owned(),
            ));
        }
        Ok(SkillDocument {
            descriptor: SkillDescriptor {
                manifest: entry.manifest.clone(),
                manifest_sha256: entry.manifest_sha256.clone(),
            },
            instructions,
            instructions_sha256: sha256(&bytes),
        })
    }

    /// Merge data-only skill documents into an already discovered immutable
    /// catalog snapshot. Duplicate IDs fail closed and no filesystem path is
    /// retained, so later runtime loads use the exact bytes resolved by
    /// `agent compose`.
    pub fn merge_data_documents(
        &mut self,
        documents: Vec<SkillDocument>,
    ) -> Result<(), SkillError> {
        for document in documents {
            let id = document.descriptor.manifest.id.clone();
            if self.entries.contains_key(&id) || self.data_entries.contains_key(&id) {
                return Err(SkillError::Duplicate(id));
            }
            self.data_entries.insert(id, document);
        }
        Ok(())
    }
}

fn discover_root(
    root: &Path,
    entries: &mut BTreeMap<SkillId, SkillEntry>,
    scanned_entries: &mut usize,
    maximum_scan_entries: usize,
) -> Result<(), SkillError> {
    if fs::symlink_metadata(root)?.file_type().is_symlink() {
        return Err(SkillError::Symlink);
    }
    let root = root.canonicalize()?;
    let mut queue = VecDeque::from([(root.clone(), 0_usize)]);
    while let Some((directory, depth)) = queue.pop_front() {
        if depth > MAX_SCAN_DEPTH {
            continue;
        }
        let mut children = Vec::new();
        for child in fs::read_dir(&directory)? {
            *scanned_entries = scanned_entries
                .checked_add(1)
                .ok_or(SkillError::CatalogTooLarge)?;
            if *scanned_entries > maximum_scan_entries {
                return Err(SkillError::CatalogTooLarge);
            }
            children.push(child?);
        }
        children.sort_by_key(fs::DirEntry::file_name);
        for child in children {
            let path = child.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(SkillError::Symlink);
            }
            if metadata.is_dir() {
                queue.push_back((path, depth + 1));
                continue;
            }
            if child.file_name() != std::ffi::OsStr::new("skill.json") {
                continue;
            }
            if entries.len() >= MAX_SKILLS {
                return Err(SkillError::CatalogTooLarge);
            }
            ensure_regular_beneath(&root, &path)?;
            let bytes = read_bounded(&path, MAX_MANIFEST_BYTES)?;
            let manifest: SkillManifest = serde_json::from_slice(&bytes)
                .map_err(|error| SkillError::InvalidManifest(error.to_string()))?;
            manifest.validate()?;
            let directory = path.parent().ok_or(SkillError::EscapedRoot)?.to_path_buf();
            let entry = SkillEntry {
                root: root.clone(),
                directory,
                manifest: manifest.clone(),
                manifest_sha256: sha256(&bytes),
            };
            if entries.insert(manifest.id.clone(), entry).is_some() {
                return Err(SkillError::Duplicate(manifest.id));
            }
        }
    }
    Ok(())
}

fn ensure_regular_beneath(root: &Path, path: &Path) -> Result<(), SkillError> {
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(root) {
        return Err(SkillError::EscapedRoot);
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(SkillError::Symlink);
    }
    Ok(())
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, SkillError> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > limit {
        return Err(SkillError::InvalidManifest(format!(
            "{} exceeds the {} byte limit",
            path.display(),
            limit
        )));
    }
    let file = fs::File::open(path)?;
    let capacity = usize::try_from(metadata.len()).map_err(|_| {
        SkillError::InvalidManifest(format!("{} is too large for this platform", path.display()))
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take(limit + 1).read_to_end(&mut bytes)?;
    let byte_length = u64::try_from(bytes.len()).map_err(|_| {
        SkillError::InvalidManifest(format!("{} length exceeds u64", path.display()))
    })?;
    if byte_length > limit {
        return Err(SkillError::InvalidManifest(format!(
            "{} exceeds the {} byte limit",
            path.display(),
            limit
        )));
    }
    Ok(bytes)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use agent_core::{ApprovalPolicy, ConcurrencyPolicy, MemoryPolicy};

    use super::*;

    fn creator_profile() -> ProfileManifest {
        ProfileManifest {
            id: ProfileId::creator(),
            display_name: "Creator".to_owned(),
            description: "test creator".to_owned(),
            allowed_tracks: BTreeSet::new(),
            capabilities: BTreeSet::new(),
            default_skills: BTreeSet::new(),
            allowed_skill_prefixes: BTreeSet::from(["creator/".to_owned()]),
            memory_policy: MemoryPolicy::Session,
            approval_policy: ApprovalPolicy::AllowTaskGrant,
            concurrency: ConcurrencyPolicy::single(),
        }
    }

    fn package_json(value: &serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&value).expect("package JSON")
    }

    fn valid_package() -> serde_json::Value {
        serde_json::json!({
            "schemaVersion": 1,
            "id": "creator/pack",
            "version": "1",
            "profiles": ["creator"],
            "requiredCapabilities": [],
            "skills": [{
                "id": "creator/pack-note",
                "version": "1",
                "description": "data skill",
                "profiles": ["creator"],
                "requiredCapabilities": [],
                "instructions": "Use the exact supplied facts."
            }]
        })
    }

    #[test]
    fn catalog_loads_only_profile_allowed_skill() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let skill = temp.path().join("creator/chat");
        fs::create_dir_all(&skill)?;
        fs::write(
            skill.join("skill.json"),
            r#"{"id":"creator/chat","version":"1","description":"chat","profiles":["creator"]}"#,
        )?;
        fs::write(skill.join("SKILL.md"), "Answer directly.")?;
        let catalog = SkillCatalog::discover([temp.path().to_path_buf()])?;
        let document = catalog.load(&SkillId::parse("creator/chat")?, &ProfileId::creator())?;
        assert_eq!(document.instructions, "Answer directly.");
        let denied = catalog.load(&SkillId::parse("creator/chat")?, &ProfileId::developer());
        assert!(matches!(denied, Err(SkillError::ProfileDenied { .. })));
        Ok(())
    }

    #[test]
    fn scan_limit_is_shared_across_all_catalog_roots() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let first = temp.path().join("first/skill");
        let second = temp.path().join("second/skill");
        for (directory, id) in [(&first, "creator/first"), (&second, "creator/second")] {
            fs::create_dir_all(directory)?;
            fs::write(
                directory.join("skill.json"),
                format!(
                    r#"{{"id":"{id}","version":"1","description":"skill","profiles":["creator"]}}"#
                ),
            )?;
        }
        let error = SkillCatalog::discover_with_limit(
            [temp.path().join("first"), temp.path().join("second")],
            3,
        )
        .expect_err("the aggregate scan must exceed the shared limit");
        assert!(matches!(error, SkillError::CatalogTooLarge));
        Ok(())
    }

    #[test]
    fn data_extension_loads_normal_package_and_binds_source_digest()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("pack.json");
        fs::write(&path, package_json(&valid_package()))?;
        let path = path.canonicalize()?;
        let document = load_data_extension(&path, "creator/pack", &creator_profile(), None)?;
        assert_eq!(document.id, "creator/pack");
        assert_eq!(
            document.skills[0].descriptor.manifest.id.as_str(),
            "creator/pack-note"
        );
        assert_eq!(document.source_sha256, sha256(&fs::read(path)?));
        Ok(())
    }

    #[test]
    fn approved_data_extension_rejects_source_digest_drift()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("pack.json");
        fs::write(&path, package_json(&valid_package()))?;
        let path = path.canonicalize()?;
        let error = load_data_extension(
            &path,
            "creator/pack",
            &creator_profile(),
            Some(&"0".repeat(64)),
        )
        .expect_err("approved digest drift must fail");
        assert!(matches!(error, SkillError::DataPackageDigestMismatch));
        Ok(())
    }

    #[test]
    fn data_extension_rejects_authority_and_namespace_widening()
    -> Result<(), Box<dyn std::error::Error>> {
        let profile = creator_profile();
        let temp = tempfile::tempdir()?;
        let cases = [
            serde_json::json!({"tools": ["write_file"]}),
            serde_json::json!({"id": "developer/pack"}),
            serde_json::json!({"requiredCapabilities": ["repo/read"]}),
            serde_json::json!({"skills": [{
                "id": "developer/pack-note",
                "version": "1",
                "description": "widened",
                "profiles": ["creator"],
                "requiredCapabilities": [],
                "instructions": "not allowed"
            }]}),
        ];
        for (index, override_value) in cases.into_iter().enumerate() {
            let mut value = valid_package();
            if let Some(object) = override_value.as_object() {
                for (key, entry) in object {
                    value[key] = entry.clone();
                }
            }
            let path = temp.path().join(format!("case-{index}.json"));
            fs::write(&path, package_json(&value))?;
            assert!(
                load_data_extension(&path, "creator/pack", &profile, None).is_err(),
                "authority widening case {index} must fail"
            );
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn data_extension_rejects_symlink_source() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let real = temp.path().join("real.json");
        let link = temp.path().join("link.json");
        fs::write(&real, package_json(&valid_package()))?;
        symlink(&real, &link)?;
        assert!(matches!(
            load_data_extension(&link, "creator/pack", &creator_profile(), None),
            Err(SkillError::Symlink)
        ));
        Ok(())
    }
}
