use adw::prelude::*;
use gtk::{gio, glib};

const APP_ID: &str = "io.github.networkoctopus.Crumbs";

pub fn run() -> glib::ExitCode {
    let application = adw::Application::builder().application_id(APP_ID).build();
    application.connect_activate(build_ui);
    application.run()
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
    let title = gtk::Label::builder()
        .label("Keep your files safe")
        .css_classes(["title-1"])
        .halign(gtk::Align::Center)
        .build();
    let description = gtk::Label::builder()
        .label("Back up your home folder to Proxmox Backup Server.")
        .css_classes(["dim-label"])
        .halign(gtk::Align::Center)
        .justify(gtk::Justification::Center)
        .wrap(true)
        .build();
    let add_button = gtk::Button::builder()
        .label("Set Up a Backup")
        .css_classes(["suggested-action", "pill"])
        .halign(gtk::Align::Center)
        .build();

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .margin_top(48)
        .margin_bottom(48)
        .margin_start(24)
        .margin_end(24)
        .build();
    let icon = gtk::Image::from_icon_name(APP_ID);
    icon.set_pixel_size(128);
    content.append(&icon);
    content.append(&title);
    content.append(&description);
    content.append(&add_button);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&content));
    let toast_overlay_weak = toast_overlay.downgrade();
    add_button.connect_clicked(move |_| {
        if let Some(toast_overlay) = toast_overlay_weak.upgrade() {
            toast_overlay.add_toast(adw::Toast::new("Backup setup is the next milestone"));
        }
    });

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toast_overlay));

    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("Crumbs")
        .default_width(720)
        .default_height(520)
        .content(&toolbar)
        .build();
    window.present();
}

