# Third-Party Notices

Crumbs bundles third-party software in its Flatpak build. This file documents
components that are redistributed with the application and are not authored by
the Crumbs project.

## Proxmox Backup Client

The Flatpak manifest downloads and installs the official static
`proxmox-backup-client` binary from the Proxmox Backup Server client package:

- Package: `proxmox-backup-client-static_4.2.3-1_amd64.deb`
- Download URL: <http://download.proxmox.com/debian/pbs-client/dists/trixie/main/binary-amd64/proxmox-backup-client-static_4.2.3-1_amd64.deb>
- SHA-256: `05ac991e89a6e899d3f236c15d13ba736221a44f138314f42cac22867ae46d55`
- Installed path in the Flatpak: `/app/bin/proxmox-backup-client`
- Upstream source repository: <https://git.proxmox.com/?p=proxmox-backup.git>
- GitHub read-only mirror: <https://github.com/proxmox/proxmox-backup>
- Upstream license declared by Proxmox: `AGPL-3`

Crumbs does not modify this binary. It is redistributed as a separate executable
that Crumbs invokes as a subprocess. If Crumbs ever ships a patched or rebuilt
version of `proxmox-backup-client`, the corresponding modified source and patch
set must be published alongside the distributed binary.

The GNU Affero General Public License version 3 text is included at
[LICENSES/AGPL-3.0.txt](LICENSES/AGPL-3.0.txt).

## Crumbs

Crumbs has not selected its own project license yet. The presence of the AGPLv3
license text in this repository documents the bundled Proxmox Backup Client; it
does not by itself declare the license for Crumbs' own source code.
