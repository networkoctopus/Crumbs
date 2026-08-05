use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const CURRENT_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AppSettingsDocument {
    pub version: u32,
    pub servers: Vec<StoredServer>,
    pub backups: Vec<StoredBackup>,
}

impl AppSettingsDocument {
    pub fn new(servers: Vec<StoredServer>, backups: Vec<StoredBackup>) -> Result<Self, StoreError> {
        validate_servers(&servers)?;
        validate_backups(&backups)?;
        Ok(Self {
            version: CURRENT_VERSION,
            servers,
            backups,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredServer {
    pub name: String,
    pub repository: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredBackup {
    pub name: String,
    pub server: String,
    pub source: PathBuf,
    pub archive_name: String,
    pub exclusions: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct AppSettingsStore {
    path: PathBuf,
}

impl AppSettingsStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<AppSettingsDocument, StoreError> {
        match fs::read_to_string(&self.path) {
            Ok(contents) => {
                let document: AppSettingsDocument = serde_json::from_str(&contents)?;
                validate_servers(&document.servers)?;
                validate_backups(&document.backups)?;
                Ok(document)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Ok(AppSettingsDocument::new(Vec::new(), Vec::new())?)
            }
            Err(error) => Err(StoreError::Io(error)),
        }
    }

    pub fn save(&self, document: &AppSettingsDocument) -> Result<(), StoreError> {
        validate_servers(&document.servers)?;
        validate_backups(&document.backups)?;
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

fn validate_servers(servers: &[StoredServer]) -> Result<(), StoreError> {
    let mut names = std::collections::BTreeSet::new();
    for server in servers {
        if server.name.trim().is_empty() {
            return Err(StoreError::InvalidServer("server name is required".into()));
        }
        if server.repository.trim().is_empty() {
            return Err(StoreError::InvalidServer(
                "server repository is required".into(),
            ));
        }
        if !names.insert(server.name.clone()) {
            return Err(StoreError::DuplicateServerName(server.name.clone()));
        }
    }
    Ok(())
}

fn validate_backups(backups: &[StoredBackup]) -> Result<(), StoreError> {
    let mut names = std::collections::BTreeSet::new();
    for backup in backups {
        if backup.name.trim().is_empty() {
            return Err(StoreError::InvalidBackup("backup name is required".into()));
        }
        if backup.server.trim().is_empty() {
            return Err(StoreError::InvalidBackup(
                "backup server is required".into(),
            ));
        }
        if backup.archive_name.trim().is_empty() {
            return Err(StoreError::InvalidBackup("archive name is required".into()));
        }
        if !backup.source.is_absolute() {
            return Err(StoreError::InvalidBackup(
                "backup source must be absolute".into(),
            ));
        }
        if !names.insert(backup.name.clone()) {
            return Err(StoreError::DuplicateBackupName(backup.name.clone()));
        }
    }
    Ok(())
}

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    Json(serde_json::Error),
    InvalidServer(String),
    InvalidBackup(String),
    DuplicateServerName(String),
    DuplicateBackupName(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "settings storage I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "settings JSON is invalid: {error}"),
            Self::InvalidServer(error) => write!(formatter, "server settings are invalid: {error}"),
            Self::InvalidBackup(error) => write!(formatter, "backup settings are invalid: {error}"),
            Self::DuplicateServerName(name) => {
                write!(formatter, "server name is duplicated: {name}")
            }
            Self::DuplicateBackupName(name) => {
                write!(formatter, "backup name is duplicated: {name}")
            }
        }
    }
}

impl Error for StoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::InvalidServer(_)
            | Self::InvalidBackup(_)
            | Self::DuplicateServerName(_)
            | Self::DuplicateBackupName(_) => None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        std::env::temp_dir()
            .join("crumbs-app-store-tests")
            .join(format!("{name}-{unique}.json"))
    }

    fn server() -> StoredServer {
        StoredServer {
            name: "PBS".into(),
            repository: "ada@pbs.example.test:backups".into(),
            fingerprint: "aa:bb".into(),
        }
    }

    fn backup() -> StoredBackup {
        StoredBackup {
            name: "laptop".into(),
            server: "PBS".into(),
            source: PathBuf::from("/home/ada"),
            archive_name: "home".into(),
            exclusions: vec!["/.cache/".into()],
        }
    }

    #[test]
    fn missing_store_loads_as_empty_document() {
        let store = AppSettingsStore::new(test_path("missing"));
        let document = store.load().expect("load empty settings");
        assert_eq!(document.version, CURRENT_VERSION);
        assert!(document.servers.is_empty());
        assert!(document.backups.is_empty());
    }

    #[test]
    fn saves_and_loads_settings() {
        let path = test_path("roundtrip");
        let store = AppSettingsStore::new(&path);
        let document = AppSettingsDocument::new(vec![server()], vec![backup()]).unwrap();
        store.save(&document).expect("save settings");
        let loaded = store.load().expect("load settings");
        assert_eq!(loaded, document);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_duplicate_server_names() {
        let error = AppSettingsDocument::new(vec![server(), server()], Vec::new())
            .expect_err("duplicate servers fail");
        assert!(matches!(error, StoreError::DuplicateServerName(name) if name == "PBS"));
    }
}
