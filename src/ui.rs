use crate::{
    app_store::{AppSettingsDocument, AppSettingsStore, StoredBackup, StoredServer},
    domain::{
        BackupProfile, ChangeDetection, CryptMode, EncryptionSettings, RetentionPolicy,
        default_home_exclusions,
    },
    executor::{CancellationToken, CommandEnvironment, run_command_streaming_cancelable},
    pbs::{CommandSpec, PbsClient},
    pbs_output::BackupActivity,
    restore::{SnapshotFile, SnapshotSummary, parse_snapshot_files, parse_snapshot_list},
    secret_store::SecretStore,
};
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
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
struct ServerConfig {
    name: String,
    repository: String,
    fingerprint: String,
}

#[derive(Clone)]
struct BackupConfig {
    name: String,
    server: String,
    source: String,
    archive_name: String,
    exclusions: Vec<String>,
}

impl From<&StoredServer> for ServerConfig {
    fn from(server: &StoredServer) -> Self {
        Self {
            name: server.name.clone(),
            repository: server.repository.clone(),
            fingerprint: server.fingerprint.clone(),
        }
    }
}

impl From<&ServerConfig> for StoredServer {
    fn from(server: &ServerConfig) -> Self {
        Self {
            name: server.name.clone(),
            repository: server.repository.clone(),
            fingerprint: server.fingerprint.clone(),
        }
    }
}

impl From<&StoredBackup> for BackupConfig {
    fn from(backup: &StoredBackup) -> Self {
        Self {
            name: backup.name.clone(),
            server: backup.server.clone(),
            source: backup.source.to_string_lossy().into_owned(),
            archive_name: backup.archive_name.clone(),
            exclusions: backup.exclusions.clone(),
        }
    }
}

impl From<&BackupConfig> for StoredBackup {
    fn from(backup: &BackupConfig) -> Self {
        Self {
            name: backup.name.clone(),
            server: backup.server.clone(),
            source: PathBuf::from(&backup.source),
            archive_name: backup.archive_name.clone(),
            exclusions: backup.exclusions.clone(),
        }
    }
}

#[derive(Clone)]
struct ActivityWidgets {
    progress_bar: gtk::ProgressBar,
    phase_label: gtk::Label,
    metrics_label: gtk::Label,
    warning_label: gtk::Label,
    log_buffer: gtk::TextBuffer,
}

#[derive(Clone)]
struct FormWidgets {
    server_name: adw::EntryRow,
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
    save_backup_button: gtk::Button,
    estimate_button: gtk::Button,
    dry_run_button: gtk::Button,
    backup_button: gtk::Button,
    cancel_backup_button: gtk::Button,
    list_snapshots_button: gtk::Button,
    list_archives_button: gtk::Button,
    restore_button: gtk::Button,
    cancel_restore_button: gtk::Button,
    add_server_button: adw::ActionRow,
    add_server_empty_row: adw::ActionRow,
    add_backup_button: adw::ActionRow,
    add_backup_empty_row: adw::ActionRow,
    restore_home_row: adw::ActionRow,
    snapshot_row: adw::ComboRow,
    archive_row: adw::ComboRow,
    restore_destination: adw::EntryRow,
    restore_destination_button: gtk::Button,
    restore_destination_path: Rc<RefCell<Option<PathBuf>>>,
    restore_pattern: adw::EntryRow,
    archive_refresh_requested: Rc<Cell<bool>>,
    updating_snapshots: Rc<Cell<bool>>,
    server_model: Rc<gtk::StringList>,
    servers: Rc<RefCell<Vec<ServerConfig>>>,
    settings_store: AppSettingsStore,
    secret_store: SecretStore,
    servers_group: adw::PreferencesGroup,
    backups_group: adw::PreferencesGroup,
    backups: Rc<RefCell<Vec<BackupConfig>>>,
    active_backup_row: Rc<RefCell<Option<adw::ActionRow>>>,
    snapshots: Rc<gtk::StringList>,
    archives: Rc<gtk::StringList>,
    backup_activity: ActivityWidgets,
    restore_activity: ActivityWidgets,
    active_activity: Rc<RefCell<ActivityWidgets>>,
    current_cancel: Rc<RefCell<Option<CancellationToken>>>,
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

    let settings_store = AppSettingsStore::new(settings_path());
    let saved_settings = settings_store.load().unwrap_or_else(|error| {
        eprintln!("Failed to load Crumbs settings: {error}");
        AppSettingsDocument::new(Vec::new(), Vec::new()).expect("empty settings are valid")
    });
    let mut server_configs = saved_settings
        .servers
        .iter()
        .map(ServerConfig::from)
        .collect::<Vec<_>>();
    if server_configs.is_empty() {
        let default_repository = default_repository();
        if !default_repository.trim().is_empty() {
            server_configs.push(ServerConfig {
                name: default_server_name(),
                repository: default_repository,
                fingerprint: default_fingerprint(),
            });
        }
    }
    let backup_configs = if saved_settings.backups.is_empty() && !server_configs.is_empty() {
        vec![BackupConfig {
            name: default_backup_id(),
            server: server_configs[0].name.clone(),
            source: default_source(),
            archive_name: "home".into(),
            exclusions: default_home_exclusions(),
        }]
    } else {
        saved_settings
            .backups
            .iter()
            .map(BackupConfig::from)
            .collect::<Vec<_>>()
    };
    let has_initial_server = !server_configs.is_empty();
    let initial_server_name = server_configs
        .first()
        .map_or_else(default_server_name, |server| server.name.clone());
    let initial_repository = server_configs
        .first()
        .map_or_else(default_repository, |server| server.repository.clone());
    let initial_fingerprint = server_configs
        .first()
        .map_or_else(default_fingerprint, |server| server.fingerprint.clone());

    let server_name = adw::EntryRow::builder()
        .title("Name")
        .text(initial_server_name)
        .build();
    let repository = adw::EntryRow::builder()
        .title("Server")
        .text(initial_repository)
        .build();
    let password = adw::PasswordEntryRow::builder().title("Password").build();
    let fingerprint = adw::EntryRow::builder()
        .title("Certificate Fingerprint")
        .text(initial_fingerprint)
        .build();
    let check_button = gtk::Button::builder().label("Check Server").build();

    let initial_server_labels: Vec<String> = server_configs.iter().map(server_label).collect();
    let initial_server_refs: Vec<&str> = initial_server_labels.iter().map(String::as_str).collect();
    let server_model = Rc::new(gtk::StringList::new(&initial_server_refs));
    let servers = Rc::new(RefCell::new(server_configs));
    let initial_selected = if has_initial_server {
        0
    } else {
        gtk::INVALID_LIST_POSITION
    };
    let backup_server_row = adw::ComboRow::builder()
        .title("Server")
        .model(&*server_model)
        .selected(initial_selected)
        .build();
    let restore_server_row = adw::ComboRow::builder()
        .title("Server")
        .model(&*server_model)
        .selected(initial_selected)
        .build();

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

