use crate::domain::{BackupProfile, ValidationError};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub arguments: Vec<OsString>,
    pub required_environment: Vec<EnvironmentCredential>,
}

impl CommandSpec {
    pub fn display_for_logs(&self) -> String {
        std::iter::once(self.program.as_os_str())
            .chain(self.arguments.iter().map(OsString::as_os_str))
            .map(|part| part.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvironmentCredential {
    Password,
    EncryptionPassword,
    Fingerprint,
}

impl EnvironmentCredential {
    pub const fn variable_name(self) -> &'static str {
        match self {
            Self::Password => "PBS_PASSWORD",
            Self::EncryptionPassword => "PBS_ENCRYPTION_PASSWORD",
            Self::Fingerprint => "PBS_FINGERPRINT",
        }
    }
}

#[derive(Clone, Debug)]
pub struct PbsClient {
    executable: PathBuf,
}

impl Default for PbsClient {
    fn default() -> Self {
        Self::new("proxmox-backup-client")
    }
}

impl PbsClient {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    pub fn backup(&self, profile: &BackupProfile) -> Result<CommandSpec, ValidationError> {
        profile.validate()?;
        let mut arguments = vec![
            "backup".into(),
            profile.archive_specification().into(),
            "--repository".into(),
            profile.repository.clone().into(),
            "--backup-id".into(),
            profile.backup_id.clone().into(),
            "--change-detection-mode".into(),
            profile.change_detection.as_argument().into(),
        ];
        add_namespace(&mut arguments, profile.namespace.as_deref());
        for exclusion in &profile.exclusions {
            arguments.push("--exclude".into());
            arguments.push(exclusion.into());
        }
        Ok(self.password_command(arguments))
    }

    pub fn snapshots(
        &self,
        repository: &str,
        namespace: Option<&str>,
    ) -> Result<CommandSpec, ValidationError> {
        if repository.trim().is_empty() {
            return Err(ValidationError::EmptyRepository);
        }
        let mut arguments = vec![
            "snapshot".into(),
            "list".into(),
            "--repository".into(),
            repository.into(),
            "--output-format".into(),
            "json".into(),
        ];
        add_namespace(&mut arguments, namespace);
        Ok(self.password_command(arguments))
    }

    pub fn prune(&self, profile: &BackupProfile) -> Result<CommandSpec, ValidationError> {
        profile.validate()?;
        let retention = profile.retention;
        let mut arguments = vec![
            "prune".into(),
            format!("host/{}", profile.backup_id).into(),
            "--repository".into(),
            profile.repository.clone().into(),
            "--keep-last".into(),
            retention.keep_last.to_string().into(),
            "--keep-daily".into(),
            retention.keep_daily.to_string().into(),
            "--keep-weekly".into(),
            retention.keep_weekly.to_string().into(),
            "--keep-monthly".into(),
            retention.keep_monthly.to_string().into(),
            "--keep-yearly".into(),
            retention.keep_yearly.to_string().into(),
        ];
        add_namespace(&mut arguments, profile.namespace.as_deref());
        Ok(self.password_command(arguments))
    }

    pub fn restore(
        &self,
        repository: &str,
        namespace: Option<&str>,
        snapshot: &str,
        archive: &str,
        destination: &Path,
    ) -> Result<CommandSpec, ValidationError> {
        if repository.trim().is_empty() {
            return Err(ValidationError::EmptyRepository);
        }
        let mut arguments = vec![
            "restore".into(),
            snapshot.into(),
            archive.into(),
            destination.as_os_str().to_owned(),
            "--repository".into(),
            repository.into(),
        ];
        add_namespace(&mut arguments, namespace);
        Ok(self.password_command(arguments))
    }

    fn password_command(&self, arguments: Vec<OsString>) -> CommandSpec {
        CommandSpec {
            program: self.executable.clone(),
            arguments,
            required_environment: vec![EnvironmentCredential::Password],
        }
    }
}

fn add_namespace(arguments: &mut Vec<OsString>, namespace: Option<&str>) {
    if let Some(namespace) = namespace.filter(|namespace| !namespace.is_empty()) {
        arguments.push("--ns".into());
        arguments.push(namespace.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ChangeDetection, RetentionPolicy};

    fn profile() -> BackupProfile {
        BackupProfile {
            name: "Home".into(),
            repository: "ada@pbs!crumbs@192.0.2.4:backups".into(),
            namespace: Some("personal/laptop".into()),
            backup_id: "laptop".into(),
            archive_name: "home".into(),
            source: PathBuf::from("/home/ada"),
            exclusions: vec!["/.cache/".into(), "/.local/share/Trash/".into()],
            change_detection: ChangeDetection::Metadata,
            retention: RetentionPolicy::default(),
        }
    }

    fn strings(command: &CommandSpec) -> Vec<String> {
        command
            .arguments
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn backup_is_constructed_without_a_shell() {
        let client = PbsClient::new("proxmox-backup-client");
        let command = client.backup(&profile()).expect("valid command");
        assert_eq!(command.program, PathBuf::from("proxmox-backup-client"));
        assert_eq!(
            strings(&command),
            vec![
                "backup",
                "home.pxar:/home/ada",
                "--repository",
                "ada@pbs!crumbs@192.0.2.4:backups",
                "--backup-id",
                "laptop",
                "--change-detection-mode",
                "metadata",
                "--ns",
                "personal/laptop",
                "--exclude",
                "/.cache/",
                "--exclude",
                "/.local/share/Trash/",
            ]
        );
    }

    #[test]
    fn snapshot_listing_requests_machine_readable_output() {
        let client = PbsClient::new("proxmox-backup-client");
        let command = client
            .snapshots("ada@pbs!crumbs@pbs.example.test:store", None)
            .expect("valid command");
        assert!(strings(&command).ends_with(&[
            "--output-format".into(),
            "json".into()
        ]));
    }

    #[test]
    fn logs_never_contain_secret_values() {
        let client = PbsClient::new("proxmox-backup-client");
        let command = client.backup(&profile()).expect("valid command");
        assert!(!command.display_for_logs().contains("PBS_PASSWORD"));
        assert!(!command.display_for_logs().contains("token-secret"));
    }
}
