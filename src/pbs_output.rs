#[derive(Clone, Debug, Default, PartialEq)]
pub struct BackupActivity {
    pub phase: String,
    pub snapshot: Option<String>,
    pub processed: Option<ByteSize>,
    pub uploaded: Option<ByteSize>,
    pub archive_done: Option<ByteSize>,
    pub archive_total: Option<ByteSize>,
    pub total_files: Option<u64>,
    pub hardlinks: Option<u64>,
    pub unchanged_files: Option<u64>,
    pub unchanged_data: Option<ByteSize>,
    pub changed_files: Option<u64>,
    pub changed_data: Option<ByteSize>,
    pub padding: Option<ByteSize>,
    pub partially_reused_chunks: Option<u64>,
    pub reused_data: Option<ByteSize>,
    pub reused_percent: Option<f64>,
    pub duration: Option<String>,
    pub end_time: Option<String>,
    pub warnings: u32,
    pub dry_run: bool,
}

impl BackupActivity {
    pub fn new() -> Self {
        Self {
            phase: "Ready".into(),
            ..Self::default()
        }
    }

    pub fn apply_line(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        if let Some(snapshot) = line.strip_prefix("Starting backup: ") {
            self.snapshot = Some(snapshot.trim().to_owned());
            self.phase = "Starting backup".into();
            return;
        }
        if line.starts_with("Starting backup protocol:") {
            self.phase = "Connecting to PBS".into();
            return;
        }
        if line.starts_with("No previous manifest available") {
            self.phase = "Preparing first backup".into();
            return;
        }
        if line.starts_with("Downloading previous manifest") {
            self.phase = "Comparing with previous backup".into();
            return;
        }
        if line.starts_with("Using previous index") {
            self.phase = "Using metadata change detection".into();
            return;
        }
        if line.starts_with("Upload directory ") || line.starts_with("Would upload directory ") {
            self.phase = "Scanning source folder".into();
            return;
        }
        if line.starts_with("Change detection summary:") {
            self.phase = "Change detection summary".into();
            return;
        }
        if let Some((files, hardlinks)) = parse_total_files(line) {
            self.total_files = Some(files);
            self.hardlinks = Some(hardlinks);
            return;
        }
        if let Some((files, data)) = parse_unchanged_files(line) {
            self.unchanged_files = Some(files);
            self.unchanged_data = Some(data);
            return;
        }
        if let Some((files, data)) = parse_changed_files(line) {
            self.changed_files = Some(files);
            self.changed_data = Some(data);
            return;
        }
        if let Some((padding, chunks)) = parse_padding(line) {
            self.padding = Some(padding);
            self.partially_reused_chunks = Some(chunks);
            return;
        }
        if let Some(reused_data) = parse_reused_data(line) {
            self.reused_data = Some(reused_data);
            return;
        }
        if line.starts_with("failed to open file:")
            || line.starts_with("skipping mount point:")
            || line.starts_with("WARNING:")
            || line.starts_with("warning:")
        {
            self.warnings = self.warnings.saturating_add(1);
            return;
        }
        if let Some(rest) = line.strip_prefix("processed ") {
            if let Some((processed, uploaded)) = parse_processed_uploaded(rest) {
                self.processed = Some(processed);
                self.uploaded = Some(uploaded);
                self.phase = "Backing up files".into();
            }
            return;
        }
        if line.contains(": had to backup ") {
            if let Some((done, total)) = parse_archive_total(line) {
                self.archive_done = Some(done);
                self.archive_total = Some(total);
                self.phase = "Finalizing archive".into();
            }
            return;
        }
        if let Some(percent) = parse_reused_percent(line) {
            self.reused_percent = Some(percent);
            return;
        }
        if let Some(duration) = line.strip_prefix("Duration:") {
            self.duration = Some(duration.trim().to_owned());
            self.phase = "Finishing".into();
            return;
        }
        if let Some(end_time) = line.strip_prefix("End Time:") {
            self.end_time = Some(end_time.trim().to_owned());
            return;
        }
        if line == "dry-run: no upload happened" {
            self.dry_run = true;
            self.phase = "Estimate complete".into();
        }
    }

    pub fn fraction(&self) -> Option<f64> {
        let done = self.archive_done?;
        let total = self.archive_total?;
        if total.bytes == 0 {
            None
        } else {
            Some((done.bytes as f64 / total.bytes as f64).clamp(0.0, 1.0))
        }
    }