    let (backup_activity_group, backup_activity) = activity_group("Activity");
    let (restore_activity_group, restore_activity) = activity_group("Activity");
    let active_activity = Rc::new(RefCell::new(backup_activity.clone()));

    let save_backup_button = gtk::Button::builder()
        .label("Save Backup Settings")
        .css_classes(["pill"])
        .halign(gtk::Align::Center)
        .width_request(210)
        .build();
    let backup_button = gtk::Button::builder()
        .label("Back Up Now")
        .css_classes(["suggested-action", "pill"])
        .halign(gtk::Align::Center)
        .width_request(210)
        .build();
    let cancel_backup_button = gtk::Button::builder()
        .label("Cancel")
        .css_classes(["destructive-action", "pill"])
        .halign(gtk::Align::Center)
        .sensitive(false)
        .width_request(130)
        .build();
    let estimate_button = gtk::Button::builder().label("Estimate").build();
    let dry_run_button = gtk::Button::builder().label("Dry Run").build();
    let backup_secondary_actions = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .halign(gtk::Align::End)
        .build();
    backup_secondary_actions.append(&estimate_button);
    backup_secondary_actions.append(&dry_run_button);

    let backup_page = adw::PreferencesPage::new();
    let backup_action_group = adw::PreferencesGroup::new();
    let backup_primary_actions = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .halign(gtk::Align::Center)
        .build();
    backup_primary_actions.append(&save_backup_button);
    backup_primary_actions.append(&backup_button);
    backup_primary_actions.append(&cancel_backup_button);
    backup_action_group.add(&backup_primary_actions);
    let backup_target_group = adw::PreferencesGroup::builder()
        .title("Backup Target")
        .description("Choose a PBS server and archive identity for this backup")
        .build();
    backup_target_group.add(&backup_server_row);
    backup_target_group.add(&backup_id);
    backup_target_group.add(&archive_name);
    let files_group = adw::PreferencesGroup::builder()
        .title("Files to Back Up")
        .description("Only files in the selected source folder are backed up")
        .build();
    files_group.add(&source);
    let exclude_group = adw::PreferencesGroup::builder()
        .title("Exclude From Backup")
        .description("The following patterns are not backed up")
        .build();
    exclude_group.add(&exclusions_row);
    let backup_tools_group = adw::PreferencesGroup::builder()
        .title("Backup Tools")
        .description("Dry-run commands use the same PBS target without uploading data")
        .build();
    backup_tools_group.add(&backup_secondary_actions);
    backup_page.add(&backup_action_group);
    backup_page.add(&backup_target_group);
    backup_page.add(&files_group);
    backup_page.add(&exclude_group);
    backup_page.add(&backup_tools_group);
    backup_page.add(&backup_activity_group);

    let schedule_page = adw::PreferencesPage::new();
    let schedule_group = adw::PreferencesGroup::builder()
        .title("Scheduled Backups")
        .build();
    let schedule_row = adw::ActionRow::builder()
        .title("Regularly Create Backups")
        .subtitle("Scheduler and monitor are planned after the manual backup MVP")
        .build();
    let schedule_switch = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .sensitive(false)
        .build();
    schedule_row.add_suffix(&schedule_switch);
    let frequency_model = gtk::StringList::new(&["Daily", "Weekly", "Monthly"]);
    let frequency_row = adw::ComboRow::builder()
        .title("Frequency")
        .model(&frequency_model)
        .sensitive(false)
        .build();
    schedule_group.add(&schedule_row);
    schedule_group.add(&frequency_row);
    let retention_group = adw::PreferencesGroup::builder()
        .title("Delete Old Archives")
        .description("Server-managed retention is the current default")
        .build();
    let retention_row = adw::ActionRow::builder()
        .title("Retention")
        .subtitle("Server managed")
        .build();
    retention_group.add(&retention_row);
    schedule_page.add(&schedule_group);
    schedule_page.add(&retention_group);

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
    let list_snapshots_button = gtk::Button::from_icon_name("view-refresh-symbolic");
    list_snapshots_button.set_tooltip_text(Some("Refresh Snapshots"));
    list_snapshots_button.set_halign(gtk::Align::End);
    let list_archives_button = gtk::Button::from_icon_name("view-refresh-symbolic");
    list_archives_button.set_tooltip_text(Some("Refresh Archives"));
    list_archives_button.set_halign(gtk::Align::End);
    list_archives_button.set_visible(false);

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
    let archive_refresh_requested = Rc::new(Cell::new(false));
    let restore_button = gtk::Button::builder()
        .label("Restore")
        .css_classes(["destructive-action", "pill"])
        .halign(gtk::Align::Center)
        .width_request(210)
        .build();
    let cancel_restore_button = gtk::Button::builder()
        .label("Cancel")
        .css_classes(["destructive-action", "pill"])
        .halign(gtk::Align::Center)
        .sensitive(false)
        .width_request(130)
        .build();

    let restore_page_content = adw::PreferencesPage::new();
    let restore_server_group = adw::PreferencesGroup::builder()
        .title("Server")
        .description("Choose the PBS server to browse")
        .build();
    restore_server_group.add(&restore_server_row);
    let restore_archive_group = adw::PreferencesGroup::builder()
        .title("Snapshots and Archives")
        .description("Snapshots refresh from the selected server; archives refresh when the snapshot changes")
        .build();
    restore_archive_group.add(&snapshot_row);
    restore_archive_group.add(&archive_row);
    let restore_group = adw::PreferencesGroup::builder()
        .title("Restore Files")
        .build();
    restore_group.add(&restore_destination);
    restore_group.add(&restore_pattern);
    let restore_actions = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .halign(gtk::Align::Center)
        .build();
    restore_actions.append(&restore_button);
    restore_actions.append(&cancel_restore_button);
    restore_group.add(&restore_actions);
    restore_page_content.add(&restore_server_group);
    restore_page_content.add(&restore_archive_group);
    restore_page_content.add(&restore_group);
    restore_page_content.add(&restore_activity_group);

    let detail_stack = adw::ViewStack::new();
    detail_stack.set_vexpand(true);
    detail_stack
        .add_titled_with_icon(
            &backup_page,
            Some("backup"),
            "Backup",
            "drive-harddisk-symbolic",
        )
        .set_use_underline(true);
    detail_stack
        .add_titled_with_icon(
            &schedule_page,
            Some("schedule"),
            "Schedule",
            "x-office-calendar-symbolic",
        )
        .set_use_underline(true);
    let detail_switcher = adw::ViewSwitcher::builder()
        .stack(&detail_stack)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .build();
    let detail_header = adw::HeaderBar::builder()
        .title_widget(&detail_switcher)
        .build();
    let detail_toolbar = adw::ToolbarView::new();
    detail_toolbar.add_top_bar(&detail_header);
    detail_toolbar.set_content(Some(&detail_stack));
    let detail_page = adw::NavigationPage::with_tag(&detail_toolbar, "Backup", "backup-detail");

