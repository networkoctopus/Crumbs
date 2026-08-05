use crate::{
    app_store::{
        AppSettingsDocument, AppSettingsStore, StoredBackup, StoredBackupSource, StoredSchedule,
        StoredScheduleFrequency, StoredServer,
    },
    domain::{
        BackupProfile, BackupSource, ChangeDetection, CryptMode, EncryptionSettings,
        RetentionPolicy, default_home_exclusions,
    },
    executor::{CancellationToken, CommandEnvironment, run_command_streaming_cancelable},
    pbs::{CommandSpec, PbsClient},
    pbs_output::{BackupActivity, ByteSize},
    restore::{SnapshotFile, SnapshotSummary, parse_snapshot_files, parse_snapshot_list},
    secret_store::SecretStore,
};
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

const APP_ID: &str = "io.github.networkoctopus.Crumbs";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

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
    sources: Vec<BackupSourceConfig>,
    exclusions: Vec<String>,
    schedule: ScheduleConfig,
}

#[derive(Clone)]
struct BackupSourceConfig {
    archive_name: String,
    path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ScheduleConfig {
    enabled: bool,
    frequency: ScheduleFrequency,
    preferred_hour: u8,
    preferred_minute: u8,
    preferred_weekday: u8,
    preferred_month_day: u8,
    run_on_battery: bool,
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            frequency: ScheduleFrequency::Daily,
            preferred_hour: 17,
            preferred_minute: 0,
            preferred_weekday: 0,
            preferred_month_day: 1,
            run_on_battery: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScheduleFrequency {
    Hourly,
    Daily,
    Weekly,
    Monthly,
}

impl ScheduleFrequency {
    fn from_selected(selected: u32) -> Self {
        match selected {
            0 => Self::Hourly,
            2 => Self::Weekly,
            3 => Self::Monthly,
            _ => Self::Daily,
        }
    }

    const fn selected(self) -> u32 {
        match self {
            Self::Hourly => 0,
            Self::Daily => 1,
            Self::Weekly => 2,
            Self::Monthly => 3,
        }
    }
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
            sources: backup
                .backup_sources()
                .into_iter()
                .map(|source| BackupSourceConfig {
                    archive_name: source.archive_name,
                    path: source.path,
                })
                .collect(),
            exclusions: backup.exclusions.clone(),
            schedule: ScheduleConfig::from(&backup.schedule),
        }
    }
}

impl From<&BackupConfig> for StoredBackup {
    fn from(backup: &BackupConfig) -> Self {
        Self {
            name: backup.name.clone(),
            server: backup.server.clone(),
            source: backup
                .sources
                .first()
                .map(|source| source.path.clone())
                .unwrap_or_else(glib::home_dir),
            archive_name: backup
                .sources
                .first()
                .map(|source| source.archive_name.clone())
                .unwrap_or_else(|| "home".into()),
            sources: backup
                .sources
                .iter()
                .map(|source| StoredBackupSource {
                    archive_name: source.archive_name.clone(),
                    path: source.path.clone(),
                })
                .collect(),
            exclusions: backup.exclusions.clone(),
            schedule: StoredSchedule::from(&backup.schedule),
        }
    }
}

impl From<&StoredSchedule> for ScheduleConfig {
    fn from(schedule: &StoredSchedule) -> Self {
        Self {
            enabled: schedule.enabled,
            frequency: match schedule.frequency {
                StoredScheduleFrequency::Hourly => ScheduleFrequency::Hourly,
                StoredScheduleFrequency::Daily => ScheduleFrequency::Daily,
                StoredScheduleFrequency::Weekly => ScheduleFrequency::Weekly,
                StoredScheduleFrequency::Monthly => ScheduleFrequency::Monthly,
            },
            preferred_hour: schedule.preferred_hour,
            preferred_minute: schedule.preferred_minute,
            preferred_weekday: schedule.preferred_weekday,
            preferred_month_day: schedule.preferred_month_day,
            run_on_battery: schedule.run_on_battery,
        }
    }
}

impl From<&ScheduleConfig> for StoredSchedule {
    fn from(schedule: &ScheduleConfig) -> Self {
        Self {
            enabled: schedule.enabled,
            frequency: match schedule.frequency {
                ScheduleFrequency::Hourly => StoredScheduleFrequency::Hourly,
                ScheduleFrequency::Daily => StoredScheduleFrequency::Daily,
                ScheduleFrequency::Weekly => StoredScheduleFrequency::Weekly,
                ScheduleFrequency::Monthly => StoredScheduleFrequency::Monthly,
            },
            preferred_hour: schedule.preferred_hour,
            preferred_minute: schedule.preferred_minute,
            preferred_weekday: schedule.preferred_weekday,
            preferred_month_day: schedule.preferred_month_day,
            run_on_battery: schedule.run_on_battery,
        }
    }
}

