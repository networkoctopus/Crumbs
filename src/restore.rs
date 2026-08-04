use serde::Deserialize;
use std::fmt;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct SnapshotFile {
    #[serde(rename = "filename")]
    pub name: String,
    #[serde(default, rename = "crypt-mode")]
    pub crypt_mode: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct SnapshotSummary {
    #[serde(rename = "backup-id")]
    pub backup_id: String,
    #[serde(rename = "backup-time")]
    pub backup_time: u64,
    #[serde(rename = "backup-type")]
    pub backup_type: String,
    #[serde(default)]
    pub files: Vec<SnapshotFile>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}

impl SnapshotSummary {
    pub fn path(&self) -> String {
        format!(
            "{}/{}/{}",
            self.backup_type,
            self.backup_id,
            unix_time_to_pbs_time(self.backup_time)
        )
    }

    pub fn title(&self) -> String {
        format!(
            "{} - {}",
            self.backup_id,
            unix_time_to_display(self.backup_time)
        )
    }
}

#[derive(Debug)]
pub enum RestoreParseError {
    Json(serde_json::Error),
}

impl fmt::Display for RestoreParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "could not parse PBS restore JSON: {error}"),
        }
    }
}

impl std::error::Error for RestoreParseError {}

impl From<serde_json::Error> for RestoreParseError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub fn parse_snapshot_list(json: &str) -> Result<Vec<SnapshotSummary>, RestoreParseError> {
    let mut snapshots: Vec<SnapshotSummary> = serde_json::from_str(json)?;
    snapshots.sort_by_key(|snapshot| std::cmp::Reverse(snapshot.backup_time));
    Ok(snapshots)
}

pub fn parse_snapshot_files(json: &str) -> Result<Vec<SnapshotFile>, RestoreParseError> {
    let mut files: Vec<SnapshotFile> = serde_json::from_str(json)?;
    files.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(files)
}

fn unix_time_to_pbs_time(timestamp: u64) -> String {
    // PBS snapshot paths are UTC ISO-like timestamps. Keeping the conversion in
    // a tiny UTC implementation avoids pulling in a date-time dependency for now.
    let days = (timestamp / 86_400) as i64;
    let seconds = timestamp % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = seconds / 3_600;
    let minute = (seconds % 3_600) / 60;
    let second = seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn unix_time_to_display(timestamp: u64) -> String {
    unix_time_to_pbs_time(timestamp).replace('T', " ")
}

fn civil_from_days(days_since_unix_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    (year, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_sorts_snapshot_list() {
        let snapshots = parse_snapshot_list(
            r#"[
                {"backup-id":"laptop","backup-time":1785851641,"backup-type":"host","files":[]},
                {"backup-id":"laptop","backup-time":1785851658,"backup-type":"host","files":[]}
            ]"#,
        )
        .expect("snapshots");
        assert_eq!(snapshots[0].backup_time, 1785851658);
        assert_eq!(snapshots[0].path(), "host/laptop/2026-08-04T13:54:18Z");
    }

    #[test]
    fn parses_snapshot_files() {
        let files = parse_snapshot_files(
            r#"[
                {"crypt-mode":"none","filename":"home.ppxar.didx","size":70},
                {"crypt-mode":"none","filename":"home.mpxar.didx","size":404}
            ]"#,
        )
        .expect("files");
        assert_eq!(files[0].name, "home.mpxar.didx");
        assert_eq!(files[1].crypt_mode.as_deref(), Some("none"));
    }
}