    let restore_header = adw::HeaderBar::builder()
        .title_widget(&gtk::Label::new(Some("Restore")))
        .build();
    let restore_toolbar = adw::ToolbarView::new();
    restore_toolbar.add_top_bar(&restore_header);
    restore_toolbar.set_content(Some(&restore_page_content));
    let restore_page = adw::NavigationPage::with_tag(&restore_toolbar, "Restore", "restore");

    let overview_header = adw::HeaderBar::builder()
        .title_widget(&gtk::Label::new(Some("Crumbs")))
        .build();

    let add_server_button = section_add_button("Add Server");
    let add_server_empty_row = empty_action_row(
        "network-server-symbolic",
        "Add a server",
        "Connect to a Proxmox Backup Server datastore",
    );
    let add_backup_button = section_add_button("Add Backup");
    let add_backup_empty_row = empty_action_row(
        "drive-harddisk-symbolic",
        "Configure a backup",
        "Choose a server, source folder, archive name, and exclusions",
    );

    let servers_group = adw::PreferencesGroup::builder().title("Servers").build();
    let server_overview_rows = servers
        .borrow()
        .iter()
        .map(|server| {
            overview_row(
                "network-server-symbolic",
                &server.name,
                &overview_server_subtitle(&server.repository),
            )
        })
        .collect::<Vec<_>>();
    if server_overview_rows.is_empty() {
        servers_group.add(&add_server_empty_row);
    } else {
        for row in server_overview_rows.iter() {
            servers_group.add(row);
        }
        servers_group.add(&add_server_button);
    }

    let backups_group = adw::PreferencesGroup::builder().title("Backups").build();
    let backup_overview_rows = backup_configs
        .iter()
        .map(|backup| {
            overview_row(
                "drive-harddisk-symbolic",
                &backup.name,
                &backup_label(backup),
            )
        })
        .collect::<Vec<_>>();
    if backup_overview_rows.is_empty() {
        backups_group.add(&add_backup_empty_row);
    } else {
        for row in backup_overview_rows.iter() {
            backups_group.add(row);
        }
        backups_group.add(&add_backup_button);
    }
    let backups = Rc::new(RefCell::new(backup_configs));

    let restore_home_group = adw::PreferencesGroup::builder().title("Restore").build();
    let restore_overview_row = overview_row(
        "document-revert-symbolic",
        "Restore Files",
        "Browse snapshots and recover files to a chosen folder",
    );
    restore_home_group.add(&restore_overview_row);

    let overview_page_content = adw::PreferencesPage::new();
    overview_page_content.add(&servers_group);
    overview_page_content.add(&backups_group);
    overview_page_content.add(&restore_home_group);
    let overview_toolbar = adw::ToolbarView::new();
    overview_toolbar.add_top_bar(&overview_header);
    overview_toolbar.set_content(Some(&overview_page_content));
    let overview_page = adw::NavigationPage::with_tag(&overview_toolbar, "Crumbs", "overview");

    let navigation_view = adw::NavigationView::new();
    navigation_view.add(&overview_page);
    navigation_view.add(&detail_page);
    navigation_view.add(&restore_page);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&navigation_view));

    let widgets = FormWidgets {
        server_name,
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
        save_backup_button,
        estimate_button,
        dry_run_button,
        backup_button,
        cancel_backup_button,
        list_snapshots_button,
        list_archives_button,
        restore_button,
        cancel_restore_button,
        add_server_button: add_server_button.clone(),
        add_server_empty_row: add_server_empty_row.clone(),
        add_backup_button: add_backup_button.clone(),
        add_backup_empty_row: add_backup_empty_row.clone(),
        restore_home_row: restore_overview_row.clone(),
        snapshot_row,
        archive_row,
        restore_destination,
        restore_destination_button,
        restore_destination_path,
        restore_pattern,
        archive_refresh_requested,
        updating_snapshots: Rc::new(Cell::new(false)),
        server_model,
        servers,
        settings_store,
        secret_store: SecretStore::new(),
        servers_group: servers_group.clone(),
        backups_group: backups_group.clone(),
        backups,
        active_backup_row: Rc::new(RefCell::new(None)),
        snapshots,
        archives,
        backup_activity,
        restore_activity,
        active_activity,
        current_cancel: Rc::new(RefCell::new(None)),
        toast_overlay: toast_overlay.clone(),
    };

    connect_operation(&widgets, OperationKind::Check);
    connect_operation(&widgets, OperationKind::Estimate);
    connect_operation(&widgets, OperationKind::DryRun);
    connect_operation(&widgets, OperationKind::Backup);
    connect_operation(&widgets, OperationKind::ListSnapshots);
    connect_operation(&widgets, OperationKind::ListArchives);
    connect_operation(&widgets, OperationKind::Restore);
    connect_snapshot_archive_refresh(&widgets);
    connect_cancel_buttons(&widgets);
    connect_server_selector(&backup_server_row, &widgets);
    connect_server_selector(&restore_server_row, &widgets);
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
    for row in server_overview_rows.iter() {
        add_server_open_action(row, &widgets);
        add_server_delete_button(row, &widgets);
    }
    for row in backup_overview_rows.iter() {
        add_backup_open_action(row, &widgets, &navigation_view);
        add_backup_delete_button(row, &widgets);
    }
    update_server_dependent_actions(&widgets);

    connect_add_server_row(&add_server_button, &widgets);
    connect_add_server_row(&add_server_empty_row, &widgets);

    let navigation_view_for_backup = navigation_view.clone();
    let widgets_for_add_backup = widgets.clone();
    let add_backup = move || {
        if !has_servers(&widgets_for_add_backup) {
            widgets_for_add_backup
                .toast_overlay
                .add_toast(adw::Toast::new("Server is required"));
            return;
        }
        prepare_new_backup(&widgets_for_add_backup);
        navigation_view_for_backup.push_by_tag("backup-detail");
    };
    let add_backup = Rc::new(add_backup);
    let add_backup_for_plus = Rc::clone(&add_backup);
    add_backup_button.connect_activated(move |_| add_backup_for_plus());
    add_backup_empty_row.connect_activated(move |_| add_backup());

    let widgets_for_save_backup = widgets.clone();
    let navigation_view_for_saved_backup = navigation_view.clone();
    widgets.save_backup_button.connect_clicked(move |_| {
        save_backup(&widgets_for_save_backup, &navigation_view_for_saved_backup);
    });

    let navigation_view_for_restore = navigation_view.clone();
    let widgets_for_restore = widgets.clone();
    restore_overview_row.connect_activated(move |_| {
        if !has_servers(&widgets_for_restore) {
            widgets_for_restore
                .toast_overlay
                .add_toast(adw::Toast::new("Add a server first"));
            return;
        }
        navigation_view_for_restore.push_by_tag("restore");
        reset_activity(&widgets_for_restore.restore_activity);
        start_operation(&widgets_for_restore, OperationKind::ListSnapshots);
    });

    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("Crumbs")
        .default_width(760)
        .default_height(720)
        .content(&toast_overlay)
        .build();
    window.present();
}

