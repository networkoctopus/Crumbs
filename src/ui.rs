use crate::{
    domain::{
        BackupProfile, ChangeDetection, CryptMode, EncryptionSettings, RetentionPolicy,
        default_home_exclusions,
    },
    executor::{CommandEnvironment, run_command_streaming},
    pbs::{CommandSpec, PbsClient},
    pbs_output::BackupActivity,
    restore::{SnapshotFile, SnapshotSummary, parse_snapshot_files, parse_snapshot_list},
};
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

const APP_ID: &str = "io.github.networkoctopus.Crumbs";

pub fn run() -> glib::ExitCode {
    let application = adw::Application::builder().application_id(APP_ID).build();
    application.connect_activate(build_ui);
    application.run()
}

#[derive(Clone)]
struct FormWidgets {
    repository: adw::EntryRow,
    password: adw::PasswordEntryRow,
    fingerprint: adw::EntryRow,
    source: adw::EntryRow,
    source_button: gtk::Button,
    source_path: Rc<RefCell<Option<PathBuf>>>,
    backup_id: adw::EntryRow,
    archive_name: adw::EntryRow,
    exclusions: gtk::TextView,
    check_button: gtk::Button,
    estimate_button: gtk::Button,
    dry_run_button: gtk::Button,
    backup_button: gtk::Button,
    list_snapshots_button: gtk::Button,
    list_archives_button: gtk::Button,
    restore_button: gtk::Button,
    snapshot_row: adw::ComboRow,
    archive_row: adw::ComboRow,
    restore_destination: adw::EntryRow,
    restore_destination_button: gtk::Button,
    restore_destination_path: Rc<RefCell<Option<PathBuf>>>,
    restore_pattern: adw::EntryRow,
    snapshots: Rc<gtk::StringList>,
    archives: Rc<gtk::StringList>,
    progress_bar: gtk::ProgressBar,
    phase_label: gtk::Label,
    metrics_label: gtk::Label,
    warning_label: gtk::Label,
    log_buffer: gtk::TextBuffer,
    toast_overlay: adw::ToastOverlay,
}

#[derive(Clone, Copy)]
enum OperationKind {
    Check,
    Estimate,
    DryRun,
    Backup,
    ListSnapshots,
    ListArchives,
    Restore,
}