#[derive(Clone)]
struct ActivityWidgets {
    progress_bar: gtk::ProgressBar,
    phase_label: gtk::Label,
    metrics_label: gtk::Label,
    warning_label: gtk::Label,
    details_row: adw::ExpanderRow,
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
    backup_sources: Rc<RefCell<Vec<BackupSourceConfig>>>,
    source_list: gtk::ListBox,
    backup_id: adw::EntryRow,
    archive_name: adw::EntryRow,
    exclusions: gtk::TextView,
    exclusions_row: adw::ActionRow,
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
    schedule_switch: gtk::Switch,
    schedule_frequency: adw::ComboRow,
    schedule_hour: gtk::SpinButton,
    schedule_minute: gtk::SpinButton,
    schedule_weekday: adw::ComboRow,
    schedule_month_day: gtk::SpinButton,
    schedule_time_row: adw::ActionRow,
    schedule_weekday_row: adw::ComboRow,
    schedule_month_day_row: adw::ActionRow,
    schedule_run_on_battery: gtk::Switch,
    schedule_status_row: adw::ActionRow,
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
    updating_form: Rc<Cell<bool>>,
    autosave_pending: Rc<Cell<bool>>,
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

fn primary_menu_button() -> gtk::MenuButton {
    let menu = gio::Menu::new();
    menu.append(Some("About Crumbs"), Some("app.about"));

    gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Main Menu")
        .menu_model(&menu)
        .build()
}

fn show_about_dialog(parent: Option<&gtk::Window>) {
    let dialog = gtk::AboutDialog::builder()
        .modal(true)
        .program_name("Crumbs")
        .version(APP_VERSION)
        .comments("Back up your personal files to Proxmox Backup Server")
        .website("https://github.com/networkoctopus/Crumbs")
        .website_label("Crumbs on GitHub")
        .logo_icon_name(APP_ID)
        .build();

    dialog.set_transient_for(parent);
    dialog.present();
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

    let about = gio::SimpleAction::new("about", None);
    let application_weak = application.downgrade();
    about.connect_activate(move |_, _| {
        let parent = application_weak
            .upgrade()
            .and_then(|application| application.active_window());
        show_about_dialog(parent.as_ref());
    });
    application.add_action(&about);

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
            sources: vec![BackupSourceConfig {
                archive_name: "home".into(),
                path: glib::home_dir(),
            }],
            exclusions: default_home_exclusions(),
            schedule: ScheduleConfig::default(),
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
        .visible(false)
        .build();
    let source_button = folder_button("Choose Source Folder");
    let source_path = Rc::new(RefCell::new(None));
    let backup_sources = Rc::new(RefCell::new(vec![BackupSourceConfig {
        archive_name: "home".into(),
        path: glib::home_dir(),
    }]));
    source.add_suffix(&source_button);
    let source_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let add_source_button = gtk::Button::from_icon_name("folder-new-symbolic");
    add_source_button.set_tooltip_text(Some("Add Folder"));
    add_source_button.add_css_class("flat");
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
    let exclusions_row = adw::ActionRow::builder()
        .title("Exclusions")
        .activatable(true)
        .build();
    exclusions_row.add_prefix(&gtk::Image::from_icon_name("folder-saved-search-symbolic"));
    let exclusions_edit_icon = gtk::Image::from_icon_name("go-next-symbolic");
    exclusions_row.add_suffix(&exclusions_edit_icon);

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
    let estimate_button = gtk::Button::builder().label("Calculate Size").build();
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
        .title("Included in Back Up")
        .description("Folders included in this PBS snapshot")
        .header_suffix(&add_source_button)
        .build();
    files_group.add(&source_list);
    let exclude_group = adw::PreferencesGroup::builder()
        .title("Exclude From Backup")
        .description("Choose common exclusions or add folder, file, and pattern rules")
        .build();
    exclude_group.add(&exclusions_row);
    let backup_tools_group = adw::PreferencesGroup::builder()
        .title("Backup Tools")
        .description("Calculate size locally or run a PBS dry run without uploading data")
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
        .description("Schedule settings are saved with this backup")
        .build();
    let schedule_row = adw::ActionRow::builder()
        .title("Regularly Create Backups")
        .subtitle("Background monitor integration is the next scheduler step")
        .build();
    let schedule_switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
    schedule_row.add_suffix(&schedule_switch);
    schedule_row.set_activatable_widget(Some(&schedule_switch));
    let frequency_model = gtk::StringList::new(&["Hourly", "Daily", "Weekly", "Monthly"]);
    let frequency_row = adw::ComboRow::builder()
        .title("Frequency")
        .model(&frequency_model)
        .selected(1)
        .build();
    let preferred_time_row = adw::ActionRow::builder()
        .title("Preferred Time")
        .subtitle("Used for daily, weekly, and monthly schedules")
        .build();
    let schedule_hour = gtk::SpinButton::with_range(0.0, 23.0, 1.0);
    schedule_hour.set_width_request(72);
    schedule_hour.set_valign(gtk::Align::Center);
    schedule_hour.set_value(17.0);
    let time_separator = gtk::Label::new(Some(":"));
    let schedule_minute = gtk::SpinButton::with_range(0.0, 59.0, 5.0);
    schedule_minute.set_width_request(72);
    schedule_minute.set_valign(gtk::Align::Center);
    let time_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .valign(gtk::Align::Center)
        .build();
    time_box.append(&schedule_hour);
    time_box.append(&time_separator);
    time_box.append(&schedule_minute);
    preferred_time_row.add_suffix(&time_box);
    let weekday_model = gtk::StringList::new(&[
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ]);
    let weekday_row = adw::ComboRow::builder()
        .title("Day of Week")
        .model(&weekday_model)
        .selected(0)
        .build();
    let month_day_row = adw::ActionRow::builder()
        .title("Day of Month")
        .subtitle("Backups scheduled after the last day run on the last day")
        .build();
    let schedule_month_day = gtk::SpinButton::with_range(1.0, 31.0, 1.0);
    schedule_month_day.set_width_request(72);
    schedule_month_day.set_valign(gtk::Align::Center);
    schedule_month_day.set_value(1.0);
    month_day_row.add_suffix(&schedule_month_day);
    let run_on_battery_row = adw::ActionRow::builder()
        .title("Run on Battery")
        .subtitle("Allow scheduled backups when the device is not plugged in")
        .build();
    let schedule_run_on_battery = gtk::Switch::builder().valign(gtk::Align::Center).build();
    run_on_battery_row.add_suffix(&schedule_run_on_battery);
    run_on_battery_row.set_activatable_widget(Some(&schedule_run_on_battery));
    let schedule_status_row = adw::ActionRow::builder()
        .title("Status")
        .subtitle("Schedule disabled")
        .build();
    schedule_status_row.add_prefix(&gtk::Image::from_icon_name("schedule-symbolic"));
    schedule_group.add(&schedule_row);
    schedule_group.add(&frequency_row);
    schedule_group.add(&preferred_time_row);
    schedule_group.add(&weekday_row);
    schedule_group.add(&month_day_row);
    schedule_group.add(&run_on_battery_row);
    schedule_group.add(&schedule_status_row);
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
    overview_header.pack_end(&primary_menu_button());

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
        backup_sources,
        source_list: source_list.clone(),
        backup_id,
        archive_name,
        exclusions,
        exclusions_row: exclusions_row.clone(),
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
        schedule_switch: schedule_switch.clone(),
        schedule_frequency: frequency_row.clone(),
        schedule_hour: schedule_hour.clone(),
        schedule_minute: schedule_minute.clone(),
        schedule_weekday: weekday_row.clone(),
        schedule_month_day: schedule_month_day.clone(),
        schedule_time_row: preferred_time_row.clone(),
        schedule_weekday_row: weekday_row.clone(),
        schedule_month_day_row: month_day_row.clone(),
        schedule_run_on_battery: schedule_run_on_battery.clone(),
        schedule_status_row: schedule_status_row.clone(),
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
        updating_form: Rc::new(Cell::new(false)),
        autosave_pending: Rc::new(Cell::new(false)),
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
    connect_server_selector(&backup_server_row, &widgets, true);
    connect_server_selector(&restore_server_row, &widgets, false);
    connect_folder_picker(
        &widgets.source,
        &widgets.source_button,
        &widgets.source_path,
        "Choose Source Folder",
    );
    let widgets_for_add_source = widgets.clone();
    add_source_button.connect_clicked(move |button| {
        choose_backup_source_folder(button, &widgets_for_add_source);
    });
    connect_folder_picker(
        &widgets.restore_destination,
        &widgets.restore_destination_button,
        &widgets.restore_destination_path,
        "Choose Restore Destination",
    );
    update_source_list(&widgets);
    update_exclusions_summary(&widgets);
    update_schedule_status(&widgets);
    let widgets_for_exclusions = widgets.clone();
    widgets.exclusions_row.connect_activated(move |row| {
        show_exclusions_dialog(row, &widgets_for_exclusions);
    });
    for row in server_overview_rows.iter() {
        add_server_open_action(row, &widgets);
        add_server_delete_button(row, &widgets);
    }
    for row in backup_overview_rows.iter() {
        add_backup_open_action(row, &widgets, &navigation_view);
        add_backup_run_button(row, &widgets);
        add_backup_delete_button(row, &widgets);
    }
    update_server_dependent_actions(&widgets);
    connect_schedule_controls(&widgets);
    connect_backup_autosave_controls(&widgets);

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
        match backup_config_from_form(&widgets_for_add_backup) {
            Ok(config) => {
                upsert_backup_config(
                    &widgets_for_add_backup,
                    &config,
                    Some(&navigation_view_for_backup),
                );
                save_app_settings_or_toast(&widgets_for_add_backup);
                navigation_view_for_backup.push_by_tag("backup-detail");
            }
            Err(error) => widgets_for_add_backup
                .toast_overlay
                .add_toast(adw::Toast::new(&error)),
        }
    };
    let add_backup = Rc::new(add_backup);
    let add_backup_for_plus = Rc::clone(&add_backup);
    add_backup_button.connect_activated(move |_| add_backup_for_plus());
    add_backup_empty_row.connect_activated(move |_| add_backup());

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
    widgets.updating_form.set(true);
    widgets.active_backup_row.borrow_mut().take();
    let next = widgets.backups.borrow().len() + 1;
    widgets.backup_id.set_text(&format!("backup-{next}"));
    widgets.archive_name.set_text("home");
    widgets.source.set_text(&default_source());
    widgets.backup_sources.replace(vec![BackupSourceConfig {
        archive_name: "home".into(),
        path: glib::home_dir(),
    }]);
    update_source_list(widgets);
    widgets
        .exclusions
        .buffer()
        .set_text(&default_home_exclusions().join("\n"));
    widgets.source_path.borrow_mut().take();
    set_schedule_form(widgets, &ScheduleConfig::default());
    widgets.updating_form.set(false);
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
) -> adw::ActionRow {
    let row = overview_row(
        "drive-harddisk-symbolic",
        &config.name,
        &backup_label(config),
    );
    add_backup_open_action(&row, widgets, navigation_view);
    add_backup_run_button(&row, widgets);
    add_backup_delete_button(&row, widgets);
    remove_row_if_attached(&widgets.backups_group, &widgets.add_backup_button);
    remove_row_if_attached(&widgets.backups_group, &widgets.add_backup_empty_row);
    widgets.backups_group.add(&row);
    widgets.backups_group.add(&widgets.add_backup_button);
    update_server_dependent_actions(widgets);
    row
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
        if let Some(backup) = backup_by_name(&widgets, &name) {
            load_backup_into_form(&widgets, &backup);
        }
        navigation_view.push_by_tag("backup-detail");
    });
}