fn section_add_button(tooltip: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().activatable(true).build();
    row.set_tooltip_text(Some(tooltip));
    let icon = gtk::Image::from_icon_name("list-add-symbolic");
    icon.add_css_class("heading");
    let box_ = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .halign(gtk::Align::Center)
        .height_request(44)
        .build();
    box_.append(&icon);
    row.set_child(Some(&box_));
    row
}

fn empty_action_row(icon_name: &str, title: &str, subtitle: &str) -> adw::ActionRow {
    overview_row(icon_name, title, subtitle)
}

fn has_servers(widgets: &FormWidgets) -> bool {
    !widgets.servers.borrow().is_empty()
}

fn has_backups(widgets: &FormWidgets) -> bool {
    !widgets.backups.borrow().is_empty()
}

fn add_row_if_detached(group: &adw::PreferencesGroup, row: &adw::ActionRow) {
    if row.parent().is_none() {
        group.add(row);
    }
}

fn remove_row_if_attached(group: &adw::PreferencesGroup, row: &adw::ActionRow) {
    if row.parent().is_some() {
        group.remove(row);
    }
}

fn update_server_dependent_actions(widgets: &FormWidgets) {
    let has_servers = has_servers(widgets);
    let has_backups = has_backups(widgets);

    if has_servers {
        remove_row_if_attached(&widgets.servers_group, &widgets.add_server_empty_row);
        add_row_if_detached(&widgets.servers_group, &widgets.add_server_button);
    } else {
        remove_row_if_attached(&widgets.servers_group, &widgets.add_server_button);
        add_row_if_detached(&widgets.servers_group, &widgets.add_server_empty_row);
    }

    if has_backups {
        remove_row_if_attached(&widgets.backups_group, &widgets.add_backup_empty_row);
        add_row_if_detached(&widgets.backups_group, &widgets.add_backup_button);
    } else {
        remove_row_if_attached(&widgets.backups_group, &widgets.add_backup_button);
        add_row_if_detached(&widgets.backups_group, &widgets.add_backup_empty_row);
    }

    widgets.add_backup_button.set_sensitive(has_servers);
    widgets.add_backup_empty_row.set_sensitive(has_servers);
    if has_servers {
        widgets.add_backup_empty_row.remove_css_class("dim-label");
        widgets.restore_home_row.remove_css_class("dim-label");
    } else {
        widgets.add_backup_empty_row.add_css_class("dim-label");
        widgets.restore_home_row.add_css_class("dim-label");
    }
    widgets.restore_home_row.set_sensitive(has_servers);
    widgets.save_backup_button.set_sensitive(has_servers);
}

fn connect_add_server_row(row: &adw::ActionRow, widgets: &FormWidgets) {
    let widgets = widgets.clone();
    row.connect_activated(move |row| {
        prepare_new_server(&widgets);
        show_server_dialog(row, &widgets, row);
    });
}

fn prepare_new_server(widgets: &FormWidgets) {
    let next = widgets.servers.borrow().len() + 1;
    widgets.server_name.set_text(&format!("PBS Server {next}"));
    widgets.repository.set_text("");
    widgets.password.set_text("");
    widgets.fingerprint.set_text("");
}

fn prepare_new_backup(widgets: &FormWidgets) {
    widgets.active_backup_row.borrow_mut().take();
    let next = widgets.backups.borrow().len() + 1;
    widgets.backup_id.set_text(&format!("backup-{next}"));
    widgets.archive_name.set_text("home");
    widgets.source.set_text(&default_source());
    widgets
        .exclusions
        .buffer()
        .set_text(&default_home_exclusions().join("\n"));
    widgets.source_path.borrow_mut().take();
    reset_activity(&widgets.backup_activity);
}

fn add_server_overview_row(widgets: &FormWidgets, server: &ServerConfig) {
    let row = overview_row("network-server-symbolic", &server.name, &server.repository);
    add_server_open_action(&row, widgets);
    add_server_delete_button(&row, widgets);
    remove_row_if_attached(&widgets.servers_group, &widgets.add_server_button);
    remove_row_if_attached(&widgets.servers_group, &widgets.add_server_empty_row);
    widgets.servers_group.add(&row);
    update_server_dependent_actions(widgets);
}

fn add_server_open_action(row: &adw::ActionRow, widgets: &FormWidgets) {
    let widgets = widgets.clone();
    row.connect_activated(move |row| {
        let name = row.title().to_string();
        if let Some(server) = widgets
            .servers
            .borrow()
            .iter()
            .find(|server| server.name == name)
            .cloned()
        {
            widgets.server_name.set_text(&server.name);
            widgets.repository.set_text(&server.repository);
            widgets.fingerprint.set_text(&server.fingerprint);
            if let Ok(Some(password)) = widgets
                .secret_store
                .get_pbs_password(&server.name, &server.repository)
            {
                widgets.password.set_text(&password);
            } else {
                widgets.password.set_text("");
            }
            show_server_dialog(row, &widgets, row);
        }
    });
}

fn add_server_delete_button(row: &adw::ActionRow, widgets: &FormWidgets) {
    let delete_button = gtk::Button::from_icon_name("user-trash-symbolic");
    delete_button.set_tooltip_text(Some("Delete Server"));
    delete_button.add_css_class("flat");
    let widgets = widgets.clone();
    let row_for_delete = row.clone();
    delete_button.connect_clicked(move |button| {
        confirm_destructive(
            button,
            "Delete Server?",
            "This removes the server from Crumbs on this device.",
            "Delete",
            {
                let widgets = widgets.clone();
                let row_for_delete = row_for_delete.clone();
                move || delete_server(&widgets, &row_for_delete)
            },
        );
    });
    row.add_suffix(&delete_button);
}

