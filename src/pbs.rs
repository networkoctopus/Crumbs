use crate::domain::{BackupProfile, RetentionCounts, RetentionPolicy, ValidationError};
use std::ffi::OsString;
use std::num::NonZeroU16;
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
            "--backup-type".into(),
            "host".into(),
            "--crypt-mode".into(),
            profile.encryption.crypt_mode.as_argument().into(),
            "--change-detection-mode".into(),
            profile.change_detection.as_argument().into(),
        ];
        if let Some(keyfile) = &profile.encryption.keyfile {
            arguments.push("--keyfile".into());
            arguments.push(keyfile.as_os_str().to_owned());
        }
        add_namespace(&mut arguments, profile.namespace.as_deref());
        for exclusion in &profile.exclusions {
            arguments.push("--exclude".into());
            arguments.push(exclusion.into());
        }
        Ok(self.profile_command(arguments, profile))
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

    pub fn snapshot_files(
        &self,
        repository: &str,
        namespace: Option<&str>,
        snapshot: &str,
    ) -> Result<CommandSpec, ValidationError> {
        if repository.trim().is_empty() {
            return Err(ValidationError::EmptyRepository);
        }
        let mut arguments = vec![
            "snapshot".into(),
            "files".into(),
            snapshot.into(),
            "--repository".into(),
            repository.into(),
            "--output-format".into(),
            "json".into(),
        ];
        add_namespace(&mut arguments, namespace);
        Ok(self.password_command(arguments))
    }

    pub fn status(&self, repository: &str) -> Result<CommandSpec, ValidationError> {
        if repository.trim().is_empty() {
            return Err(ValidationError::EmptyRepository);
        }
        Ok(self.password_command(vec![
            "status".into(),
            "--repository".into(),
            repository.into(),
            "--output-format".into(),
            "json".into(),
        ]))
    }

    pub fn version(&self, repository: Option<&str>) -> Result<CommandSpec, ValidationError> {
        let mut arguments = vec!["version".into(), "--output-format".into(), "json".into()];
        if let Some(repository) = repository.filter(|repository| !repository.trim().is_empty()) {
            arguments.push("--repository".into());
            arguments.push(repository.into());
            Ok(self.password_command(arguments))
        } else {
            Ok(CommandSpec {
                program: self.executable.clone(),
                arguments,
                required_environment: Vec::new(),
            })
        }
    }

    pub fn prune(&self, profile: &BackupProfile) -> Result<Option<CommandSpec>, ValidationError> {
        profile.validate()?;
        let RetentionPolicy::ClientManaged(retention) = profile.retention else {
            return Ok(None);
        };

        let mut arguments = vec![
            "prune".into(),
            format!("host/{}", profile.backup_id).into(),
            "--repository".into(),
            profile.repository.clone().into(),
        ];
        add_retention(&mut arguments, retention);
        add_namespace(&mut arguments, profile.namespace.as_deref());
        Ok(Some(self.profile_command(arguments, profile)))
    }

    pub fn restore(
        &self,
        repository: &str,
        namespace: Option<&str>,
        snapshot: &str,
        archive: &str,
        destination: &Path,
    ) -> Result<CommandSpec, ValidationError> {
        self.restore_with_patterns(repository, namespace, snapshot, archive, destination, &[])
    }

    pub fn restore_with_patterns(
        &self,
        repository: &str,
        namespace: Option<&str>,
        snapshot: &str,
        archive: &str,
        destination: &Path,
        patterns: &[String],
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
            "--ignore-ownership".into(),
            "true".into(),
        ];
        for pattern in patterns {
            arguments.push("--pattern".into());
            arguments.push(pattern.into());
        }
        add_namespace(&mut arguments, namespace);
        Ok(self.password_command(arguments))
    }

    fn profile_command(&self, arguments: Vec<OsString>, profile: &BackupProfile) -> CommandSpec {
        let mut required_environment = vec![EnvironmentCredential::Password];
        if profile.requires_fingerprint {
            required_environment.push(EnvironmentCredential::Fingerprint);
        }
        if profile.encryption.key_is_password_protected {
            required_environment.push(EnvironmentCredential::EncryptionPassword);
        }
        CommandSpec {
            program: self.executable.clone(),
            arguments,
            required_environment,
        }
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

fn add_retention(arguments: &mut Vec<OsString>, retention: RetentionCounts) {
    add_keep(arguments, "--keep-hourly", retention.keep_hourly);
    add_keep(arguments, "--keep-last", retention.keep_last);
    add_keep(arguments, "--keep-daily", retention.keep_daily);
    add_keep(arguments, "--keep-weekly", retention.keep_weekly);
    add_keep(arguments, "--keep-monthly", retention.keep_monthly);
    add_keep(arguments, "--keep-yearly", retention.keep_yearly);
}

fn add_keep(arguments: &mut Vec<OsString>, option: &str, count: Option<NonZeroU16>) {
    if let Some(count) = count {
        arguments.push(option.into());
        arguments.push(count.to_string().into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ChangeDetection, EncryptionSettings, RetentionCounts};

    fn profile() -> BackupProfile {
        BackupProfile {
            id: "home".into(),
            name: "Home".into(),
            repository: "ada@pbs!crumbs@192.0.2.4:backups".into(),
            namespace: Some("personal/laptop".into()),
            backup_id: "laptop".into(),
            archive_name: "home".into(),
            source: PathBuf::from("/home/ada"),
            exclusions: vec!["/.cache/".into(), "/.local/share/Trash/".into()],
            change_detection: ChangeDetection::Metadata,
            encryption: EncryptionSettings::default(),
            requires_fingerprint: false,
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
                "--backup-type",
                "host",
                "--crypt-mode",
                "encrypt",
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
    fn profile_can_require_fingerprint_environment() {
        let client = PbsClient::new("proxmox-backup-client");
        let mut profile = profile();
        profile.requires_fingerprint = true;
        let command = client.backup(&profile).expect("valid command");
        assert!(
            command
                .required_environment
                .contains(&EnvironmentCredential::Fingerprint)
        );
    }

    #[test]
    fn protected_key_requires_encryption_password() {
        let client = PbsClient::new("proxmox-backup-client");
        let mut profile = profile();
        profile.encryption.key_is_password_protected = true;
        let command = client.backup(&profile).expect("valid command");
        assert_eq!(
            command.required_environment,
            vec![
                EnvironmentCredential::Password,
                EnvironmentCredential::EncryptionPassword
            ]
        );
    }

    #[test]
    fn server_managed_retention_does_not_emit_prune_command() {
        let client = PbsClient::new("proxmox-backup-client");
        assert_eq!(client.prune(&profile()).expect("valid profile"), None);
    }

    #[test]
    fn client_managed_prune_only_emits_enabled_keep_rules() {
        let client = PbsClient::new("proxmox-backup-client");
        let mut profile = profile();
        profile.retention = RetentionPolicy::ClientManaged(RetentionCounts {
            keep_hourly: None,
            keep_last: NonZeroU16::new(3),
            keep_daily: None,
            keep_weekly: NonZeroU16::new(4),
            keep_monthly: None,
            keep_yearly: None,
        });
        let command = client
            .prune(&profile)
            .expect("valid profile")
            .expect("client prune command");
        assert_eq!(
            strings(&command),
            vec![
                "prune",
                "host/laptop",
                "--repository",
                "ada@pbs!crumbs@192.0.2.4:backups",
                "--keep-last",
                "3",
                "--keep-weekly",
                "4",
                "--ns",
                "personal/laptop",
            ]
        );
    }

    #[test]
    fn snapshot_listing_requests_machine_readable_output() {
        let client = PbsClient::new("proxmox-backup-client");
        let command = client
            .snapshots("ada@pbs!crumbs@pbs.example.test:store", None)
            .expect("valid command");
        assert!(strings(&command).ends_with(&["--output-format".into(), "json".into()]));
    }

    #[test]
    fn snapshot_files_request_machine_readable_output() {
        let client = PbsClient::new("proxmox-backup-client");
        let command = client
            .snapshot_files(
                "ada@pbs!crumbs@pbs.example.test:store",
                Some("personal/laptop"),
                "host/laptop/2026-08-04T12:00:00Z",
            )
            .expect("valid command");
        assert_eq!(
            strings(&command),
            vec![
                "snapshot",
                "files",
                "host/laptop/2026-08-04T12:00:00Z",
                "--repository",
                "ada@pbs!crumbs@pbs.example.test:store",
                "--output-format",
                "json",
                "--ns",
                "personal/laptop",
            ]
        );
    }

    #[test]
    fn restore_can_limit_files_with_patterns() {
        let client = PbsClient::new("proxmox-backup-client");
        let command = client
            .restore_with_patterns(
                "ada@pbs!crumbs@pbs.example.test:store",
                None,
                "host/laptop/2026-08-04T12:00:00Z",
                "home.pxar",
                Path::new("/tmp/restore"),
                &["Documents/**/*.pdf".into()],
            )
            .expect("valid command");
        assert_eq!(
            strings(&command),
            vec![
                "restore",
                "host/laptop/2026-08-04T12:00:00Z",
                "home.pxar",
                "/tmp/restore",
                "--repository",
                "ada@pbs!crumbs@pbs.example.test:store",
                "--ignore-ownership",
                "true",
                "--pattern",
                "Documents/**/*.pdf",
            ]
        );
    }

    #[test]
    fn logs_never_contain_secret_values() {
        let client = PbsClient::new("proxmox-backup-client");
        let command = client.backup(&profile()).expect("valid command");
        assert!(!command.display_for_logs().contains("PBS_PASSWORD"));
        assert!(!command.display_for_logs().contains("token-secret"));
    }
}
