use std::error::Error;
use std::fmt;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupProfile {
    pub name: String,
    pub repository: String,
    pub namespace: Option<String>,
    pub backup_id: String,
    pub archive_name: String,
    pub source: PathBuf,
    pub exclusions: Vec<String>,
    pub change_detection: ChangeDetection,
    pub retention: RetentionPolicy,
}

impl BackupProfile {
    pub fn home(repository: impl Into<String>, home: impl Into<PathBuf>) -> Self {
        Self {
            name: "Home".into(),
            repository: repository.into(),
            namespace: None,
            backup_id: "desktop".into(),
            archive_name: "home".into(),
            source: home.into(),
            exclusions: default_home_exclusions(),
            change_detection: ChangeDetection::Metadata,
            retention: RetentionPolicy::default(),
        }
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
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
        if !self.source.is_absolute() {
            return Err(ValidationError::SourceNotAbsolute);
        }
        Ok(())
    }

    pub fn archive_specification(&self) -> String {
        format!("{}.pxar:{}", self.archive_name, self.source.display())
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetentionPolicy {
    pub keep_last: u16,
    pub keep_daily: u16,
    pub keep_weekly: u16,
    pub keep_monthly: u16,
    pub keep_yearly: u16,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            keep_last: 3,
            keep_daily: 7,
            keep_weekly: 4,
            keep_monthly: 6,
            keep_yearly: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScheduleFrequency {
    Hourly,
    Daily,
    Weekly,
    Monthly,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationError {
    EmptyName,
    EmptyRepository,
    InvalidBackupId,
    InvalidArchiveName,
    SourceNotAbsolute,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyName => "the profile name cannot be empty",
            Self::EmptyRepository => "the PBS repository cannot be empty",
            Self::InvalidBackupId => "the backup ID contains unsupported characters",
            Self::InvalidArchiveName => "the archive name contains unsupported characters",
            Self::SourceNotAbsolute => "the backup source must be an absolute path",
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
            name: "Laptop".into(),
            repository: "user@pbs!laptop@pbs.example.test:store".into(),
            namespace: None,
            backup_id: "silver-laptop".into(),
            archive_name: "home".into(),
            source: PathBuf::from("/home/ada"),
            exclusions: default_home_exclusions(),
            change_detection: ChangeDetection::Metadata,
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
}