fn delete_server(widgets: &FormWidgets, row: &adw::ActionRow) {
    let server_name = row.title().to_string();
    let removed = {
        let mut servers = widgets.servers.borrow_mut();
        servers
            .iter()
            .position(|server| server.name == server_name)
            .map(|index| (index, servers.remove(index)))
    };

    if let Some((index, removed_server)) = removed {
        if let Err(error) = widgets
            .secret_store
            .delete_pbs_password(&removed_server.name, &removed_server.repository)
        {
            widgets
                .toast_overlay
                .add_toast(adw::Toast::new(&error.to_string()));
        }
        if widgets.server_model.n_items() > index as u32 {
            widgets.server_model.remove(index as u32);
        }
        if widgets.servers.borrow().is_empty() {
            widgets.server_name.set_text("");
            widgets.repository.set_text("");
            widgets.password.set_text("");
            widgets.fingerprint.set_text("");
            widgets.add_server_empty_row.set_title("Add a server");
            widgets
                .add_server_empty_row
                .set_subtitle("Connect to a Proxmox Backup Server datastore");
        }
        widgets.servers_group.remove(row);
        update_server_dependent_actions(widgets);
        save_app_settings_or_toast(widgets);
        widgets
            .toast_overlay
            .add_toast(adw::Toast::new("Server deleted"));
    }
}

fn add_backup_overview_row(
    widgets: &FormWidgets,
    config: &BackupConfig,
    navigation_view: &adw::NavigationView,
) {
    let row = overview_row(
        "drive-harddisk-symbolic",
        &config.name,
        &backup_label(config),
    );
    add_backup_open_action(&row, widgets, navigation_view);
    add_backup_delete_button(&row, widgets);
    remove_row_if_attached(&widgets.backups_group, &widgets.add_backup_button);
    remove_row_if_attached(&widgets.backups_group, &widgets.add_backup_empty_row);
    widgets.backups_group.add(&row);
    widgets.backups_group.add(&widgets.add_backup_button);
    update_server_dependent_actions(widgets);
}

fn add_backup_open_action(
    row: &adw::ActionRow,
    widgets: &FormWidgets,
    navigation_view: &adw::NavigationView,
) {
    let widgets = widgets.clone();
    let navigation_view = navigation_view.clone();
    row.connect_activated(move |row| {
        widgets.active_backup_row.replace(Some(row.clone()));
        let name = row.title().to_string();
        if let Some(backup) = widgets
            .backups
            .borrow()
            .iter()
            .find(|backup| backup.name == name)
            .cloned()
        {
            widgets.backup_id.set_text(&backup.name);
            widgets.archive_name.set_text(&backup.archive_name);
            widgets.source.set_text(&backup.source);
            widgets.source_path.borrow_mut().take();
            widgets.server_name.set_text(&backup.server);
            widgets
                .exclusions
                .buffer()
                .set_text(&backup.exclusions.join("\n"));
        }
        navigation_view.push_by_tag("backup-detail");
    });
}

fn add_backup_delete_button(row: &adw::ActionRow, widgets: &FormWidgets) {
    let delete_button = gtk::Button::from_icon_name("user-trash-symbolic");
    delete_button.set_tooltip_text(Some("Delete Backup"));
    delete_button.add_css_class("flat");
    let widgets = widgets.clone();
    let row_for_delete = row.clone();
    delete_button.connect_clicked(move |button| {
        confirm_destructive(
            button,
            "Delete Backup?",
            "This removes the backup settings from Crumbs on this device.",
            "Delete",
            {
                let widgets = widgets.clone();
                let row_for_delete = row_for_delete.clone();
                move || delete_backup(&widgets, &row_for_delete)
            },
        );
    });
    row.add_suffix(&delete_button);
}

fn delete_backup(widgets: &FormWidgets, row: &adw::ActionRow) {
    let backup_name = row.title().to_string();
    let removed = {
        let mut backups = widgets.backups.borrow_mut();
        backups
            .iter()
            .position(|backup| backup.name == backup_name)
            .inspect(|index| {
                backups.remove(*index);
            })
            .is_some()
    };

    if removed {
        widgets.backups_group.remove(row);
        update_server_dependent_actions(widgets);
        save_app_settings_or_toast(widgets);
        widgets
            .toast_overlay
            .add_toast(adw::Toast::new("Backup deleted"));
    }
}

fn show_server_dialog(
    anchor: &impl IsA<gtk::Widget>,
    widgets: &FormWidgets,
    overview_row: &adw::ActionRow,
) {
    let dialog = adw::Window::builder()
        .title("Server Settings")
        .default_width(520)
        .modal(true)
        .build();
    if let Some(parent) = anchor
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok())
    {
        dialog.set_transient_for(Some(&parent));
    }

    let header = adw::HeaderBar::builder()
        .title_widget(&gtk::Label::new(Some("Server Settings")))
        .show_end_title_buttons(false)
        .build();
    let close_button = gtk::Button::builder().label("Cancel").build();
    header.pack_start(&close_button);
    let save_button = gtk::Button::builder()
        .label("Save")
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    header.pack_end(&save_button);

    let name_row = adw::EntryRow::builder()
        .title("Name")
        .text(widgets.server_name.text())
        .build();
    let server_row = adw::EntryRow::builder()
        .title("Server")
        .text(widgets.repository.text())
        .build();
    let password_row = adw::PasswordEntryRow::builder()
        .title("Password")
        .text(widgets.password.text())
        .build();
    let fingerprint_row = adw::EntryRow::builder()
        .title("Certificate Fingerprint")
        .text(widgets.fingerprint.text())
        .build();
    connect_server_dialog_validation(&save_button, &name_row, &server_row, &password_row);
    let check_button = gtk::Button::builder().label("Check Server").build();

    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder()
        .title("Proxmox Backup Server")
        .description("Credentials are kept in memory until Secret Service support is added")
        .build();
    group.add(&name_row);
    group.add(&server_row);
    group.add(&password_row);
    group.add(&fingerprint_row);
    group.add(&check_button);
    page.add(&group);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    dialog.set_content(Some(&toolbar));

    let dialog_for_close = dialog.clone();
    close_button.connect_clicked(move |_| dialog_for_close.close());

    let widgets_for_check = widgets.clone();
    let check_name = name_row.clone();
    let check_server = server_row.clone();
    let check_password = password_row.clone();
    let check_fingerprint = fingerprint_row.clone();
    check_button.connect_clicked(move |_| {
        copy_server_dialog_fields(
            &widgets_for_check,
            &check_name,
            &check_server,
            &check_password,
            &check_fingerprint,
        );
        start_operation(&widgets_for_check, OperationKind::Check);
    });

    let widgets_for_save = widgets.clone();
    let overview_row = overview_row.clone();
    let dialog_for_save = dialog.clone();
    save_button.connect_clicked(move |_| {
        copy_server_dialog_fields(
            &widgets_for_save,
            &name_row,
            &server_row,
            &password_row,
            &fingerprint_row,
        );
        if save_server(&widgets_for_save, &overview_row) {
            dialog_for_save.close();
        }
    });

    dialog.present();
}