    pub fn summary(&self) -> String {
        let mut lines = vec![self.phase.clone()];
        if let Some(snapshot) = &self.snapshot {
            lines.push(format!("Snapshot: {snapshot}"));
        }
        if let (Some(processed), Some(uploaded)) = (self.processed, self.uploaded) {
            lines.push(format!(
                "Processed {}, uploaded {}",
                processed.display(),
                uploaded.display()
            ));
        }
        if let Some(total_files) = self.total_files {
            lines.push(match self.hardlinks {
                Some(hardlinks) => format!(
                    "Files: {} total, {} hardlinks",
                    format_count(total_files),
                    format_count(hardlinks)
                ),
                None => format!("Files: {} total", format_count(total_files)),
            });
        }
        if let (Some(files), Some(data)) = (self.unchanged_files, self.unchanged_data) {
            lines.push(format!(
                "Unchanged: {} files, {} reusable",
                format_count(files),
                data.display()
            ));
        }
        if let (Some(files), Some(data)) = (self.changed_files, self.changed_data) {
            lines.push(format!(
                "Changed: {} files, {}",
                format_count(files),
                data.display()
            ));
        }
        if let (Some(done), Some(total)) = (self.archive_done, self.archive_total) {
            lines.push(format!(
                "Archive: {} of {}",
                done.display(),
                total.display()
            ));
        }
        if let Some(reused) = self.reused_percent {
            match self.reused_data {
                Some(data) => lines.push(format!(
                    "Reused {} ({reused:.1}%) from previous backup",
                    data.display()
                )),
                None => lines.push(format!("Reused {reused:.1}% from previous backup")),
            }
        } else if let Some(data) = self.reused_data {
            lines.push(format!("Reused {} from previous backup", data.display()));
        }
        if self.warnings > 0 {
            lines.push(format!("{} warnings", self.warnings));
        }
        if let Some(duration) = &self.duration {
            lines.push(format!("Duration: {duration}"));
        }
        if self.dry_run {
            if self.processed.is_none()
                && self.archive_total.is_none()
                && self.changed_data.is_none()
            {
                lines.push("PBS dry run completed, but did not provide a size estimate for this backup mode".into());
            } else {
                lines.push("Dry run: no upload happened".into());
            }
        }
        lines.join("\n")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ByteSize {
    bytes: u64,
}

impl ByteSize {
    pub const fn from_bytes(bytes: u64) -> Self {
        Self { bytes }
    }

    pub const fn bytes(self) -> u64 {
        self.bytes
    }

    pub fn display(self) -> String {
        const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
        let mut value = self.bytes as f64;
        let mut unit = 0;
        while value >= 1024.0 && unit < UNITS.len() - 1 {
            value /= 1024.0;
            unit += 1;
        }
        if unit == 0 {
            format!("{} {}", self.bytes, UNITS[unit])
        } else {
            format!("{value:.3} {}", UNITS[unit])
        }
    }
}

fn parse_total_files(line: &str) -> Option<(u64, u64)> {
    let rest = line.strip_prefix("- ")?;
    let (files, rest) = parse_count(rest)?;
    let rest = rest.trim_start().strip_prefix("total files (")?;
    let (hardlinks, rest) = parse_count(rest)?;
    rest.strip_prefix(" hardlinks)")?;
    Some((files, hardlinks))
}

fn parse_unchanged_files(line: &str) -> Option<(u64, ByteSize)> {
    let rest = line.strip_prefix("- ")?;
    let (files, rest) = parse_count(rest)?;
    let rest = rest
        .trim_start()
        .strip_prefix("unchanged, reusable files with ")?;
    let (data, _) = parse_size(rest)?;
    Some((files, data))
}

fn parse_changed_files(line: &str) -> Option<(u64, ByteSize)> {
    let rest = line.strip_prefix("- ")?;
    let (files, rest) = parse_count(rest)?;
    let rest = rest
        .trim_start()
        .strip_prefix("changed or non-reusable files with ")?;
    let (data, _) = parse_size(rest)?;
    Some((files, data))
}

fn parse_padding(line: &str) -> Option<(ByteSize, u64)> {
    let rest = line.strip_prefix("- ")?;
    let (padding, rest) = parse_size(rest)?;
    let rest = rest.trim_start().strip_prefix("padding in ")?;
    let (chunks, _) = parse_count(rest)?;
    Some((padding, chunks))
}

fn parse_reused_data(line: &str) -> Option<ByteSize> {
    let (_, rest) = line.split_once(": reused ")?;
    let (data, rest) = parse_size(rest)?;
    if rest.trim_start().starts_with("from previous snapshot") {
        Some(data)
    } else {
        None
    }
}

fn parse_count(input: &str) -> Option<(u64, &str)> {
    let input = input.trim_start();
    let split_at = input
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(input.len());
    if split_at == 0 {
        return None;
    }
    Some((input[..split_at].parse().ok()?, &input[split_at..]))
}

fn format_count(value: u64) -> String {
    let text = value.to_string();
    let mut out = String::new();
    for (index, character) in text.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push(',');
        }
        out.push(character);
    }
    out.chars().rev().collect()
}