fn build_ui(application: &adw::Application) {
    let quit = gio::SimpleAction::new("quit", None);
    let application_weak = application.downgrade();
    quit.connect_activate(move |_, _| {
        if let Some(application) = application_weak.upgrade() {
            application.quit();
        }
    });
    application.add_action(&quit);
    application.set_accels_for_action("app.quit", &["<primary>q"]);

    let header = adw::HeaderBar::builder()
        .title_widget(&gtk::Label::new(Some("Crumbs")))
        .build();

    let repository = adw::EntryRow::builder()
        .title("Repository")
        .text(default_repository())
        .build();
    let password = adw::PasswordEntryRow::builder().title("Password").build();
    let fingerprint = adw::EntryRow::builder()
        .title("Certificate Fingerprint")
        .text(default_fingerprint())
        .build();

    let connection_group = adw::PreferencesGroup::builder()
        .title("Proxmox Backup Server")
        .build();
    connection_group.add(&repository);
    connection_group.add(&password);
    connection_group.add(&fingerprint);

    let source = adw::EntryRow::builder()
        .title("Source Folder")
        .text(default_source())
        .build();
    let source_button = folder_button("Choose Source Folder");
    let source_path = Rc::new(RefCell::new(None));
    source.add_suffix(&source_button);
    let backup_id = adw::EntryRow::builder()
        .title("Backup ID")
        .text(default_backup_id())
        .build();
    let archive_name = adw::EntryRow::builder()
        .title("Archive Name")
        .text("home")
        .build();

    let exclusions = gtk::TextView::builder()
        .monospace(true)
        .wrap_mode(gtk::WrapMode::None)
        .height_request(110)
        .build();
    exclusions
        .buffer()
        .set_text(&default_home_exclusions().join("\n"));
    let exclusions_scroll = gtk::ScrolledWindow::builder()
        .min_content_height(110)
        .child(&exclusions)
        .build();
    let exclusions_row = adw::ActionRow::builder().title("Exclusions").build();
    exclusions_row.set_child(Some(&exclusions_scroll));

    let backup_group = adw::PreferencesGroup::builder().title("Backup").build();
    backup_group.add(&source);
    backup_group.add(&backup_id);
    backup_group.add(&archive_name);
    backup_group.add(&exclusions_row);

    let check_button = gtk::Button::builder().label("Check Connection").build();
    let estimate_button = gtk::Button::builder().label("Dry Run Estimate").build();
    let dry_run_button = gtk::Button::builder().label("Dry Run").build();
    let backup_button = gtk::Button::builder()
        .label("Back Up Now")
        .css_classes(["suggested-action"])
        .build();
    let button_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .halign(gtk::Align::End)
        .build();
    button_box.append(&check_button);
    button_box.append(&estimate_button);
    button_box.append(&dry_run_button);
    button_box.append(&backup_button);

    let phase_label = gtk::Label::builder()
        .label("Ready")
        .xalign(0.0)
        .css_classes(["heading"])
        .build();
    let metrics_label = gtk::Label::builder()
        .label("No activity yet")
        .xalign(0.0)
        .wrap(true)
        .build();
    let warning_label = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["warning"])
        .visible(false)
        .build();
    let progress_bar = gtk::ProgressBar::builder().show_text(false).build();
    progress_bar.set_pulse_step(0.08);

    let status_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    status_box.append(&phase_label);
    status_box.append(&progress_bar);
    status_box.append(&metrics_label);
    status_box.append(&warning_label);

    let snapshots = Rc::new(gtk::StringList::new(&[]));
    let archives = Rc::new(gtk::StringList::new(&[]));
    let snapshot_row = adw::ComboRow::builder()
        .title("Snapshot")
        .model(&*snapshots)
        .build();
    let archive_row = adw::ComboRow::builder()
        .title("Archive")
        .model(&*archives)
        .build();
    let restore_destination = adw::EntryRow::builder()
        .title("Restore Destination")
        .text(default_restore_destination())
        .build();
    let restore_destination_button = folder_button("Choose Restore Destination");
    let restore_destination_path = Rc::new(RefCell::new(None));
    restore_destination.add_suffix(&restore_destination_button);
    let restore_pattern = adw::EntryRow::builder()
        .title("Path Patterns")
        .text("")
        .build();
    let list_snapshots_button = gtk::Button::builder().label("List Snapshots").build();
    let list_archives_button = gtk::Button::builder().label("List Archives").build();
    let restore_button = gtk::Button::builder()
        .label("Restore")
        .css_classes(["destructive-action"])
        .build();
    let restore_buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .halign(gtk::Align::End)
        .build();
    restore_buttons.append(&list_snapshots_button);
    restore_buttons.append(&list_archives_button);
    restore_buttons.append(&restore_button);
    let restore_group = adw::PreferencesGroup::builder().title("Restore").build();
    restore_group.add(&snapshot_row);
    restore_group.add(&archive_row);
    restore_group.add(&restore_destination);
    restore_group.add(&restore_pattern);
    restore_group.add(&restore_buttons);

    let log_buffer = gtk::TextBuffer::new(None);
    log_buffer.set_text("Ready");
    let log_view = gtk::TextView::builder()
        .buffer(&log_buffer)
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .vexpand(true)
        .build();
    let log_scroll = gtk::ScrolledWindow::builder()
        .min_content_height(180)
        .vexpand(true)
        .child(&log_view)
        .build();
    let details_row = adw::ExpanderRow::builder()
        .title("Details")
        .subtitle("Raw proxmox-backup-client output")
        .build();
    details_row.add_row(&log_scroll);

    let log_group = adw::PreferencesGroup::builder().title("Activity").build();
    log_group.add(&status_box);
    log_group.add(&details_row);

    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    page.append(&connection_group);
    page.append(&backup_group);
    page.append(&button_box);
    page.append(&restore_group);
    page.append(&log_group);

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&page)
        .build();
    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&scrolled));

    let widgets = FormWidgets {
        repository,
        password,
        fingerprint,
        source,
        source_button,
        source_path,
        backup_id,
        archive_name,
        exclusions,
        check_button,
        estimate_button,
        dry_run_button,
        backup_button,
        list_snapshots_button,
        list_archives_button,
        restore_button,
        snapshot_row,
        archive_row,
        restore_destination,
        restore_destination_button,
        restore_destination_path,
        restore_pattern,
        snapshots,
        archives,
        progress_bar,
        phase_label,
        metrics_label,
        warning_label,
        log_buffer,
        toast_overlay: toast_overlay.clone(),
    };

    connect_operation(&widgets, OperationKind::Check);
    connect_operation(&widgets, OperationKind::Estimate);
    connect_operation(&widgets, OperationKind::DryRun);
    connect_operation(&widgets, OperationKind::Backup);
    connect_operation(&widgets, OperationKind::ListSnapshots);
    connect_operation(&widgets, OperationKind::ListArchives);
    connect_folder_picker(
        &widgets.source,
        &widgets.source_button,
        &widgets.source_path,
        "Choose Source Folder",
    );
    connect_folder_picker(
        &widgets.restore_destination,
        &widgets.restore_destination_button,
        &widgets.restore_destination_path,
        "Choose Restore Destination",
    );

    connect_operation(&widgets, OperationKind::Restore);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toast_overlay));

    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("Crumbs")
        .default_width(760)
        .default_height(720)
        .content(&toolbar)
        .build();
    window.present();
}