fn connect_server_dialog_validation(
    save_button: &gtk::Button,
    name: &adw::EntryRow,
    server: &adw::EntryRow,
    password: &adw::PasswordEntryRow,
) {
    update_server_dialog_save_button(save_button, name, server, password);
    for row in [name, server] {
        let save_button = save_button.clone();
        let name = name.clone();
        let server = server.clone();
        let password = password.clone();
        row.connect_changed(move |_| {
            update_server_dialog_save_button(&save_button, &name, &server, &password);
        });
    }
    let save_button = save_button.clone();
    let name = name.clone();
    let server = server.clone();
    let password_for_signal = password.clone();
    password.connect_changed(move |_| {
        update_server_dialog_save_button(&save_button, &name, &server, &password_for_signal);
    });
}

fn update_server_dialog_save_button(
    save_button: &gtk::Button,
    name: &adw::EntryRow,
    server: &adw::EntryRow,
    password: &adw::PasswordEntryRow,
) {
    save_button.set_sensitive(
        !name.text().trim().is_empty()
            && !server.text().trim().is_empty()
            && !password.text().is_empty(),
    );
}

fn copy_server_dialog_fields(
    widgets: &FormWidgets,
    name: &adw::EntryRow,
    repository: &adw::EntryRow,
    password: &adw::PasswordEntryRow,
    fingerprint: &adw::EntryRow,
) {
    widgets.server_name.set_text(name.text().trim());
    widgets.repository.set_text(repository.text().trim());
    widgets.password.set_text(password.text().as_str());
    widgets.fingerprint.set_text(fingerprint.text().trim());
}

fn backup_label(config: &BackupConfig) -> String {
    format!(
        "{} as {}.pxar -> {}",
        folder_label(&PathBuf::from(&config.source)),
        config.archive_name,
        config.server
    )
}

fn save_backup(widgets: &FormWidgets, navigation_view: &adw::NavigationView) {
    let backup_id = widgets.backup_id.text().trim().to_owned();
    let archive_name = widgets.archive_name.text().trim().to_owned();
    let source = selected_path(&widgets.source, &widgets.source_path);
    if backup_id.is_empty() {
        widgets
            .toast_overlay
            .add_toast(adw::Toast::new("Backup ID is required"));
        return;
    }
    if archive_name.is_empty() {
        widgets
            .toast_overlay
            .add_toast(adw::Toast::new("Archive name is required"));
        return;
    }
    if source.is_empty() {
        widgets
            .toast_overlay
            .add_toast(adw::Toast::new("Source folder is required"));
        return;
    }

    let server = widgets.server_name.text().trim().to_owned();
    if server.is_empty()
        || !widgets
            .servers
            .borrow()
            .iter()
            .any(|saved| saved.name == server)
    {
        widgets
            .toast_overlay
            .add_toast(adw::Toast::new("Server is required"));
        return;
    }
    let config = BackupConfig {
        name: backup_id.clone(),
        server,
        source,
        archive_name,
        exclusions: exclusion_patterns(widgets),
    };
    let active_row = widgets.active_backup_row.borrow().clone();
    let previous_name = active_row.as_ref().map(|row| row.title().to_string());
    let updated_existing = {
        let mut backups = widgets.backups.borrow_mut();
        if let Some(existing) = backups.iter_mut().find(|backup| {
            previous_name
                .as_ref()
                .is_some_and(|name| backup.name == *name)
                || backup.name == config.name
        }) {
            *existing = config.clone();
            true
        } else {
            backups.push(config.clone());
            false
        }
    };

    if updated_existing {
        if let Some(row) = active_row.as_ref() {
            row.set_title(&config.name);
            row.set_subtitle(&backup_label(&config));
        }
        save_app_settings_or_toast(widgets);
        widgets
            .toast_overlay
            .add_toast(adw::Toast::new("Backup settings updated"));
    } else {
        add_backup_overview_row(widgets, &config, navigation_view);
        widgets.active_backup_row.borrow_mut().take();
        save_app_settings_or_toast(widgets);
        widgets
            .toast_overlay
            .add_toast(adw::Toast::new("Backup settings saved"));
    }
}

fn store_current_server_password_or_toast(widgets: &FormWidgets, server: &ServerConfig) -> bool {
    let password = widgets.password.text();
    if password.is_empty() {
        return true;
    }
    if let Err(error) =
        widgets
            .secret_store
            .store_pbs_password(&server.name, &server.repository, password.as_str())
    {
        widgets
            .toast_overlay
            .add_toast(adw::Toast::new(&error.to_string()));
        return false;
    }
    true
}

fn password_for_operation(widgets: &FormWidgets, repository: &str) -> Result<String, String> {
    let password = widgets.password.text().to_string();
    if !password.is_empty() {
        return Ok(password);
    }
    let server_name = widgets.server_name.text().trim().to_owned();
    if server_name.is_empty() {
        return Err("Password is required".into());
    }
    match widgets
        .secret_store
        .get_pbs_password(&server_name, repository)
        .map_err(|error| error.to_string())?
    {
        Some(password) if !password.is_empty() => Ok(password),
        _ => Err("Password is required".into()),
    }
}

fn save_app_settings(widgets: &FormWidgets) -> Result<(), crate::app_store::StoreError> {
    let servers = widgets
        .servers
        .borrow()
        .iter()
        .map(StoredServer::from)
        .collect::<Vec<_>>();
    let backups = widgets
        .backups
        .borrow()
        .iter()
        .map(StoredBackup::from)
        .collect::<Vec<_>>();
    let document = AppSettingsDocument::new(servers, backups)?;
    widgets.settings_store.save(&document)
}

fn save_app_settings_or_toast(widgets: &FormWidgets) {
    if let Err(error) = save_app_settings(widgets) {
        widgets
            .toast_overlay
            .add_toast(adw::Toast::new(&error.to_string()));
    }
}