fn add_backup_run_button(row: &adw::ActionRow, widgets: &FormWidgets) {
    let run_button = gtk::Button::from_icon_name("media-playback-start-symbolic");
    run_button.set_tooltip_text(Some("Back Up Now"));
    run_button.add_css_class("flat");
    let widgets = widgets.clone();
    let row_for_run = row.clone();
    run_button.connect_clicked(move |_| {
        let name = row_for_run.title().to_string();
        if let Some(backup) = backup_by_name(&widgets, &name) {
            widgets.active_backup_row.replace(Some(row_for_run.clone()));
            load_backup_into_form(&widgets, &backup);
            start_operation(&widgets, OperationKind::Backup);
        }
    });
    row.add_suffix(&run_button);
}

fn backup_by_name(widgets: &FormWidgets, name: &str) -> Option<BackupConfig> {
    widgets
        .backups
        .borrow()
        .iter()
        .find(|backup| backup.name == name)
        .cloned()
}

fn load_backup_into_form(widgets: &FormWidgets, backup: &BackupConfig) {
    widgets.updating_form.set(true);
    widgets.backup_id.set_text(&backup.name);
    if let Some(source) = backup.sources.first() {
        widgets.archive_name.set_text(&source.archive_name);
        widgets
            .source
            .set_text(source.path.to_string_lossy().as_ref());
    }
    widgets.backup_sources.replace(backup.sources.clone());
    widgets.source_path.borrow_mut().take();
    update_source_list(widgets);
    widgets.server_name.set_text(&backup.server);
    widgets
        .exclusions
        .buffer()
        .set_text(&backup.exclusions.join("\n"));
    set_schedule_form(widgets, &backup.schedule);
    widgets.updating_form.set(false);
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
    let source_count = config.sources.len();
    let first = config.sources.first();
    let target = match (source_count, first) {
        (0, _) => format!("No folders selected -> {}", config.server),
        (1, Some(source)) => format!(
            "{} as {}.pxar -> {}",
            folder_label(&source.path),
            source.archive_name,
            config.server
        ),
        (_, Some(source)) => format!(
            "{} and {} more -> {}",
            folder_label(&source.path),
            source_count - 1,
            config.server
        ),
        _ => format!("No folders selected -> {}", config.server),
    };
    format!("{target} · {}", schedule_summary(&config.schedule))
}

fn backup_config_from_form(widgets: &FormWidgets) -> Result<BackupConfig, String> {
    let backup_id = widgets.backup_id.text().trim().to_owned();
    let archive_name = widgets.archive_name.text().trim().to_owned();
    let sources = widgets.backup_sources.borrow().clone();
    if backup_id.is_empty() {
        return Err("Backup ID is required".into());
    }
    if archive_name.is_empty() {
        return Err("Archive name is required".into());
    }
    if sources.is_empty() {
        return Err("Folders to back up are required".into());
    }

    let server = widgets.server_name.text().trim().to_owned();
    if server.is_empty()
        || !widgets
            .servers
            .borrow()
            .iter()
            .any(|saved| saved.name == server)
    {
        return Err("Server is required".into());
    }

    Ok(BackupConfig {
        name: backup_id,
        server,
        sources,
        exclusions: exclusion_patterns(widgets),
        schedule: schedule_from_form(widgets),
    })
}