fn folder_button(tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name("folder-open-symbolic");
    button.set_valign(gtk::Align::Center);
    button.set_tooltip_text(Some(tooltip));
    button
}

fn connect_folder_picker(
    row: &adw::EntryRow,
    button: &gtk::Button,
    selected_path: &Rc<RefCell<Option<PathBuf>>>,
    title: &str,
) {
    let clear_path = Rc::clone(selected_path);
    row.connect_changed(move |_| {
        clear_path.borrow_mut().take();
    });

    let row = row.clone();
    let selected_path = Rc::clone(selected_path);
    let title = title.to_owned();
    button.connect_clicked(move |button| {
        let dialog = gtk::FileDialog::builder().title(&title).modal(true).build();
        let parent = button
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok());
        let row = row.clone();
        let selected_path = Rc::clone(&selected_path);
        dialog.select_folder(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
            if let Ok(folder) = result {
                if let Some(path) = folder.path() {
                    row.set_text(&folder_label(&path));
                    selected_path.borrow_mut().replace(path);
                }
            }
        });
    });
}

fn folder_label(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn selected_path(row: &adw::EntryRow, selected_path: &Rc<RefCell<Option<PathBuf>>>) -> String {
    selected_path
        .borrow()
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| row.text().trim().to_owned())
}

fn connect_operation(widgets: &FormWidgets, kind: OperationKind) {
    let button = match kind {
        OperationKind::Check => widgets.check_button.clone(),
        OperationKind::Estimate => widgets.estimate_button.clone(),
        OperationKind::DryRun => widgets.dry_run_button.clone(),
        OperationKind::Backup => widgets.backup_button.clone(),
        OperationKind::ListSnapshots => widgets.list_snapshots_button.clone(),
        OperationKind::ListArchives => widgets.list_archives_button.clone(),
        OperationKind::Restore => widgets.restore_button.clone(),
    };
    let widgets = widgets.clone();
    button.connect_clicked(move |_| start_operation(&widgets, kind));
}