fn exclusion_patterns(widgets: &FormWidgets) -> Vec<String> {
    let buffer = widgets.exclusions.buffer();
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn reset_activity(activity: &ActivityWidgets) {
    activity.progress_bar.set_fraction(0.0);
    activity.phase_label.set_text("Ready");
    activity.metrics_label.set_text("No activity yet");
    activity.warning_label.set_visible(false);
    activity.log_buffer.set_text("Ready");
}

fn activity_group(title: &str) -> (adw::PreferencesGroup, ActivityWidgets) {
    let progress_bar = gtk::ProgressBar::builder().show_text(false).build();
    progress_bar.set_pulse_step(0.08);
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
        .min_content_height(160)
        .vexpand(true)
        .child(&log_view)
        .build();
    let details_row = adw::ExpanderRow::builder()
        .title("Details")
        .subtitle("Raw proxmox-backup-client output")
        .build();
    details_row.add_row(&log_scroll);

    let group = adw::PreferencesGroup::builder().title(title).build();
    group.add(&status_box);
    group.add(&details_row);

    (
        group,
        ActivityWidgets {
            progress_bar,
            phase_label,
            metrics_label,
            warning_label,
            log_buffer,
        },
    )
}

fn overview_row(icon_name: &str, title: &str, subtitle: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .activatable(true)
        .build();
    let icon = gtk::Image::from_icon_name(icon_name);
    row.add_prefix(&icon);
    row
}

fn overview_server_subtitle(label: &str) -> String {
    if label.trim().is_empty() {
        "No server configured".into()
    } else {
        label.into()
    }
}

fn server_label(server: &ServerConfig) -> String {
    if server.repository.trim().is_empty() {
        server.name.clone()
    } else {
        format!("{} • {}", server.name, server.repository)
    }
}

fn apply_selected_server(widgets: &FormWidgets, selected: u32) {
    if selected == gtk::INVALID_LIST_POSITION {
        return;
    }
    if let Some(server) = widgets.servers.borrow().get(selected as usize) {
        widgets.server_name.set_text(&server.name);
        widgets.repository.set_text(&server.repository);
        widgets.fingerprint.set_text(&server.fingerprint);
        if let Ok(Some(password)) = widgets
            .secret_store
            .get_pbs_password(&server.name, &server.repository)
        {
            widgets.password.set_text(&password);
        } else {
            widgets.password.set_text("");
        }
    }
}

fn connect_server_selector(row: &adw::ComboRow, widgets: &FormWidgets) {
    let row = row.clone();
    let widgets = widgets.clone();
    row.connect_selected_notify(move |row| {
        apply_selected_server(&widgets, row.selected());
    });
}

fn save_server(widgets: &FormWidgets, primary_overview_row: &adw::ActionRow) -> bool {
    let name = widgets.server_name.text().trim().to_owned();
    let repository = widgets.repository.text().trim().to_owned();
    let fingerprint = widgets.fingerprint.text().trim().to_owned();
    if name.is_empty() {
        widgets
            .toast_overlay
            .add_toast(adw::Toast::new("Server name is required"));
        return false;
    }
    if repository.is_empty() {
        widgets
            .toast_overlay
            .add_toast(adw::Toast::new("Server is required"));
        return false;
    }
    if widgets.password.text().is_empty() {
        widgets
            .toast_overlay
            .add_toast(adw::Toast::new("Password is required"));
        return false;
    }

    let server = ServerConfig {
        name,
        repository,
        fingerprint,
    };
    if !store_current_server_password_or_toast(widgets, &server) {
        return false;
    }

    let label = server_label(&server);
    let previous_name = primary_overview_row.title().to_string();
    let updated_index =
        {
            let mut servers = widgets.servers.borrow_mut();
            if let Some((index, existing)) = servers.iter_mut().enumerate().find(|(_, existing)| {
                existing.name == previous_name || existing.name == server.name
            }) {
                *existing = server.clone();
                Some(index)
            } else {
                servers.push(server.clone());
                None
            }
        };

    if let Some(index) = updated_index {
        primary_overview_row.set_title(&server.name);
        primary_overview_row.set_subtitle(&server.repository);
        widgets.server_model.splice(index as u32, 1, &[&label]);
        save_app_settings_or_toast(widgets);
        widgets
            .toast_overlay
            .add_toast(adw::Toast::new("Server updated"));
        update_server_dependent_actions(widgets);
    } else {
        add_server_overview_row(widgets, &server);
        widgets.server_model.append(&label);
        save_app_settings_or_toast(widgets);
        widgets
            .toast_overlay
            .add_toast(adw::Toast::new("Server added"));
        widgets
            .snapshot_row
            .set_selected(gtk::INVALID_LIST_POSITION);
        widgets.archive_row.set_selected(gtk::INVALID_LIST_POSITION);
        update_server_dependent_actions(widgets);
        return true;
    }
    primary_overview_row.set_subtitle(&overview_server_subtitle(&label));
    true
}

fn confirm_destructive(
    anchor: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    action_label: &str,
    action: impl FnOnce() + 'static,
) {
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .close_response("cancel")
        .default_response("cancel")
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("confirm", action_label)]);
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);

    let parent = anchor
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok());
    dialog.choose(
        parent.as_ref(),
        None::<&gio::Cancellable>,
        move |response| {
            if response.as_str() == "confirm" {
                action();
            }
        },
    );
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

fn connect_snapshot_archive_refresh(widgets: &FormWidgets) {
    let snapshot_row = widgets.snapshot_row.clone();
    let widgets = widgets.clone();
    snapshot_row.connect_selected_notify(move |_| {
        if !widgets.updating_snapshots.get() && widgets.snapshots.n_items() > 0 {
            request_archive_refresh(&widgets);
        }
    });
}

fn request_archive_refresh(widgets: &FormWidgets) {
    if widgets.archive_refresh_requested.replace(true) {
        return;
    }

    let widgets = widgets.clone();
    glib::idle_add_local_once(move || {
        widgets.archive_refresh_requested.set(false);
        if widgets.snapshots.n_items() > 0 && selected_snapshot(&widgets).is_ok() {
            start_operation(&widgets, OperationKind::ListArchives);
        }
    });
}

fn is_browse_operation(kind: OperationKind) -> bool {
    matches!(
        kind,
        OperationKind::ListSnapshots | OperationKind::ListArchives
    )
}

fn set_active_activity(widgets: &FormWidgets, kind: OperationKind) {
    let activity = if matches!(
        kind,
        OperationKind::ListSnapshots | OperationKind::ListArchives | OperationKind::Restore
    ) {
        widgets.restore_activity.clone()
    } else {
        widgets.backup_activity.clone()
    };
    widgets.active_activity.replace(activity);
}

fn should_toast(kind: OperationKind) -> bool {
    !is_browse_operation(kind)
}

