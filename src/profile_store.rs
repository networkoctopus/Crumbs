use crate::domain::{BackupProfile, ValidationError};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const CURRENT_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProfilesDocument {
    pub version: u32,
    pub profiles: Vec<BackupProfile>,
}

impl ProfilesDocument {
    pub fn new(profiles: Vec<BackupProfile>) -> Result<Self, StoreError> {
        validate_profiles(&profiles)?;
        Ok(Self {
            version: CURRENT_VERSION,
            profiles,
        })
    }
}

#[derive(Clone, Debug)]
pub struct ProfileStore {
    path: PathBuf,
}

impl ProfileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<ProfilesDocument, StoreError> {
        match fs::read_to_string(&self.path) {
            Ok(contents) => {
                let document: ProfilesDocument = serde_json::from_str(&contents)?;
                validate_profiles(&document.profiles)?;
                Ok(document)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Ok(ProfilesDocument::new(Vec::new())?)
            }
            Err(error) => Err(StoreError::Io(error)),
        }
    }

    pub fn save(&self, document: &ProfilesDocument) -> Result<(), StoreError> {
        validate_profiles(&document.profiles)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary_path = self.path.with_extension("json.tmp");
        let contents = serde_json::to_string_pretty(document)?;
        fs::write(&temporary_path, contents)?;
        fs::rename(temporary_path, &self.path)?;
        Ok(())
    }
}

fn validate_profiles(profiles: &[BackupProfile]) -> Result<(), StoreError> {
    let mut ids = std::collections::BTreeSet::new();
    for profile in profiles {
        profile.validate()?;
        if !ids.insert(profile.id.clone()) {
            return Err(StoreError::DuplicateProfileId(profile.id.clone()));
        }
    }
    Ok(())
}

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    Json(serde_json::Error),
    Validation(ValidationError),
    DuplicateProfileId(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "profile storage I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "profile storage JSON is invalid: {error}"),
            Self::Validation(error) => write!(formatter, "profile is invalid: {error}"),
            Self::DuplicateProfileId(id) => write!(formatter, "profile ID is duplicated: {id}"),
        }
    }
}

impl Error for StoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Validation(error) => Some(error),
            Self::DuplicateProfileId(_) => None,
        }
    }
}

impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<ValidationError> for StoreError {
    fn from(error: ValidationError) -> Self {
        Self::Validation(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        ChangeDetection, EncryptionSettings, RetentionPolicy, default_home_exclusions,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    fn profile(id: &str) -> BackupProfile {
        BackupProfile {
            id: id.into(),
            name: "Home".into(),
            repository: "ada@pbs!crumbs@pbs.example.test:backups".into(),
            namespace: Some("personal/laptop".into()),
            backup_id: "laptop".into(),
            archive_name: "home".into(),
            source: PathBuf::from("/home/ada"),
            sources: Vec::new(),
            exclusions: default_home_exclusions(),
            change_detection: ChangeDetection::Metadata,
            encryption: EncryptionSettings::default(),
            requires_fingerprint: false,
            retention: RetentionPolicy::ServerManaged,
        }
    }

    fn test_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir()
            .join("crumbs-profile-store-tests")
            .join(format!("{name}-{unique}.json"))
    }

    #[test]
    fn missing_store_loads_as_empty_document() {
        let store = ProfileStore::new(test_path("missing"));
        let document = store.load().expect("empty document");
        assert_eq!(document.version, CURRENT_VERSION);
        assert!(document.profiles.is_empty());
    }

    #[test]
    fn saves_and_loads_profiles() {
        let path = test_path("roundtrip");
        let store = ProfileStore::new(&path);
        let document = ProfilesDocument::new(vec![profile("home")]).expect("valid document");
        store.save(&document).expect("save profile");
        let loaded = store.load().expect("load profile");
        assert_eq!(loaded, document);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_duplicate_profile_ids() {
        let error = ProfilesDocument::new(vec![profile("home"), profile("home")])
            .expect_err("duplicate IDs fail");
        assert!(matches!(error, StoreError::DuplicateProfileId(id) if id == "home"));
    }

    #[test]
    fn rejects_invalid_profiles_before_saving() {
        let path = test_path("invalid");
        let store = ProfileStore::new(path);
        let mut profile = profile("home");
        profile.repository.clear();
        let document = ProfilesDocument {
            version: CURRENT_VERSION,
            profiles: vec![profile],
        };
        assert!(matches!(
            store.save(&document),
            Err(StoreError::Validation(_))
        ));
    }
}