fn start_operation(widgets: &FormWidgets, kind: OperationKind) {
    let operation = match build_operation(widgets, kind) {
        Ok(operation) => operation,
        Err(error) => {
            widgets.toast_overlay.add_toast(adw::Toast::new(&error));
            widgets.log_buffer.set_text(&error);
            return;
        }
    };

    widgets.check_button.set_sensitive(false);
    widgets.estimate_button.set_sensitive(false);
    widgets.dry_run_button.set_sensitive(false);
    widgets.backup_button.set_sensitive(false);
    widgets.list_snapshots_button.set_sensitive(false);
    widgets.list_archives_button.set_sensitive(false);
    widgets.restore_button.set_sensitive(false);
    widgets.progress_bar.set_fraction(0.0);
    widgets.progress_bar.pulse();
    widgets.phase_label.set_text("Running");
    widgets
        .metrics_label
        .set_text(&operation.command.display_for_logs());
    widgets.warning_label.set_visible(false);
    widgets.log_buffer.set_text("");

    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut activity = BackupActivity::new();
        let result =
            run_command_streaming(&operation.command, &operation.environment, |_, line| {
                activity.apply_line(line);
                let _ = sender.send(OperationMessage::Line {
                    line: line.to_owned(),
                    activity: activity.clone(),
                });
            });
        let _ = sender.send(OperationMessage::Finished { result, activity });
    });

    let widgets = widgets.clone();
    glib::timeout_add_local(Duration::from_millis(150), move || {
        let mut finished = false;
        while let Ok(message) = receiver.try_recv() {
            match message {
                OperationMessage::Line { line, activity } => {
                    append_log_line(&widgets.log_buffer, &line);
                    update_activity(&widgets, &activity);
                }
                OperationMessage::Finished { result, activity } => {
                    set_buttons_sensitive(&widgets, true);
                    finished = true;
                    match result {
                        Ok(output) => {
                            let status = if output.success() {
                                "Succeeded"
                            } else {
                                "Failed"
                            };
                            widgets.toast_overlay.add_toast(adw::Toast::new(status));
                            handle_success_output(&widgets, kind, &output.stdout);
                            update_activity(&widgets, &activity);
                            widgets.phase_label.set_text(status);
                            widgets.progress_bar.set_fraction(1.0);
                            if output.combined_log().trim().is_empty() {
                                append_log_line(&widgets.log_buffer, "No output");
                            }
                            append_log_line(
                                &widgets.log_buffer,
                                &format!("{status} in {:.1}s", output.elapsed.as_secs_f32()),
                            );
                        }
                        Err(error) => {
                            widgets.toast_overlay.add_toast(adw::Toast::new("Failed"));
                            widgets.phase_label.set_text("Failed");
                            widgets.metrics_label.set_text(&error.to_string());
                            append_log_line(&widgets.log_buffer, &error.to_string());
                        }
                    }
                }
            }
        }
        if finished {
            glib::ControlFlow::Break
        } else {
            widgets.progress_bar.pulse();
            glib::ControlFlow::Continue
        }
    });
}

enum OperationMessage {
    Line {
        line: String,
        activity: BackupActivity,
    },
    Finished {
        result: Result<crate::executor::CommandOutput, crate::executor::ExecutorError>,
        activity: BackupActivity,
    },
}

fn set_buttons_sensitive(widgets: &FormWidgets, sensitive: bool) {
    widgets.check_button.set_sensitive(sensitive);
    widgets.estimate_button.set_sensitive(sensitive);
    widgets.dry_run_button.set_sensitive(sensitive);
    widgets.backup_button.set_sensitive(sensitive);
    widgets.list_snapshots_button.set_sensitive(sensitive);
    widgets.list_archives_button.set_sensitive(sensitive);
    widgets.restore_button.set_sensitive(sensitive);
}