fn connect_cancel_buttons(widgets: &FormWidgets) {
    for button in [
        &widgets.cancel_backup_button,
        &widgets.cancel_restore_button,
    ] {
        let widgets = widgets.clone();
        button.connect_clicked(move |button| {
            confirm_destructive(
                button,
                "Cancel Operation?",
                "The running proxmox-backup-client process will be stopped.",
                "Cancel Operation",
                {
                    let widgets = widgets.clone();
                    move || {
                        if let Some(cancel) = widgets.current_cancel.borrow().as_ref() {
                            cancel.cancel();
                            widgets
                                .active_activity
                                .borrow()
                                .phase_label
                                .set_text("Cancelling");
                        }
                    }
                },
            );
        });
    }
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
    set_active_activity(widgets, kind);
    let activity_widgets = widgets.active_activity.borrow().clone();
    let update_activity_panel = !is_browse_operation(kind);
    let operation = match build_operation(widgets, kind) {
        Ok(operation) => operation,
        Err(error) => {
            widgets.toast_overlay.add_toast(adw::Toast::new(&error));
            activity_widgets.log_buffer.set_text(&error);
            return;
        }
    };

    widgets.check_button.set_sensitive(false);
    widgets.estimate_button.set_sensitive(false);
    widgets.dry_run_button.set_sensitive(false);
    widgets.backup_button.set_sensitive(false);
    widgets
        .cancel_backup_button
        .set_sensitive(matches!(kind, OperationKind::Backup));
    widgets.list_snapshots_button.set_sensitive(false);
    widgets.list_archives_button.set_sensitive(false);
    widgets.restore_button.set_sensitive(false);
    widgets
        .cancel_restore_button
        .set_sensitive(matches!(kind, OperationKind::Restore));
    let cancellation = CancellationToken::new();
    widgets.current_cancel.replace(Some(cancellation.clone()));
    if update_activity_panel {
        activity_widgets.progress_bar.set_fraction(0.0);
        activity_widgets.progress_bar.pulse();
        activity_widgets.phase_label.set_text("Running");
        activity_widgets
            .metrics_label
            .set_text(&operation.command.display_for_logs());
        activity_widgets.warning_label.set_visible(false);
        activity_widgets.log_buffer.set_text("");
    }

    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut activity = BackupActivity::new();
        let result = run_command_streaming_cancelable(
            &operation.command,
            &operation.environment,
            &cancellation,
            |_, line| {
                activity.apply_line(line);
                let _ = sender.send(OperationMessage::Line {
                    line: line.to_owned(),
                    activity: activity.clone(),
                });
            },
        );
        let _ = sender.send(OperationMessage::Finished { result, activity });
    });

    let widgets = widgets.clone();
    glib::timeout_add_local(Duration::from_millis(150), move || {
        let mut finished = false;
        let mut refresh_archives = false;
        while let Ok(message) = receiver.try_recv() {
            match message {
                OperationMessage::Line { line, activity } => {
                    if update_activity_panel {
                        append_log_line(&activity_widgets.log_buffer, &line);
                        update_activity(&activity_widgets, &activity);
                    }
                }
                OperationMessage::Finished { result, activity } => {
                    set_buttons_sensitive(&widgets, true);
                    widgets.cancel_backup_button.set_sensitive(false);
                    widgets.cancel_restore_button.set_sensitive(false);
                    widgets.current_cancel.borrow_mut().take();
                    finished = true;
                    match result {
                        Ok(output) => {
                            let status = if output.success() {
                                "Succeeded"
                            } else {
                                "Failed"
                            };
                            if should_toast(kind) {
                                widgets.toast_overlay.add_toast(adw::Toast::new(status));
                            }
                            handle_success_output(&widgets, kind, &output.stdout);
                            if update_activity_panel {
                                update_activity(&activity_widgets, &activity);
                                activity_widgets.phase_label.set_text(status);
                                activity_widgets.progress_bar.set_fraction(1.0);
                            }
                            refresh_archives = output.success()
                                && matches!(kind, OperationKind::ListSnapshots)
                                && widgets.snapshots.n_items() > 0;
                            if update_activity_panel {
                                if output.combined_log().trim().is_empty() {
                                    append_log_line(&activity_widgets.log_buffer, "No output");
                                }
                                append_log_line(
                                    &activity_widgets.log_buffer,
                                    &format!("{status} in {:.1}s", output.elapsed.as_secs_f32()),
                                );
                            }
                        }
                        Err(error) => {
                            let status =
                                if matches!(error, crate::executor::ExecutorError::Canceled { .. })
                                {
                                    "Canceled"
                                } else {
                                    "Failed"
                                };
                            widgets.toast_overlay.add_toast(adw::Toast::new(status));
                            activity_widgets.phase_label.set_text(status);
                            activity_widgets.metrics_label.set_text(&error.to_string());
                            append_log_line(&activity_widgets.log_buffer, &error.to_string());
                        }
                    }
                }
            }
        }
        if finished {
            if refresh_archives {
                request_archive_refresh(&widgets);
            }
            glib::ControlFlow::Break
        } else {
            if update_activity_panel {
                activity_widgets.progress_bar.pulse();
            }
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
    widgets.cancel_backup_button.set_sensitive(false);
    widgets.list_snapshots_button.set_sensitive(sensitive);
    widgets.list_archives_button.set_sensitive(sensitive);
    widgets.restore_button.set_sensitive(sensitive);
    widgets.cancel_restore_button.set_sensitive(false);
}

fn update_activity(widgets: &ActivityWidgets, activity: &BackupActivity) {
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
    let fingerprint = widgets.fingerprint.text().trim().to_owned();
    if repository.is_empty() {
        return Err("Server is required".into());
    }
    let password = password_for_operation(widgets, &repository)?;

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
            Err(error) => widgets
                .toast_overlay
                .add_toast(adw::Toast::new(&error.to_string())),
        },
        OperationKind::ListArchives => match parse_snapshot_files(stdout) {
            Ok(files) => populate_archives(widgets, &files),
            Err(error) => widgets
                .toast_overlay
                .add_toast(adw::Toast::new(&error.to_string())),
        },
        _ => {}
    }
}

fn populate_snapshots(widgets: &FormWidgets, snapshots: &[SnapshotSummary]) {
    widgets.updating_snapshots.set(true);
    clear_string_list(&widgets.snapshots);
    for snapshot in snapshots {
        widgets
            .snapshots
            .append(&format!("{}|{}", snapshot.path(), snapshot.title()));
    }
    if !snapshots.is_empty() {
        widgets.snapshot_row.set_selected(0);
    }
    widgets.updating_snapshots.set(false);
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
    widgets
        .active_activity
        .borrow()
        .metrics_label
        .set_text(&format!(
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

    let exclusions = exclusion_patterns(widgets);

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

fn settings_path() -> PathBuf {
    glib::user_config_dir().join("Crumbs").join("settings.json")
}

fn default_repository() -> String {
    std::env::var("CRUMBS_TEST_PBS_DIRECT_REPOSITORY").unwrap_or_default()
}

fn default_fingerprint() -> String {
    std::env::var("PBS_FINGERPRINT").unwrap_or_default()
}

fn default_server_name() -> String {
    std::env::var("CRUMBS_TEST_PBS_NAME")
        .ok()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "PBS Server".into())
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
