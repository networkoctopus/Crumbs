# Third-Party Notices

Crumbs bundles third-party software in its Flatpak build. This file documents
components that are redistributed with the application and are not authored by
the Crumbs project.

## Proxmox Backup Client

The Flatpak builds `proxmox-backup-client` from source for each target
architecture. The primary source is Proxmox Backup Server v4.2.0:

- Source repository: <https://git.proxmox.com/git/proxmox-backup.git>
- Git revision: `035c449897fafc228c8bbf3a5b5ba38564478ac7`
- Installed path in the Flatpak: `/app/bin/proxmox-backup-client`
- GitHub read-only mirror: <https://github.com/proxmox/proxmox-backup>
- Upstream license declared by Proxmox: `AGPL-3`

The build also uses these pinned Proxmox source repositories:

- `proxmox`: `22c4d5ecbfce6eb2fd566181e0b7d23ac2df4f0c`
- `proxmox-fuse`: `ac99ac97f7c2eb7ab9ee6ec3b41034e68b1eca7d`
- `pxar`: `091a8a382d0d6fc71025351fb35c51b1f3b0074d`
- `pathpatterns`: `5323cbe49ae5d592eb8a3fa2e215550e83dd7fba`

The manifest applies
[`build-aux/pbs-client-path-dependencies.patch`](build-aux/pbs-client-path-dependencies.patch),
originally published by the Arch User Repository package maintainers, to route
Proxmox crates that are unavailable on crates.io to the sibling source trees.
Its upstream SHA-256 is
`8d9198b3d8560659fca2e964b74f896f892d5709030b297cc99d64eb406f11ec`.
The patch changes dependency locations, not backup-client behaviour. The
resolved Rust dependency set is pinned in
[`build-aux/pbs-client.Cargo.lock`](build-aux/pbs-client.Cargo.lock).

The GNU Affero General Public License version 3 text is included at
[LICENSES/AGPL-3.0.txt](LICENSES/AGPL-3.0.txt).

## libfuse

The Flatpak builds the libfuse shared library from the `fuse-3.16.2` tag at
revision `7a92727d97c10290b3501d86a194738973edb61d`:

- Source repository: <https://github.com/libfuse/libfuse>
- License for the bundled library: `LGPL-2.1`

The upstream license notice and LGPLv2.1 text are installed with the
application under `/app/share/doc/crumbs/libfuse/`.

## Crumbs

Crumbs has not selected its own project license yet. The presence of the AGPLv3
license text in this repository documents the bundled Proxmox Backup Client; it
does not by itself declare the license for Crumbs' own source code.