fn update_activity(widgets: &FormWidgets, activity: &BackupActivity) {
    widgets.phase_label.set_text(&activity.phase);
    widgets.metrics_label.set_text(&activity.summary());
    let _ = activity.fraction();
    if activity.warnings > 0 {
        widgets
            .warning_label
            .set_text(&format!("{} warnings. See details.", activity.warnings));
        widgets.warning_label.set_visible(true);
    }
}

fn append_log_line(buffer: &gtk::TextBuffer, line: &str) {
    let mut end = buffer.end_iter();
    if buffer.char_count() > 0 {
        buffer.insert(&mut end, "\n");
    }
    buffer.insert(&mut end, line);
}

struct UiOperation {
    command: CommandSpec,
    environment: CommandEnvironment,
}

fn build_operation(widgets: &FormWidgets, kind: OperationKind) -> Result<UiOperation, String> {
    let repository = widgets.repository.text().trim().to_owned();
    let password = widgets.password.text().to_string();
    let fingerprint = widgets.fingerprint.text().trim().to_owned();
    if repository.is_empty() {
        return Err("Repository is required".into());
    }
    if password.is_empty() {
        return Err("Password is required".into());
    }

    let mut environment = CommandEnvironment::new();
    environment.insert("PBS_PASSWORD", password);
    if !fingerprint.is_empty() {
        environment.insert("PBS_FINGERPRINT", fingerprint);
    }

    let client = PbsClient::new(pbs_client_path());
    let command = match kind {
        OperationKind::Check => client.status(&repository),
        OperationKind::Estimate | OperationKind::DryRun | OperationKind::Backup => {
            let mut command = client
                .backup(&profile_from_form(widgets, &repository)?)
                .map_err(|error| error.to_string())?;
            if matches!(kind, OperationKind::Estimate | OperationKind::DryRun) {
                command.arguments.push(OsString::from("--dry-run"));
                command.arguments.push(OsString::from("true"));
            }
            Ok(command)
        }
        OperationKind::ListSnapshots => client.snapshots(&repository, None),
        OperationKind::ListArchives => {
            let snapshot = selected_snapshot(widgets)?;
            client.snapshot_files(&repository, None, &snapshot)
        }
        OperationKind::Restore => {
            let snapshot = selected_snapshot(widgets)?;
            let archive = selected_archive(widgets)?;
            let destination = selected_path(
                &widgets.restore_destination,
                &widgets.restore_destination_path,
            );
            if destination.is_empty() {
                return Err("Restore destination is required".into());
            }
            let patterns = restore_patterns(widgets);
            client.restore_with_patterns(
                &repository,
                None,
                &snapshot,
                &archive,
                &PathBuf::from(destination),
                &patterns,
            )
        }
    }
    .map_err(|error| error.to_string())?;

    Ok(UiOperation {
        command,
        environment,
    })
}

fn handle_success_output(widgets: &FormWidgets, kind: OperationKind, stdout: &str) {
    match kind {
        OperationKind::ListSnapshots => match parse_snapshot_list(stdout) {
            Ok(snapshots) => populate_snapshots(widgets, &snapshots),
            Err(error) => widgets.metrics_label.set_text(&error.to_string()),
        },
        OperationKind::ListArchives => match parse_snapshot_files(stdout) {
            Ok(files) => populate_archives(widgets, &files),
            Err(error) => widgets.metrics_label.set_text(&error.to_string()),
        },
        _ => {}
    }
}

fn populate_snapshots(widgets: &FormWidgets, snapshots: &[SnapshotSummary]) {
    clear_string_list(&widgets.snapshots);
    for snapshot in snapshots {
        widgets
            .snapshots
            .append(&format!("{}|{}", snapshot.path(), snapshot.title()));
    }
    if !snapshots.is_empty() {
        widgets.snapshot_row.set_selected(0);
    }
    widgets
        .metrics_label
        .set_text(&format!("{} snapshots found", snapshots.len()));
}