fn parse_processed_uploaded(rest: &str) -> Option<(ByteSize, ByteSize)> {
    let (processed, after_processed) = parse_size(rest)?;
    let after = after_processed.trim_start();
    let after = after.strip_prefix("in ")?;
    let (_, after_time) = after.split_once(", uploaded ")?;
    let (uploaded, _) = parse_size(after_time)?;
    Some((processed, uploaded))
}

fn parse_archive_total(line: &str) -> Option<(ByteSize, ByteSize)> {
    let (_, rest) = line.split_once(": had to backup ")?;
    let (done, rest) = parse_size(rest)?;
    let rest = rest.trim_start().strip_prefix("of ")?;
    let (total, _) = parse_size(rest)?;
    Some((done, total))
}

fn parse_reused_percent(line: &str) -> Option<f64> {
    let start = line.find('(')? + 1;
    let end = line[start..].find("%)")? + start;
    line[start..end].parse().ok()
}

fn parse_size(input: &str) -> Option<(ByteSize, &str)> {
    let input = input.trim_start();
    let mut parts = input.splitn(3, char::is_whitespace);
    let value = parts.next()?;
    let unit = parts.next()?;
    let rest = parts.next().unwrap_or("");
    let value: f64 = value.parse().ok()?;
    let multiplier = match unit {
        "B" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((
        ByteSize::from_bytes((value * multiplier).round() as u64),
        rest,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_periodic_progress() {
        let mut activity = BackupActivity::new();
        activity.apply_line("processed 613.874 MiB in 1m, uploaded 573.497 MiB");
        assert_eq!(activity.phase, "Backing up files");
        assert_eq!(activity.processed.unwrap().display(), "613.874 MiB");
        assert_eq!(activity.uploaded.unwrap().display(), "573.497 MiB");
    }

    #[test]
    fn parses_archive_summary_and_fraction() {
        let mut activity = BackupActivity::new();
        activity.apply_line("home.ppxar: had to backup 6.875 GiB of 7.427 GiB (compressed 2.917 GiB) in 441.36 s (average 15.95 MiB/s)");
        let fraction = activity.fraction().expect("fraction");
        assert!(fraction > 0.92 && fraction < 0.93);
    }

    #[test]
    fn counts_warnings_without_making_them_primary() {
        let mut activity = BackupActivity::new();
        activity.apply_line("failed to open file: \"shadow\": access denied");
        activity.apply_line("skipping mount point: \".local/share/flatpak\"");
        activity.apply_line("warning: file size increased while reading: file will be truncated!");
        assert_eq!(activity.warnings, 3);
    }

    #[test]
    fn parses_change_detection_summary() {
        let mut activity = BackupActivity::new();
        activity.apply_line("- 107395 total files (1745 hardlinks)");
        activity.apply_line("- 103719 unchanged, reusable files with 7.148 GiB data");
        activity.apply_line("- 1931 changed or non-reusable files with 287.845 MiB data");
        activity.apply_line("- 43.734 MiB padding in 40 partially reused chunks");
        assert_eq!(activity.total_files, Some(107395));
        assert_eq!(activity.hardlinks, Some(1745));
        assert_eq!(activity.unchanged_files, Some(103719));
        assert_eq!(activity.unchanged_data.unwrap().display(), "7.148 GiB");
        assert_eq!(activity.changed_files, Some(1931));
        assert_eq!(activity.changed_data.unwrap().display(), "287.845 MiB");
        assert_eq!(activity.padding.unwrap().display(), "43.734 MiB");
        assert_eq!(activity.partially_reused_chunks, Some(40));
    }

    #[test]
    fn parses_reused_data_and_percent() {
        let mut activity = BackupActivity::new();
        activity.apply_line(
            "home.ppxar: reused 7.19 GiB from previous snapshot for unchanged files (3215 chunks)",
        );
        activity.apply_line("home.ppxar: backup was done incrementally, reused 7.322 GiB (98.0%)");
        assert_eq!(activity.reused_data.unwrap().display(), "7.190 GiB");
        assert_eq!(activity.reused_percent, Some(98.0));
    }
}
