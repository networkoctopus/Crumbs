use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::num::NonZeroU16;
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BackupProfile {
    pub id: String,
    pub name: String,
    pub repository: String,
    pub namespace: Option<String>,
    pub backup_id: String,
    pub archive_name: String,
    pub source: PathBuf,
    pub exclusions: Vec<String>,
    pub change_detection: ChangeDetection,
    pub encryption: EncryptionSettings,
    pub requires_fingerprint: bool,
    pub retention: RetentionPolicy,
}

impl BackupProfile {
    pub fn home(repository: impl Into<String>, home: impl Into<PathBuf>) -> Self {
        Self {
            id: "home".into(),
            name: "Home".into(),
            repository: repository.into(),
            namespace: None,
            backup_id: "desktop".into(),
            archive_name: "home".into(),
            source: home.into(),
            exclusions: default_home_exclusions(),
            change_detection: ChangeDetection::Metadata,
            encryption: EncryptionSettings::default(),
            requires_fingerprint: false,
            retention: RetentionPolicy::default(),
        }
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if !valid_profile_id(&self.id) {
            return Err(ValidationError::InvalidProfileId);
        }
        if self.name.trim().is_empty() {
            return Err(ValidationError::EmptyName);
        }
        if self.repository.trim().is_empty() {
            return Err(ValidationError::EmptyRepository);
        }
        if !valid_identifier(&self.backup_id) {
            return Err(ValidationError::InvalidBackupId);
        }
        if !valid_identifier(&self.archive_name) {
            return Err(ValidationError::InvalidArchiveName);
        }
        if let Some(namespace) = &self.namespace {
            if namespace.trim() != namespace || namespace.contains("//") {
                return Err(ValidationError::InvalidNamespace);
            }
        }
        if !self.source.is_absolute() {
            return Err(ValidationError::SourceNotAbsolute);
        }
        if let Some(keyfile) = &self.encryption.keyfile {
            if !keyfile.is_absolute() {
                return Err(ValidationError::KeyfileNotAbsolute);
            }
        }
        if let RetentionPolicy::ClientManaged(keep) = self.retention {
            if keep.is_empty() {
                return Err(ValidationError::EmptyRetentionPolicy);
            }
        }
        Ok(())
    }

    pub fn archive_specification(&self) -> String {
        format!("{}.pxar:{}", self.archive_name, self.source.display())
    }
}

fn valid_profile_id(value: &str) -> bool {
    valid_identifier(value)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeDetection {
    Legacy,
    Data,
    #[default]
    Metadata,
}

impl ChangeDetection {
    pub const fn as_argument(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Data => "data",
            Self::Metadata => "metadata",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EncryptionSettings {
    pub crypt_mode: CryptMode,
    pub keyfile: Option<PathBuf>,
    pub key_is_password_protected: bool,
}

impl Default for EncryptionSettings {
    fn default() -> Self {
        Self {
            crypt_mode: CryptMode::Encrypt,
            keyfile: None,
            key_is_password_protected: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CryptMode {
    None,
    Encrypt,
    SignOnly,
}

impl CryptMode {
    pub const fn as_argument(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Encrypt => "encrypt",
            Self::SignOnly => "sign-only",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "mode", content = "keep")]
pub enum RetentionPolicy {
    ServerManaged,
    ClientManaged(RetentionCounts),
    Disabled,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self::ServerManaged
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetentionCounts {
    pub keep_hourly: Option<NonZeroU16>,
    pub keep_last: Option<NonZeroU16>,
    pub keep_daily: Option<NonZeroU16>,
    pub keep_weekly: Option<NonZeroU16>,
    pub keep_monthly: Option<NonZeroU16>,
    pub keep_yearly: Option<NonZeroU16>,
}

impl RetentionCounts {
    pub const fn empty() -> Self {
        Self {
            keep_hourly: None,
            keep_last: None,
            keep_daily: None,
            keep_weekly: None,
            keep_monthly: None,
            keep_yearly: None,
        }
    }

    pub fn desktop_default() -> Self {
        Self {
            keep_hourly: None,
            keep_last: NonZeroU16::new(3),
            keep_daily: NonZeroU16::new(7),
            keep_weekly: NonZeroU16::new(4),
            keep_monthly: NonZeroU16::new(6),
            keep_yearly: NonZeroU16::new(1),
        }
    }

    pub const fn is_empty(self) -> bool {
        self.keep_hourly.is_none()
            && self.keep_last.is_none()
            && self.keep_daily.is_none()
            && self.keep_weekly.is_none()
            && self.keep_monthly.is_none()
            && self.keep_yearly.is_none()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScheduleFrequency {
    Hourly,
    Daily,
    Weekly,
    Monthly,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationError {
    InvalidProfileId,
    EmptyName,
    EmptyRepository,
    InvalidBackupId,
    InvalidArchiveName,
    InvalidNamespace,
    SourceNotAbsolute,
    KeyfileNotAbsolute,
    EmptyRetentionPolicy,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidProfileId => "the profile ID contains unsupported characters",
            Self::EmptyName => "the profile name cannot be empty",
            Self::EmptyRepository => "the PBS repository cannot be empty",
            Self::InvalidBackupId => "the backup ID contains unsupported characters",
            Self::InvalidArchiveName => "the archive name contains unsupported characters",
            Self::InvalidNamespace => "the backup namespace is not valid",
            Self::SourceNotAbsolute => "the backup source must be an absolute path",
            Self::KeyfileNotAbsolute => "the encryption key file must be an absolute path",
            Self::EmptyRetentionPolicy => "client-managed retention needs at least one keep rule",
        })
    }
}

impl Error for ValidationError {}

pub fn default_home_exclusions() -> Vec<String> {
    [
        "/.cache/",
        "/.ccache/",
        "/.local/share/Trash/",
        "/.var/app/*/cache/",
        "/.var/app/*/config/Cache/",
        "/.var/app/*/config/Code Cache/",
        "/.xsession-errors",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_profile() -> BackupProfile {
        BackupProfile {
            id: "laptop".into(),
            name: "Laptop".into(),
            repository: "user@pbs!laptop@pbs.example.test:store".into(),
            namespace: None,
            backup_id: "silver-laptop".into(),
            archive_name: "home".into(),
            source: PathBuf::from("/home/ada"),
            exclusions: default_home_exclusions(),
            change_detection: ChangeDetection::Metadata,
            encryption: EncryptionSettings::default(),
            requires_fingerprint: false,
            retention: RetentionPolicy::default(),
        }
    }

    #[test]
    fn accepts_a_valid_home_profile() {
        assert_eq!(valid_profile().validate(), Ok(()));
    }

    #[test]
    fn rejects_shell_metacharacters_in_archive_names() {
        let mut profile = valid_profile();
        profile.archive_name = "home;rm".into();
        assert_eq!(profile.validate(), Err(ValidationError::InvalidArchiveName));
    }

    #[test]
    fn requires_an_absolute_source() {
        let mut profile = valid_profile();
        profile.source = PathBuf::from("Documents");
        assert_eq!(profile.validate(), Err(ValidationError::SourceNotAbsolute));
    }

    #[test]
    fn rejects_empty_client_managed_retention() {
        let mut profile = valid_profile();
        profile.retention = RetentionPolicy::ClientManaged(RetentionCounts::empty());
        assert_eq!(
            profile.validate(),
            Err(ValidationError::EmptyRetentionPolicy)
        );
    }

    #[test]
    fn server_managed_retention_is_the_default() {
        assert_eq!(RetentionPolicy::default(), RetentionPolicy::ServerManaged);
    }
}