fn upsert_backup_config(
    widgets: &FormWidgets,
    config: &BackupConfig,
    navigation_view: Option<&adw::NavigationView>,
) -> bool {
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

    if let Some(row) = active_row.as_ref() {
        row.set_title(&config.name);
        row.set_subtitle(&backup_label(config));
    } else if let Some(navigation_view) = navigation_view {
        let row = add_backup_overview_row(widgets, config, navigation_view);
        widgets.active_backup_row.replace(Some(row));
    }

    updated_existing
}

fn autosave_current_backup(widgets: &FormWidgets) {
    if widgets.updating_form.get() || widgets.active_backup_row.borrow().is_none() {
        return;
    }
    if let Ok(config) = backup_config_from_form(widgets) {
        upsert_backup_config(widgets, &config, None);
        save_app_settings_or_toast(widgets);
    }
}

fn autosave_current_backup_deferred(widgets: &FormWidgets) {
    if widgets.updating_form.get()
        || widgets.autosave_pending.get()
        || widgets.active_backup_row.borrow().is_none()
    {
        return;
    }
    widgets.autosave_pending.set(true);
    let widgets = widgets.clone();
    glib::idle_add_local_once(move || {
        widgets.autosave_pending.set(false);
        autosave_current_backup(&widgets);
    });
}

fn schedule_from_form(widgets: &FormWidgets) -> ScheduleConfig {
    ScheduleConfig {
        enabled: widgets.schedule_switch.is_active(),
        frequency: ScheduleFrequency::from_selected(widgets.schedule_frequency.selected()),
        preferred_hour: widgets.schedule_hour.value_as_int().clamp(0, 23) as u8,
        preferred_minute: widgets.schedule_minute.value_as_int().clamp(0, 59) as u8,
        preferred_weekday: widgets.schedule_weekday.selected().min(6) as u8,
        preferred_month_day: widgets.schedule_month_day.value_as_int().clamp(1, 31) as u8,
        run_on_battery: widgets.schedule_run_on_battery.is_active(),
    }
}

fn set_schedule_form(widgets: &FormWidgets, schedule: &ScheduleConfig) {
    widgets.schedule_switch.set_active(schedule.enabled);
    widgets
        .schedule_frequency
        .set_selected(schedule.frequency.selected());
    widgets
        .schedule_hour
        .set_value(schedule.preferred_hour as f64);
    widgets
        .schedule_minute
        .set_value(schedule.preferred_minute as f64);
    widgets
        .schedule_weekday
        .set_selected((schedule.preferred_weekday.min(6)) as u32);
    widgets
        .schedule_month_day
        .set_value(schedule.preferred_month_day.clamp(1, 31) as f64);
    widgets
        .schedule_run_on_battery
        .set_active(schedule.run_on_battery);
    update_schedule_status(widgets);
}

fn update_schedule_status(widgets: &FormWidgets) {
    let schedule = schedule_from_form(widgets);
    widgets
        .schedule_time_row
        .set_visible(!matches!(schedule.frequency, ScheduleFrequency::Hourly));
    widgets
        .schedule_weekday_row
        .set_visible(matches!(schedule.frequency, ScheduleFrequency::Weekly));
    widgets
        .schedule_month_day_row
        .set_visible(matches!(schedule.frequency, ScheduleFrequency::Monthly));
    widgets
        .schedule_status_row
        .set_subtitle(&schedule_summary(&schedule));
}

fn schedule_summary(schedule: &ScheduleConfig) -> String {
    if !schedule.enabled {
        return "Schedule disabled".into();
    }
    let battery = if schedule.run_on_battery {
        "allowed on battery"
    } else {
        "plugged in preferred"
    };
    match schedule.frequency {
        ScheduleFrequency::Hourly => format!("Runs hourly; {battery}. Background monitor pending."),
        ScheduleFrequency::Daily => format!(
            "Runs daily around {:02}:{:02}; {battery}. Background monitor pending.",
            schedule.preferred_hour, schedule.preferred_minute
        ),
        ScheduleFrequency::Weekly => format!(
            "Runs weekly on {} around {:02}:{:02}; {battery}. Background monitor pending.",
            weekday_name(schedule.preferred_weekday),
            schedule.preferred_hour,
            schedule.preferred_minute
        ),
        ScheduleFrequency::Monthly => format!(
            "Runs monthly on day {} around {:02}:{:02}; {battery}. Background monitor pending.",
            schedule.preferred_month_day, schedule.preferred_hour, schedule.preferred_minute
        ),
    }
}

fn weekday_name(index: u8) -> &'static str {
    match index {
        1 => "Tuesday",
        2 => "Wednesday",
        3 => "Thursday",
        4 => "Friday",
        5 => "Saturday",
        6 => "Sunday",
        _ => "Monday",
    }
}

fn connect_schedule_controls(widgets: &FormWidgets) {
    let widgets_for_switch = widgets.clone();
    widgets.schedule_switch.connect_active_notify(move |_| {
        update_schedule_status(&widgets_for_switch);
        autosave_current_backup_deferred(&widgets_for_switch);
    });

    let widgets_for_frequency = widgets.clone();
    widgets
        .schedule_frequency
        .connect_selected_notify(move |_| {
            update_schedule_status(&widgets_for_frequency);
            autosave_current_backup_deferred(&widgets_for_frequency);
        });

    let widgets_for_hour = widgets.clone();
    widgets.schedule_hour.connect_value_changed(move |_| {
        update_schedule_status(&widgets_for_hour);
        autosave_current_backup_deferred(&widgets_for_hour);
    });

    let widgets_for_minute = widgets.clone();
    widgets.schedule_minute.connect_value_changed(move |_| {
        update_schedule_status(&widgets_for_minute);
        autosave_current_backup_deferred(&widgets_for_minute);
    });

    let widgets_for_weekday = widgets.clone();
    widgets.schedule_weekday.connect_selected_notify(move |_| {
        update_schedule_status(&widgets_for_weekday);
        autosave_current_backup_deferred(&widgets_for_weekday);
    });

    let widgets_for_month_day = widgets.clone();
    widgets.schedule_month_day.connect_value_changed(move |_| {
        update_schedule_status(&widgets_for_month_day);
        autosave_current_backup_deferred(&widgets_for_month_day);
    });

    let widgets_for_battery = widgets.clone();
    widgets
        .schedule_run_on_battery
        .connect_active_notify(move |_| {
            update_schedule_status(&widgets_for_battery);
            autosave_current_backup_deferred(&widgets_for_battery);
        });
}