fn populate_archives(widgets: &FormWidgets, files: &[SnapshotFile]) {
    clear_string_list(&widgets.archives);
    let mut archives = BTreeSet::new();
    for file in files {
        let archive = display_archive_name(&file.name);
        if archive.ends_with(".pxar") {
            archives.insert(archive);
        }
    }
    for archive in archives {
        widgets.archives.append(&archive);
    }
    if widgets.archives.n_items() > 0 {
        widgets.archive_row.set_selected(0);
    }
    widgets.metrics_label.set_text(&format!(
        "{} restorable archives found",
        widgets.archives.n_items()
    ));
}

fn clear_string_list(list: &gtk::StringList) {
    while list.n_items() > 0 {
        list.remove(0);
    }
}

fn display_archive_name(filename: &str) -> String {
    filename
        .replace(".mpxar.didx", ".pxar")
        .replace(".ppxar.didx", ".pxar")
        .replace(".pxar.didx", ".pxar")
}

fn selected_snapshot(widgets: &FormWidgets) -> Result<String, String> {
    let selected = widgets.snapshot_row.selected();
    widgets
        .snapshots
        .string(selected)
        .map(|value| value.split('|').next().unwrap_or(value.as_str()).to_owned())
        .ok_or_else(|| "Select a snapshot first".into())
}

fn selected_archive(widgets: &FormWidgets) -> Result<String, String> {
    let selected = widgets.archive_row.selected();
    widgets
        .archives
        .string(selected)
        .map(|value| value.to_string())
        .ok_or_else(|| "Select an archive first".into())
}

fn restore_patterns(widgets: &FormWidgets) -> Vec<String> {
    widgets
        .restore_pattern
        .text()
        .split(',')
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .map(str::to_owned)
        .collect()
}

fn profile_from_form(widgets: &FormWidgets, repository: &str) -> Result<BackupProfile, String> {
    let source = selected_path(&widgets.source, &widgets.source_path);
    let backup_id = widgets.backup_id.text().trim().to_owned();
    let archive_name = widgets.archive_name.text().trim().to_owned();
    if source.is_empty() {
        return Err("Source folder is required".into());
    }
    if backup_id.is_empty() {
        return Err("Backup ID is required".into());
    }
    if archive_name.is_empty() {
        return Err("Archive name is required".into());
    }

    let buffer = widgets.exclusions.buffer();
    let exclusions = buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();

    let profile = BackupProfile {
        id: "manual".into(),
        name: "Manual Backup".into(),
        repository: repository.to_owned(),
        namespace: None,
        backup_id,
        archive_name,
        source: PathBuf::from(source),
        exclusions,
        change_detection: ChangeDetection::Metadata,
        encryption: EncryptionSettings {
            crypt_mode: CryptMode::None,
            keyfile: None,
            key_is_password_protected: false,
        },
        requires_fingerprint: !widgets.fingerprint.text().trim().is_empty(),
        retention: RetentionPolicy::ServerManaged,
    };
    profile.validate().map_err(|error| error.to_string())?;
    Ok(profile)
}

fn pbs_client_path() -> PathBuf {
    let local = PathBuf::from("local/bin/proxmox-backup-client");
    if local.exists() {
        local
    } else {
        PathBuf::from("proxmox-backup-client")
    }
}

fn default_repository() -> String {
    std::env::var("CRUMBS_TEST_PBS_DIRECT_REPOSITORY").unwrap_or_default()
}

fn default_fingerprint() -> String {
    std::env::var("PBS_FINGERPRINT").unwrap_or_default()
}

fn default_source() -> String {
    glib::home_dir().to_string_lossy().into_owned()
}

fn default_restore_destination() -> String {
    glib::home_dir()
        .join("Downloads")
        .join("CrumbsRestored")
        .to_string_lossy()
        .into_owned()
}

fn default_backup_id() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|hostname| !hostname.trim().is_empty())
        .unwrap_or_else(|| "desktop".into())
}
