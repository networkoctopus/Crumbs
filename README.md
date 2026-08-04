# Crumbs

Crumbs is a friendly Proxmox Backup Server client for the GNOME desktop. It is
intended for image-based Linux desktops where the operating system can be
replaced or rolled back while the user's home directory remains persistent.

Crumbs follows Pika Backup's product model: protect personal files, schedule
jobs through a lightweight background monitor, and delegate the backup format
and transport to a proven command-line engine. Its engine is
`proxmox-backup-client`; Crumbs does not implement another backup format.

## Current state

- Rust/GTK4/libadwaita application shell
- Profile, retention, schedule, and exclusion domain models
- Typed PBS command adapter
- Unit tests for validation and generated arguments
- Initial desktop, icon, and development Flatpak metadata

The setup flow, Secret Service integration, background monitor, subprocess
execution, and restore browser are the next milestones.

## Architecture

```text
Crumbs UI
├── profiles and preferences
├── backup activity
├── snapshot and restore views
├── schedule monitor
└── PBS adapter
    └── proxmox-backup-client
```

The adapter constructs argument arrays and never invokes a shell. Credentials
are absent from profiles and command specifications; the future executor will
retrieve them from Secret Service and expose them only to the child process.

## Development

Test the core without GTK:

```bash
cargo test --no-default-features
```

Run the app on a system with GTK4 and libadwaita development packages:

```bash
cargo run
```

The Flatpak manifest is for development and temporarily permits Cargo network
access. A release must pin dependencies and provide generated offline sources.

The provisional application ID is `io.github.networkoctopus.Crumbs`. No
project licence has been selected yet.