fn connect_backup_autosave_controls(widgets: &FormWidgets) {
    let widgets_for_id = widgets.clone();
    widgets
        .backup_id
        .connect_changed(move |_| autosave_current_backup_deferred(&widgets_for_id));

    let widgets_for_archive = widgets.clone();
    widgets
        .archive_name
        .connect_changed(move |_| autosave_current_backup_deferred(&widgets_for_archive));
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

#[derive(Clone, Copy)]
struct PredefinedExclusion {
    title: &'static str,
    subtitle: &'static str,
    icon: &'static str,
    patterns: &'static [&'static str],
}

const PREDEFINED_EXCLUSIONS: &[PredefinedExclusion] = &[
    PredefinedExclusion {
        title: "Caches",
        subtitle: "Data that can be regenerated when needed",
        icon: "folder-saved-search-symbolic",
        patterns: &[
            "/.cache/",
            "/.ccache/",
            "/.var/app/*/cache/",
            "/.var/app/*/config/Cache/",
            "/.var/app/*/config/Code Cache/",
        ],
    },
    PredefinedExclusion {
        title: "Trash",
        subtitle: "Files that have not been irretrievably deleted",
        icon: "user-trash-symbolic",
        patterns: &["/.local/share/Trash/"],
    },
    PredefinedExclusion {
        title: "Flatpak App Installations",
        subtitle: "Documents and data are still backed up",
        icon: "preferences-desktop-apps-symbolic",
        patterns: &["/.local/share/flatpak/"],
    },
    PredefinedExclusion {
        title: "Virtual Machines and Containers",
        subtitle: "Might include data stored within",
        icon: "computer-symbolic",
        patterns: &[
            "/.local/share/gnome-boxes/",
            "/.var/app/org.gnome.Boxes/",
            "/.var/app/org.gnome.BoxesDevel/",
            "/.local/share/bottles/",
            "/.var/app/com.usebottles.bottles/",
            "/.local/share/libvirt/",
            "/.config/libvirt/",
            "/.local/share/containers/",
            "/.local/share/docker/",
        ],
    },
];

#[allow(deprecated)]
fn choose_backup_source_folder(anchor: &impl IsA<gtk::Widget>, widgets: &FormWidgets) {
    let parent = anchor
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok());
    let dialog = gtk::FileChooserNative::builder()
        .title("Add Folder")
        .action(gtk::FileChooserAction::SelectFolder)
        .accept_label("Add")
        .cancel_label("Cancel")
        .modal(true)
        .build();
    if let Some(parent) = parent.as_ref() {
        dialog.set_transient_for(Some(parent));
    }
    dialog.set_select_multiple(false);
    if let Some(parent) = glib::home_dir().parent() {
        let _ = dialog.set_current_folder(Some(&gio::File::for_path(parent)));
    }

    let widgets = widgets.clone();
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            if let Some(path) = dialog.file().and_then(|folder| folder.path()) {
                add_backup_source(&widgets, path);
            }
        }
        dialog.destroy();
    });
    dialog.show();
}

fn add_backup_source(widgets: &FormWidgets, path: PathBuf) {
    if !path.is_dir() {
        widgets.toast_overlay.add_toast(adw::Toast::new(
            "Only folders can be included in a .pxar backup",
        ));
        return;
    }
    if widgets
        .backup_sources
        .borrow()
        .iter()
        .any(|source| same_source_path(&source.path, &path))
    {
        widgets
            .toast_overlay
            .add_toast(adw::Toast::new("Already included in this backup"));
        return;
    }
    let archive_name = unique_archive_name(
        &widgets.backup_sources.borrow(),
        &archive_name_for_path(&path),
    );
    widgets
        .backup_sources
        .borrow_mut()
        .push(BackupSourceConfig { archive_name, path });
    sync_legacy_source_fields(widgets);
    update_source_list(widgets);
    autosave_current_backup(widgets);
}

fn remove_backup_source(widgets: &FormWidgets, path: &Path) {
    widgets
        .backup_sources
        .borrow_mut()
        .retain(|source| source.path != path);
    sync_legacy_source_fields(widgets);
    update_source_list(widgets);
    autosave_current_backup(widgets);
}

fn sync_legacy_source_fields(widgets: &FormWidgets) {
    let previous_updating = widgets.updating_form.replace(true);
    if let Some(source) = widgets.backup_sources.borrow().first() {
        let archive_name = source.archive_name.as_str();
        if widgets.archive_name.text().as_str() != archive_name {
            widgets.archive_name.set_text(archive_name);
        }
        let source_path = source.path.to_string_lossy();
        if widgets.source.text().as_str() != source_path.as_ref() {
            widgets.source.set_text(source_path.as_ref());
        }
    } else {
        if !widgets.archive_name.text().is_empty() {
            widgets.archive_name.set_text("");
        }
        if !widgets.source.text().is_empty() {
            widgets.source.set_text("");
        }
    }
    widgets.source_path.borrow_mut().take();
    widgets.updating_form.set(previous_updating);
}

fn update_source_list(widgets: &FormWidgets) {
    while let Some(child) = widgets.source_list.first_child() {
        widgets.source_list.remove(&child);
    }

    let sources = widgets.backup_sources.borrow().clone();
    if sources.is_empty() {
        let row = adw::ActionRow::builder()
            .title("No folders selected")
            .subtitle("Add a folder to back up")
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));
        widgets.source_list.append(&row);
        return;
    }

    for source in sources {
        let icon = source_icon(&source.path);
        let title = source_title(&source.path);
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(format!(
                "{} as {}.pxar",
                display_path(&source.path),
                source.archive_name
            ))
            .build();
        row.add_prefix(&gtk::Image::from_icon_name(icon));
        let remove_button = gtk::Button::from_icon_name("user-trash-symbolic");
        remove_button.add_css_class("flat");
        remove_button.set_tooltip_text(Some("Remove From Backup"));
        let widgets_for_remove = widgets.clone();
        let path_for_remove = source.path.clone();
        remove_button.connect_clicked(move |_| {
            remove_backup_source(&widgets_for_remove, &path_for_remove);
        });
        row.add_suffix(&remove_button);
        widgets.source_list.append(&row);
    }
}

fn same_source_path(left: &Path, right: &Path) -> bool {
    left == right || display_path(left) == display_path(right)
}

fn source_icon(path: &Path) -> &'static str {
    if path_is_home(path) {
        "user-home-symbolic"
    } else {
        "folder-symbolic"
    }
}

fn source_title(path: &Path) -> String {
    if path_is_home(path) {
        "Home".into()
    } else {
        folder_label(path)
    }
}

