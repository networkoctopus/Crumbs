use secret_service::{EncryptionType, blocking::SecretService};
use std::collections::HashMap;
use std::error::Error;
use std::fmt;

const APPLICATION: &str = "io.github.networkoctopus.Crumbs";
const KIND_PASSWORD: &str = "pbs-password";

#[derive(Clone, Debug, Default)]
pub struct SecretStore;

impl SecretStore {
    pub fn new() -> Self {
        Self
    }

    pub fn store_pbs_password(
        &self,
        server_name: &str,
        repository: &str,
        password: &str,
    ) -> Result<(), SecretError> {
        if password.is_empty() {
            return Ok(());
        }
        let service = SecretService::connect(EncryptionType::Dh)?;
        let collection = service.get_default_collection()?;
        collection.ensure_unlocked()?;
        let label = format!("Crumbs password for {server_name}");
        let attributes = password_attributes(server_name, repository);
        let existing_items = collection.search_items(attributes.clone())?;
        if let Some(item) = existing_items.first() {
            item.ensure_unlocked()?;
            item.set_label(&label)?;
            item.set_secret(password.as_bytes(), "text/plain")?;
        } else {
            collection.create_item(&label, attributes, password.as_bytes(), true, "text/plain")?;
        }
        Ok(())
    }

    pub fn get_pbs_password(
        &self,
        server_name: &str,
        repository: &str,
    ) -> Result<Option<String>, SecretError> {
        let service = SecretService::connect(EncryptionType::Dh)?;
        let search = service.search_items(password_attributes(server_name, repository))?;
        let mut items = search.unlocked;
        if items.is_empty() && !search.locked.is_empty() {
            let locked_refs = search.locked.iter().collect::<Vec<_>>();
            service.unlock_all(&locked_refs)?;
            items = service
                .search_items(password_attributes(server_name, repository))?
                .unlocked;
        }
        if let Some(item) = items.first() {
            let secret = item.get_secret()?;
            return String::from_utf8(secret)
                .map(Some)
                .map_err(|error| SecretError::Utf8(error.to_string()));
        }
        Ok(None)
    }

    pub fn delete_pbs_password(
        &self,
        server_name: &str,
        repository: &str,
    ) -> Result<(), SecretError> {
        let service = SecretService::connect(EncryptionType::Dh)?;
        let search = service.search_items(password_attributes(server_name, repository))?;
        for item in search.unlocked.into_iter().chain(search.locked.into_iter()) {
            item.delete()?;
        }
        Ok(())
    }
}

fn password_attributes<'a>(server_name: &'a str, repository: &'a str) -> HashMap<&'a str, &'a str> {
    HashMap::from([
        ("application", APPLICATION),
        ("kind", KIND_PASSWORD),
        ("server-name", server_name),
        ("repository", repository),
    ])
}

#[derive(Debug)]
pub enum SecretError {
    Service(secret_service::Error),
    Utf8(String),
}

impl fmt::Display for SecretError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Service(error) => write!(formatter, "Secret Service failed: {error}"),
            Self::Utf8(error) => write!(formatter, "stored secret is not valid text: {error}"),
        }
    }
}

impl Error for SecretError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Service(error) => Some(error),
            Self::Utf8(_) => None,
        }
    }
}

impl From<secret_service::Error> for SecretError {
    fn from(error: secret_service::Error) -> Self {
        Self::Service(error)
    }
}
