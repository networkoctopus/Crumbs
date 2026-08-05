# Crumbs

Crumbs is a friendly Proxmox Backup Server client for the GNOME desktop. It is
intended for image-based Linux desktops where the operating system can be
replaced or rolled back while the user's home directory remains persistent.

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
access.

The provisional application ID is `io.github.networkoctopus.Crumbs`. No
project licence has been selected yet.

## Licensing

Crumbs bundles the official `proxmox-backup-client` binary in the Flatpak. That
component is distributed under the GNU Affero General Public License version 3
by Proxmox. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for the bundled
package version, source links, checksum, and license notice.