fn path_is_home(path: &Path) -> bool {
    path == glib::home_dir() || document_portal_leaf(path).is_some_and(|leaf| leaf == home_leaf())
}

fn home_leaf() -> String {
    glib::home_dir()
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Home")
        .to_owned()
}

fn archive_name_for_path(path: &Path) -> String {
    if path_is_home(path) {
        return "home".into();
    }
    let stem = path
        .file_stem()
        .or_else(|| path.file_name())
        .and_then(|value| value.to_str())
        .unwrap_or("archive");
    sanitize_archive_name(stem)
}

fn sanitize_archive_name(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned();
    if sanitized.is_empty() {
        "archive".into()
    } else {
        sanitized
    }
}

fn unique_archive_name(existing: &[BackupSourceConfig], base: &str) -> String {
    if !existing.iter().any(|source| source.archive_name == base) {
        return base.into();
    }
    for index in 2.. {
        let candidate = format!("{base}-{index}");
        if !existing
            .iter()
            .any(|source| source.archive_name == candidate)
        {
            return candidate;
        }
    }
    unreachable!()
}

fn show_exclusions_dialog(anchor: &impl IsA<gtk::Widget>, widgets: &FormWidgets) {
    let dialog = adw::Window::builder()
        .title("Excluded From Backup")
        .default_width(560)
        .modal(true)
        .build();
    if let Some(parent) = anchor
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok())
    {
        dialog.set_transient_for(Some(&parent));
    }

    let header = adw::HeaderBar::builder()
        .title_widget(&gtk::Label::new(Some("Exclude From Backup")))
        .show_end_title_buttons(false)
        .build();
    let done_button = gtk::Button::builder().label("Done").build();
    header.pack_end(&done_button);

    let page = adw::PreferencesPage::new();
    let add_group = adw::PreferencesGroup::new();
    let folder_row = dialog_action_row(
        "folder-symbolic",
        "Exclude Folders",
        "Choose a folder to skip",
    );
    let file_row = dialog_action_row(
        "text-x-generic-symbolic",
        "Exclude Files",
        "Choose a file to skip",
    );
    let pattern_row = dialog_action_row(
        "folder-saved-search-symbolic",
        "Exclude Pattern",
        "Add a PBS exclude pattern",
    );
    add_group.add(&folder_row);
    add_group.add(&file_row);
    add_group.add(&pattern_row);
    page.add(&add_group);

    let suggested_group = adw::PreferencesGroup::builder()
        .title("Suggested Exclusions")
        .build();
    for predefined in PREDEFINED_EXCLUSIONS {
        let check = gtk::CheckButton::new();
        check.add_css_class("selection-mode");
        check.set_active(exclusion_patterns(widgets).contains(&predefined.patterns[0].to_owned()));
        let row = adw::ActionRow::builder()
            .title(predefined.title)
            .subtitle(predefined.subtitle)
            .activatable_widget(&check)
            .build();
        row.add_prefix(&check);
        row.add_prefix(&gtk::Image::from_icon_name(predefined.icon));
        let widgets_for_toggle = widgets.clone();
        let patterns = predefined
            .patterns
            .iter()
            .map(|pattern| pattern.to_string())
            .collect::<Vec<_>>();
        check.connect_toggled(move |button| {
            if button.is_active() {
                add_exclusion_patterns(&widgets_for_toggle, &patterns);
            } else {
                remove_exclusion_patterns(&widgets_for_toggle, &patterns);
            }
        });
        suggested_group.add(&row);
    }
    page.add(&suggested_group);

    let current_group = adw::PreferencesGroup::builder()
        .title("Current Rules")
        .build();
    populate_current_exclusion_rows(&current_group, widgets);
    page.add(&current_group);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    dialog.set_content(Some(&toolbar));

    let dialog_for_done = dialog.clone();
    done_button.connect_clicked(move |_| dialog_for_done.close());

    let widgets_for_folder = widgets.clone();
    folder_row.connect_activated(move |row| {
        choose_exclusion_folder(row, &widgets_for_folder);
    });
    let widgets_for_file = widgets.clone();
    file_row.connect_activated(move |row| {
        choose_exclusion_file(row, &widgets_for_file);
    });
    let widgets_for_pattern = widgets.clone();
    pattern_row.connect_activated(move |row| {
        show_pattern_dialog(row, &widgets_for_pattern);
    });

    dialog.present();
}

fn dialog_action_row(icon: &str, title: &str, subtitle: &str) -> adw::ActionRow {
    let row = overview_row(icon, title, subtitle);
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    row
}

fn choose_exclusion_folder(anchor: &impl IsA<gtk::Widget>, widgets: &FormWidgets) {
    let dialog = gtk::FileDialog::builder()
        .title("Exclude Folder")
        .accept_label("Select")
        .modal(true)
        .build();
    let parent = anchor
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok());
    let widgets = widgets.clone();
    dialog.select_folder(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
        if let Ok(folder) = result {
            if let Some(path) = folder.path() {
                add_exclusion_patterns(&widgets, &[pattern_for_path(&path, true)]);
            }
        }
    });
}

fn choose_exclusion_file(anchor: &impl IsA<gtk::Widget>, widgets: &FormWidgets) {
    let dialog = gtk::FileDialog::builder()
        .title("Exclude File")
        .accept_label("Select")
        .modal(true)
        .build();
    let parent = anchor
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok());
    let widgets = widgets.clone();
    dialog.open(parent.as_ref(), None::<&gio::Cancellable>, move |result| {
        if let Ok(file) = result {
            if let Some(path) = file.path() {
                add_exclusion_patterns(&widgets, &[pattern_for_path(&path, false)]);
            }
        }
    });
}

fn show_pattern_dialog(anchor: &impl IsA<gtk::Widget>, widgets: &FormWidgets) {
    let dialog = adw::Window::builder()
        .title("Exclude Pattern")
        .default_width(460)
        .modal(true)
        .build();
    if let Some(parent) = anchor
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok())
    {
        dialog.set_transient_for(Some(&parent));
    }
    let header = adw::HeaderBar::builder()
        .title_widget(&gtk::Label::new(Some("Exclude Pattern")))
        .show_end_title_buttons(false)
        .build();
    let cancel_button = gtk::Button::builder().label("Cancel").build();
    let add_button = gtk::Button::builder()
        .label("Add")
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    header.pack_start(&cancel_button);
    header.pack_end(&add_button);
    let row = adw::EntryRow::builder().title("Pattern").build();
    row.add_css_class("monospace");
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    group.add(&row);
    page.add(&group);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    dialog.set_content(Some(&toolbar));

    let add_for_change = add_button.clone();
    row.connect_changed(move |row| add_for_change.set_sensitive(!row.text().trim().is_empty()));
    let dialog_for_cancel = dialog.clone();
    cancel_button.connect_clicked(move |_| dialog_for_cancel.close());
    let widgets_for_add = widgets.clone();
    let dialog_for_add = dialog.clone();
    add_button.connect_clicked(move |_| {
        let pattern = row.text().trim().to_owned();
        if !pattern.is_empty() {
            add_exclusion_patterns(&widgets_for_add, &[pattern]);
            dialog_for_add.close();
        }
    });
    dialog.present();
}

