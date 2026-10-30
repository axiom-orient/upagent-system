use std::{collections::BTreeSet, fs, io::Read, path::Path};

use agent_core::{CapabilityId, ProfileId, ProfileManifest, SkillId};
use serde::{Deserialize, Serialize};

use super::{
    MAX_DATA_PACKAGE_BYTES, MAX_DATA_PACKAGE_SKILLS, MAX_INSTRUCTION_BYTES, SkillDescriptor,
    SkillDocument, SkillError, SkillManifest, sha256,
};

/// UTF-8 JSON-only tier-2/3 extension package.  There are intentionally no
/// executable, process, Wasm, native, tool, provider, or authority fields in
/// this type; `deny_unknown_fields` makes those declarations fail closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DataSkillPackage {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    pub profiles: BTreeSet<ProfileId>,
    #[serde(default)]
    pub required_capabilities: BTreeSet<CapabilityId>,
    pub skills: Vec<DataSkillEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DataSkillEntry {
    pub id: SkillId,
    pub version: String,
    pub description: String,
    pub profiles: BTreeSet<ProfileId>,
    #[serde(default)]
    pub required_capabilities: BTreeSet<CapabilityId>,
    pub instructions: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataExtensionDocument {
    pub id: String,
    pub source_sha256: String,
    pub skills: Vec<SkillDocument>,
}

/// Load and validate a tier-2/3 data package without executing or discovering
/// any code.  The caller supplies the active profile, so the package cannot
/// widen profile, tool, provider, workflow, effect, or memory authority.
pub fn load_data_extension(
    path: &Path,
    expected_id: &str,
    profile: &ProfileManifest,
    approved_sha256: Option<&str>,
) -> Result<DataExtensionDocument, SkillError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(SkillError::Symlink);
    }
    if !path.is_absolute() {
        return Err(SkillError::InvalidDataPackage(
            "data package path must be absolute".to_owned(),
        ));
    }
    let canonical = path.canonicalize()?;
    if canonical != path {
        return Err(SkillError::Symlink);
    }
    let bytes = read_data_package_once(path, MAX_DATA_PACKAGE_BYTES)
        .map_err(|error| SkillError::InvalidDataPackage(error.to_string()))?;
    let source_sha256 = sha256(&bytes);
    if let Some(approved) = approved_sha256
        && approved != source_sha256
    {
        return Err(SkillError::DataPackageDigestMismatch);
    }
    let package: DataSkillPackage = serde_json::from_slice(&bytes)
        .map_err(|error| SkillError::InvalidDataPackage(error.to_string()))?;
    validate_data_package(&package, expected_id, profile)?;
    let mut skills = Vec::with_capacity(package.skills.len());
    for entry in package.skills {
        let manifest = SkillManifest {
            id: entry.id,
            version: entry.version,
            description: entry.description,
            profiles: entry.profiles,
            required_capabilities: entry.required_capabilities,
            instruction_file: "SKILL.md".to_owned(),
        };
        manifest.validate()?;
        let manifest_bytes = serde_json::to_vec(&manifest)
            .map_err(|error| SkillError::InvalidDataPackage(error.to_string()))?;
        let instructions_sha256 = sha256(entry.instructions.as_bytes());
        skills.push(SkillDocument {
            descriptor: SkillDescriptor {
                manifest,
                manifest_sha256: sha256(&manifest_bytes),
            },
            instructions: entry.instructions,
            instructions_sha256,
        });
    }
    Ok(DataExtensionDocument {
        id: package.id,
        source_sha256,
        skills,
    })
}

fn validate_data_package(
    package: &DataSkillPackage,
    expected_id: &str,
    profile: &ProfileManifest,
) -> Result<(), SkillError> {
    if package.schema_version != 1
        || package.id != expected_id
        || package.id.starts_with("_upagent")
        || package.id.starts_with("kernel/")
        || package.version.trim().is_empty()
        || package.version.len() > 64
        || package.profiles.is_empty()
        || !package.profiles.contains(&profile.id)
        || package.skills.is_empty()
        || package.skills.len() > MAX_DATA_PACKAGE_SKILLS
        || !profile
            .allowed_skill_prefixes
            .iter()
            .any(|prefix| package.id.starts_with(prefix))
        || !package
            .required_capabilities
            .is_subset(&profile.capabilities)
    {
        return Err(SkillError::InvalidDataPackage(
            "package identity, profile namespace, capabilities, or size is invalid".to_owned(),
        ));
    }
    let mut ids = BTreeSet::new();
    for skill in &package.skills {
        if skill.id.as_str().starts_with("_upagent")
            || skill.id.as_str().starts_with("kernel/")
            || !ids.insert(skill.id.clone())
            || !profile.allows_skill(&skill.id)
            || !skill.profiles.contains(&profile.id)
            || !skill.required_capabilities.is_subset(&profile.capabilities)
            || skill.instructions.trim().is_empty()
            || skill.instructions.len()
                > usize::try_from(MAX_INSTRUCTION_BYTES).unwrap_or(usize::MAX)
        {
            return Err(SkillError::InvalidDataPackage(format!(
                "skill {} is outside the active profile authority",
                skill.id
            )));
        }
    }
    Ok(())
}

/// Open the package once with no-follow semantics. The descriptor, not a
/// second pathname lookup, owns the bounded bytes that are hashed and parsed.
fn read_data_package_once(path: &Path, limit: u64) -> Result<Vec<u8>, std::io::Error> {
    #[cfg(unix)]
    let file: fs::File = {
        use rustix::fs::{Mode, OFlags, open};

        let descriptor = open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
        descriptor.into()
    };
    #[cfg(not(unix))]
    let mut file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "data extension package must be a regular file",
        ));
    }
    if metadata.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("data extension package exceeds the {limit} byte limit"),
        ));
    }
    let capacity = usize::try_from(metadata.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "data extension package is too large for this platform",
        )
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("data extension package exceeds the {limit} byte limit"),
        ));
    }
    Ok(bytes)
}
