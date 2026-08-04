#[cfg(feature = "gui")]
fn main() -> gtk::glib::ExitCode {
    crumbs::ui::run()
}

#[cfg(not(feature = "gui"))]
fn main() {
    eprintln!("Crumbs was built without its graphical interface");
}