fn populate_current_exclusion_rows(group: &adw::PreferencesGroup, widgets: &FormWidgets) {
    for pattern in exclusion_patterns(widgets) {
        let row = adw::ActionRow::builder()
            .title(pattern.as_str())
            .subtitle("PBS exclude pattern")
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("folder-saved-search-symbolic"));
        let remove = gtk::Button::from_icon_name("user-trash-symbolic");
        remove.add_css_class("flat");
        remove.set_tooltip_text(Some("Remove Exclusion"));
        let widgets_for_remove = widgets.clone();
        let pattern_for_remove = pattern.clone();
        remove.connect_clicked(move |_| {
            remove_exclusion_patterns(
                &widgets_for_remove,
                std::slice::from_ref(&pattern_for_remove),
            );
        });
        row.add_suffix(&remove);
        group.add(&row);
    }
}

fn pattern_for_path(path: &Path, directory: bool) -> String {
    let mut value = path.to_string_lossy().into_owned();
    if directory && !value.ends_with('/') {
        value.push('/');
    }
    value
}

fn add_exclusion_patterns(widgets: &FormWidgets, patterns: &[String]) {
    let mut existing = exclusion_patterns(widgets);
    for pattern in patterns {
        if !existing.iter().any(|current| current == pattern) {
            existing.push(pattern.clone());
        }
    }
    set_exclusion_patterns(widgets, &existing);
}

fn remove_exclusion_patterns(widgets: &FormWidgets, patterns: &[String]) {
    let existing = exclusion_patterns(widgets)
        .into_iter()
        .filter(|pattern| !patterns.iter().any(|removed| removed == pattern))
        .collect::<Vec<_>>();
    set_exclusion_patterns(widgets, &existing);
}

fn set_exclusion_patterns(widgets: &FormWidgets, patterns: &[String]) {
    widgets.exclusions.buffer().set_text(&patterns.join("\n"));
    update_exclusions_summary(widgets);
    autosave_current_backup_deferred(widgets);
}

fn update_exclusions_summary(widgets: &FormWidgets) {
    let patterns = exclusion_patterns(widgets);
    widgets.exclusions_row.set_subtitle(&match patterns.len() {
        0 => "Nothing excluded from backup".into(),
        1 => format!("1 rule: {}", patterns[0]),
        len => format!("{len} rules configured"),
    });
}

fn reset_activity(activity: &ActivityWidgets) {
    activity.progress_bar.set_fraction(0.0);
    activity.phase_label.set_text("Ready");
    activity.metrics_label.set_text("No activity yet");
    activity.warning_label.set_visible(false);
    activity.details_row.set_visible(false);
    activity.details_row.set_expanded(false);
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
        .visible(false)
        .build();
    details_row.add_row(&log_scroll);
    status_box.append(&details_row);

    let group = adw::PreferencesGroup::builder().title(title).build();
    group.add(&status_box);

    (
        group,
        ActivityWidgets {
            progress_bar,
            phase_label,
            metrics_label,
            warning_label,
            details_row,
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

fn connect_server_selector(row: &adw::ComboRow, widgets: &FormWidgets, autosave: bool) {
    let row = row.clone();
    let widgets = widgets.clone();
    row.connect_selected_notify(move |row| {
        apply_selected_server(&widgets, row.selected());
        if autosave {
            autosave_current_backup(&widgets);
        }
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
    document_portal_leaf(path)
        .or_else(|| {
            path.file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| display_path(path))
}

fn display_path(path: &std::path::Path) -> String {
    document_portal_display_path(path).unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn document_portal_display_path(path: &std::path::Path) -> Option<String> {
    let text = path.to_string_lossy();
    let marker = "/doc/";
    let index = text.find(marker)?;
    let remainder = &text[index + marker.len()..];
    let (_, name) = remainder.split_once('/')?;
    if name.is_empty() {
        None
    } else if name == home_leaf() {
        Some(glib::home_dir().to_string_lossy().into_owned())
    } else {
        Some(glib::home_dir().join(name).to_string_lossy().into_owned())
    }
}

fn document_portal_leaf(path: &std::path::Path) -> Option<String> {
    document_portal_display_path(path).and_then(|display| {
        Path::new(&display)
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
    })
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
    if matches!(kind, OperationKind::Estimate) {
        button.connect_clicked(move |_| start_size_estimate(&widgets));
    } else {
        button.connect_clicked(move |_| start_operation(&widgets, kind));
    }
}

fn start_size_estimate(widgets: &FormWidgets) {
    sync_legacy_source_fields(widgets);
    let sources = widgets.backup_sources.borrow().clone();
    if sources.is_empty() {
        widgets
            .toast_overlay
            .add_toast(adw::Toast::new("Files to back up are required"));
        return;
    }
    let exclusions = exclusion_patterns(widgets);
    let activity = widgets.backup_activity.clone();
    widgets.active_activity.replace(activity.clone());
    set_buttons_sensitive(widgets, false);
    activity.progress_bar.set_fraction(0.0);
    activity.progress_bar.pulse();
    activity.phase_label.set_text("Calculating size");
    activity.metrics_label.set_text("Scanning selected files");
    activity.warning_label.set_visible(false);
    activity.details_row.set_visible(false);
    activity.details_row.set_expanded(false);
    activity.log_buffer.set_text("");

    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let result = calculate_backup_size(&sources, &exclusions);
        let _ = sender.send(result);
    });

    let widgets = widgets.clone();
    glib::timeout_add_local(Duration::from_millis(150), move || {
        match receiver.try_recv() {
            Ok(Ok(estimate)) => {
                set_buttons_sensitive(&widgets, true);
                activity.phase_label.set_text("Estimate complete");
                activity.progress_bar.set_fraction(1.0);
                activity.metrics_label.set_text(&format!(
                    "{} in {} files across {} folders",
                    ByteSize::from_bytes(estimate.bytes).display(),
                    format_integer(estimate.files),
                    format_integer(estimate.directories)
                ));
                if !estimate.warnings.is_empty() {
                    activity.warning_label.set_text(&format!(
                        "{} unreadable paths. See details.",
                        estimate.warnings.len()
                    ));
                    activity.warning_label.set_visible(true);
                    activity.details_row.set_visible(true);
                    activity.log_buffer.set_text(&estimate.warnings.join("\n"));
                }
                widgets
                    .toast_overlay
                    .add_toast(adw::Toast::new("Estimate complete"));
                glib::ControlFlow::Break
            }
            Ok(Err(error)) => {
                set_buttons_sensitive(&widgets, true);
                activity.phase_label.set_text("Estimate failed");
                activity.metrics_label.set_text(&error);
                activity.details_row.set_visible(true);
                activity.log_buffer.set_text(&error);
                widgets
                    .toast_overlay
                    .add_toast(adw::Toast::new("Estimate failed"));
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => {
                activity.progress_bar.pulse();
                glib::ControlFlow::Continue
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                set_buttons_sensitive(&widgets, true);
                activity.phase_label.set_text("Estimate failed");
                activity
                    .metrics_label
                    .set_text("Size calculation stopped unexpectedly");
                activity.details_row.set_visible(true);
                activity
                    .log_buffer
                    .set_text("Size calculation stopped unexpectedly");
                glib::ControlFlow::Break
            }
        }
    });
}

#[derive(Default)]
struct SizeEstimate {
    bytes: u64,
    files: u64,
    directories: u64,
    warnings: Vec<String>,
}

fn calculate_backup_size(
    sources: &[BackupSourceConfig],
    exclusions: &[String],
) -> Result<SizeEstimate, String> {
    if sources.is_empty() {
        return Err("Files to back up are required".into());
    }
    let mut estimate = SizeEstimate::default();
    for source in sources {
        if !source.path.is_absolute() {
            return Err("Backup source must be absolute".into());
        }
        calculate_path_size(&source.path, &source.path, exclusions, &mut estimate);
    }
    Ok(estimate)
}

fn calculate_path_size(
    source: &Path,
    path: &Path,
    exclusions: &[String],
    estimate: &mut SizeEstimate,
) {
    if path_is_excluded(source, path, exclusions) {
        return;
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            push_estimate_warning(estimate, format!("{}: {error}", path.display()));
            return;
        }
    };
    if metadata.file_type().is_symlink() {
        return;
    }
    if metadata.is_file() {
        estimate.files = estimate.files.saturating_add(1);
        estimate.bytes = estimate.bytes.saturating_add(metadata.len());
        return;
    }
    if metadata.is_dir() {
        estimate.directories = estimate.directories.saturating_add(1);
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) => {
                push_estimate_warning(estimate, format!("{}: {error}", path.display()));
                return;
            }
        };
        for entry in entries {
            match entry {
                Ok(entry) => calculate_path_size(source, &entry.path(), exclusions, estimate),
                Err(error) => push_estimate_warning(estimate, error.to_string()),
            }
        }
    }
}

fn push_estimate_warning(estimate: &mut SizeEstimate, warning: String) {
    if estimate.warnings.len() < 200 {
        estimate.warnings.push(warning);
    }
}

fn path_is_excluded(source: &Path, path: &Path, exclusions: &[String]) -> bool {
    let absolute = path.to_string_lossy();
    let relative = path
        .strip_prefix(source)
        .map(|path| path.to_string_lossy())
        .unwrap_or_else(|_| absolute.clone());
    let relative = relative.trim_start_matches('/');
    exclusions.iter().any(|pattern| {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return false;
        }
        if pattern.contains('*') {
            return wildcard_match(pattern, &absolute)
                || wildcard_match(pattern.trim_start_matches('/'), relative);
        }
        if pattern.starts_with('/') && Path::new(pattern).is_absolute() {
            let pattern = pattern.trim_end_matches('/');
            return absolute == pattern || absolute.starts_with(&format!("{pattern}/"));
        }
        let relative_pattern = pattern.trim_start_matches('/').trim_end_matches('/');
        relative == relative_pattern || relative.starts_with(&format!("{relative_pattern}/"))
    })
}

fn format_integer(value: u64) -> String {
    let text = value.to_string();
    let mut output = String::new();
    for (index, character) in text.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            output.push(',');
        }
        output.push(character);
    }
    output.chars().rev().collect()
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let parts = pattern.split('*').collect::<Vec<_>>();
    if parts.len() == 1 {
        return pattern == value;
    }
    let mut remainder = value;
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if index == 0 && !remainder.starts_with(part) {
            return false;
        }
        let Some(position) = remainder.find(part) else {
            return false;
        };
        remainder = &remainder[position + part.len()..];
    }
    parts
        .last()
        .is_none_or(|part| part.is_empty() || value.ends_with(part))
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
        activity_widgets.details_row.set_visible(false);
        activity_widgets.details_row.set_expanded(false);
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
                                append_log_line(
                                    &activity_widgets.log_buffer,
                                    &format!("{status} in {:.1}s", output.elapsed.as_secs_f32()),
                                );
                                activity_widgets.details_row.set_visible(
                                    matches!(kind, OperationKind::DryRun)
                                        || activity.warnings > 0
                                        || !output.success(),
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
                            activity_widgets.details_row.set_visible(true);
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
        OperationKind::DryRun | OperationKind::Backup => {
            let mut command = client
                .backup(&profile_from_form(widgets, &repository)?)
                .map_err(|error| error.to_string())?;
            if matches!(kind, OperationKind::Estimate | OperationKind::DryRun) {
                command.arguments.push(OsString::from("--dry-run"));
                command.arguments.push(OsString::from("true"));
            }
            Ok(command)
        }
        OperationKind::Estimate => unreachable!("Estimate uses the local size calculator"),
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
    sync_legacy_source_fields(widgets);
    let backup_id = widgets.backup_id.text().trim().to_owned();
    let sources = widgets.backup_sources.borrow().clone();
    if sources.is_empty() {
        return Err("Files to back up are required".into());
    }
    if backup_id.is_empty() {
        return Err("Backup ID is required".into());
    }

    let first_source = sources.first().expect("sources checked as non-empty");
    let exclusions = exclusion_patterns(widgets);

    let profile = BackupProfile {
        id: "manual".into(),
        name: "Manual Backup".into(),
        repository: repository.to_owned(),
        namespace: None,
        backup_id,
        archive_name: first_source.archive_name.clone(),
        source: first_source.path.clone(),
        sources: sources
            .into_iter()
            .map(|source| BackupSource {
                archive_name: source.archive_name,
                path: source.path,
            })
            .collect(),
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
