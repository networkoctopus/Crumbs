# Proxmox Backup Client Feature Map

This note maps `proxmox-backup-client` setup, options, and environment
variables into Crumbs UI surfaces. It is based on the Proxmox Backup Server
4.2.x client documentation and should be reviewed when the bundled client
version changes.


## MVP Progress

The first usable milestone is a manual home-folder backup to PBS from a GNOME
Flatpak app. Restore should be available early enough that users can prove their
backups are useful.

Status legend:

- Done: implemented and covered by local tests or real PBS smoke tests.
- Partial: model or command support exists, but UI/executor/packaging work remains.
- Next: the next implementation slice.
- Later: deliberately deferred until the manual-backup MVP works.

1. Core PBS correctness: Partial
   - Done: retention modes are modeled as server-managed, client-managed, and disabled.
   - Done: client-managed retention uses positive counts only, so Crumbs cannot emit `--keep-* 0`.
   - Done: backup, prune, restore, pattern restore, snapshot list, snapshot files, status, and version command specs exist.
   - Done: metadata incremental backup, full restore, pattern restore, prune dry-run, and encrypted backup/restore were verified against a real PBS instance.
   - Done: `PBS_PASSWORD`, optional `PBS_ENCRYPTION_PASSWORD`, and profile-required `PBS_FINGERPRINT` are modeled as runtime credentials.
   - Next: add `login` only if Crumbs deliberately supports PBS ticket storage.

2. Profile persistence: Partial
   - Done: profile documents save and load as JSON.
   - Done: profiles have stable IDs and duplicate IDs are rejected.
   - Done: profile files contain no secrets.
   - Done: source folder, exclusions, namespace, backup ID, archive name, retention, change detection, and encryption settings serialize.
   - Next: choose the production config path, likely `$XDG_CONFIG_HOME/crumbs/profiles.json`.
   - Later: add version migrations beyond document version 1.

3. Secrets: Partial
   - Done: the first GUI keeps PBS password and fingerprint in memory and injects them into child process environments only.
   - Next: add Secret Service integration.
   - Next: store PBS password or API token secret, certificate fingerprint, and encryption password by profile ID.
   - Later: consider file-descriptor based password passing where PBS supports it.

4. Subprocess executor: Partial
   - Done: run `proxmox-backup-client` from `CommandSpec` without a shell.
   - Done: capture stdout, stderr, exit status, and elapsed time.
   - Done: return structured success/failure results.
   - Done: basic executor tests cover command execution and environment injection.
   - Done: support cancellation by terminating the running child process through a cancellation token.
   - Done: parse observed PBS output into compact activity status, progress, warning count, and estimates.

5. Flatpak packaging: Partial
   - Done: the app builds and installs as a Flatpak.
   - Done: a local ignored static `proxmox-backup-client` is available for development testing.
   - Done: the Flatpak manifest bundles pinned `proxmox-backup-client-static` 4.2.3-1 and verifies its checksum.
   - Done: the installed Flatpak can run the bundled client and reach the direct PBS endpoint.
   - Later: remove dev Cargo network access and add generated offline Cargo sources.
   - Later: add AppStream/metainfo and validate appstream/desktop/icon metadata.

6. Setup UI: Partial
   - Done: first libadwaita form captures server, password, fingerprint, source folder, backup ID, archive name, and exclusions.
   - Done: backup and restore folders can be selected with GTK folder pickers instead of manual path entry.
   - Done: Check Server validates PBS access through the bundled client.
   - Done: the home screen is organized around PBS-shaped Servers, Backups, and Restore sections.
   - Done: the home screen uses Pika-inspired overview rows with icons and backup/server previews.
   - Done: Add Server opens a modal setup flow instead of expanding fields on the home page.
   - Done: named servers can be added repeatedly to the in-session server list, shown on the home page, and selected from Backup or Restore.
   - Done: Servers and Backups sections keep a full-width centered plus button at the bottom for adding more items.
   - Done: server and backup rows expose delete actions in the in-session lists.
   - Done: server, backup, and restore rows are activatable; clicking the row opens that item while trash stays as a separate suffix action.
   - Done: backup creation and restore entry are disabled while no server is configured.
   - Done: delete and cancel actions ask for confirmation before continuing.
   - Done: Backup targets open a detail view with Backup and Schedule tabs.
   - Done: backup settings can be saved repeatedly in-session so additional backup rows appear on the home page.
   - Next: save non-secret server and backup fields through profile persistence.
   - Next: add Secret Service persistence for credentials.
   - Later: expand setup into a proper guided Pika/Deja Dup-style flow with namespace, encryption, and retention controls.

7. Manual backup UI: Partial
   - Done: first GUI provides Check Connection, Estimate, Dry Run, Back Up Now, and details.
   - Done: Backup controls now live in a dedicated Backup tab with server, source, archive, and exclusion groups.
   - Done: Backup and Restore actions expose confirmed Cancel controls for running operations.
   - Done: dry run and backup use metadata change detection and server-managed retention.
   - Done: raw PBS output is tucked into Details in the Backup tab while the main view shows compact status, progress, processed/uploaded amounts, and warnings.
   - Next: show last backup status and profile overview.
   - Next: run client-managed prune after successful backup when enabled.

8. Restore UI: Partial
   - Done: command specs and real PBS restore tests exist.
   - Done: snapshot list and snapshot file parsers are covered by local tests.
   - Done: first GUI slice lists snapshots, lists restorable pxar archives, accepts a restore destination, and restores all or selected paths.
   - Done: restore now combines server selection, snapshot browsing, archive selection, destination selection, and restore execution in one Restore page.
   - Done: restore destination can be selected with a GTK folder picker so Flatpak can grant the chosen output folder.
   - Done: restore has its own activity/progress/details section.
   - Done: archives refresh automatically when snapshots load or the selected snapshot changes, without success-toast spam or Activity panel noise.
   - Next: verify portal-granted restore destinations across host folders and document any sandbox limits.
   - Next: replace free-text restore patterns with a browsable snapshot file/tree picker.
   - Next: surface restore failures and warnings in a friendlier summary.
   - Later: add encrypted/keyfile restore controls, namespace selection, and optional FUSE mounting.

9. Scheduler and monitor: Later
   - Partial: the GUI now has a Schedule tab with disabled MVP placeholders for automatic backup and retention controls.
   - Later: split into app/common/monitor binaries like Pika.
   - Later: use the Background Portal for scheduled backups.
   - Later: add battery, metered-network, retry, and notification behavior.

## Real PBS Test Notes

A real PBS instance is configured locally for development in `local/pbs-test.env`.
The `local/` directory is ignored by git. A safe template lives at
`local.example/pbs-test.env.example`.

Verified against the direct PBS endpoint:

- `status --output-format json`
- metadata-mode initial backup
- metadata-mode no-change incremental backup
- metadata-mode changed-file incremental backup
- exclusion handling
- snapshot listing
- snapshot files listing
- full restore
- pattern restore
- GUI restore command wiring with snapshot/archive JSON parsing
- prune dry-run
- encrypted backup and restore with an unprotected local test key

Known test caveat:

- The proxy endpoint on port 443 presents a different TLS fingerprint than the
  direct PBS endpoint, so the current test fingerprint works for the direct
  endpoint only.

## MVP Setup Fields

These should be first-class fields in the setup flow.

- Profile name
- Server address
- Port, defaulting to `8007`
- Datastore
- Authentication ID, supporting both `user@realm` and `user@realm!token`
- Password or API token secret, stored in Secret Service
- Certificate fingerprint, optional but important for self-signed PBS setups
- Namespace, optional
- Backup ID, defaulting to the local hostname or a user-friendly machine name
- Archive name, defaulting to `home`
- Source folder, defaulting to the user's home directory
- Exclusion list, seeded with desktop-safe defaults
- Encryption mode: encrypt, sign-only, or none
- Encryption password, required when using a password-protected client key

Crumbs should prefer constructing `--repository` from the structured fields for
predictability, but it should still be able to display the equivalent PBS
repository string:

```text
[[auth-id@]server[:port]:]datastore
```

## Backup Options

Friendly controls:

- Dry run: expose as a checkbox before starting a backup.
- Change detection mode: expose as an advanced selector.
  - `metadata` is a good default for desktop backups.
  - `data` is slower but more thorough.
  - `legacy` exists for compatibility.
- Include all mounted subdirectories: advanced checkbox for `--all-file-systems`.
- Include mounted path: advanced repeatable field for `--include-dev`.
- Chunk size: advanced numeric selector, constrained to powers of two from 64 to
  4096 KiB.
- Entries max: advanced numeric field for very large trees.
- Upload bandwidth limit: advanced size field for `--rate`.
- Burst limit: advanced size field for token bucket burst behavior.
- Backup type: keep internal as `host` for Crumbs.
- Backup time: keep internal, normally set by the client/current time.
- Output format: keep internal as JSON where Crumbs parses output.

Archive specifications should eventually support more than one archive per
backup job, but the MVP should use one `pxar` archive:

```text
home.pxar:/home/user
```

Later archive types:

- `pxar` for file trees
- `img` for block devices, likely outside the Flatpak-only MVP
- `conf` and `log` for specialized uploads

## Exclusions

PBS supports `.pxarexclude` files and repeated `--exclude` CLI patterns.
Crumbs should start with repeated `--exclude` arguments generated from the
profile and later add a UI affordance for writing or importing `.pxarexclude`
rules.

Useful UI groups:

- Caches
- Trash
- Build artifacts
- Virtual machines and containers
- Downloads, optional because users disagree about this one
- Custom patterns

## Environment And Secrets

Secrets must never be stored in profile files or displayed in logs.

Runtime environment values Crumbs should inject:

- `PBS_PASSWORD`
- `PBS_ENCRYPTION_PASSWORD`
- `PBS_FINGERPRINT`
- `ALL_PROXY`, only if the user configures a proxy

Crumbs should avoid storing these in long-lived process environments. Prefer
per-child environment injection, and later consider file-descriptor based
variants if the executor grows that capability:

- `PBS_PASSWORD_FD`
- `PBS_ENCRYPTION_PASSWORD_FD`

Non-secret environment variables that can be represented as profile fields:

- `PBS_REPOSITORY`
- `PBS_SERVER`
- `PBS_PORT`
- `PBS_DATASTORE`
- `PBS_AUTH_ID`
- `PBS_NAMESPACE`

Crumbs should pass command-line arguments for these values rather than relying
on ambient environment, so command specs are self-contained and testable.

Debug logging:

- `PBS_LOG` controls client logging verbosity.
- `PXAR_LOG` controls pxar logging verbosity.

These belong in diagnostics/developer settings, not the normal setup flow.

## Connection Checks

Setup should validate a profile before saving it.

Useful commands:

- `version --output-format json`
- `status --output-format json`
- `login`, only if ticket storage is deliberately wanted

For a Flatpak desktop app, Crumbs should probably avoid relying on persistent
PBS login tickets for the MVP and instead use Secret Service plus per-command
environment injection.

## Retention And Prune

Retention should be separately enabled, like Pika's prune settings. Crumbs should support three retention modes:

- Server managed: Crumbs does not run prune. PBS datastore or prune-job policy handles retention.
- Client managed: Crumbs runs `proxmox-backup-client prune` for this backup group after successful backups.
- Disabled: no pruning is triggered by Crumbs, and the user has not told Crumbs that PBS handles it.

Server managed should be the safest default for users who already administer their PBS server. PBS supports datastore-level prune settings and dedicated prune jobs, including namespace scope and max-depth. Crumbs generally should not create or edit server prune jobs in the MVP because that requires broader server-management permissions and affects more than one desktop backup profile.

Client managed controls:

- Enable pruning after successful backup.
- Dry-run prune preview.
- Keep hourly.
- Keep last.
- Keep daily.
- Keep weekly.
- Keep monthly.
- Keep yearly.

PBS prune keep fields are positive counts. Crumbs should model disabled keep rules as `None` rather than emitting `--keep-* 0`.

The UI should offer presets first and advanced numeric fields second. It should also explain that pruning only removes snapshot metadata immediately; PBS garbage collection is what later frees unreferenced chunk data on the datastore.

## Restore Surface

Restore needs to be treated as a first-class workflow, not as a secondary detail of backups. The user should be able to prove that a backup is useful without leaving Crumbs.

MVP restore flow:

- List snapshots with `snapshot list --output-format json`.
- Filter by namespace, backup type, and backup ID.
- Show the archives contained in a selected snapshot with `snapshot files --output-format json`.
- Let the user choose an archive and a destination folder.
- Default to restoring into a newly created destination folder so existing files are not overwritten by accident.
- Run `restore <snapshot> <archive-name> <target>` and stream progress and errors.
- Offer a confirmation page even though restore itself does not have a `--dry-run` option.

Safety controls:

- Existing destination handling: expose `--allow-existing-dirs` as a checkbox with cautious wording.
- Overwrite behavior: keep `--overwrite` and more specific overwrite flags off by default.
- Ownership and permissions: for unprivileged desktop restores, offer advanced checkboxes for `--ignore-ownership`, `--ignore-permissions`, `--ignore-acls`, `--ignore-xattrs`, and `--ignore-extract-device-errors`.
- Pattern restore: expose repeated `--pattern` values when the user restores selected paths or glob matches instead of a full archive.
- Rate and burst limits: reuse the advanced network throttling controls from backups.

Archive inspection:

- `snapshot list` finds available snapshots.
- `snapshot files` lists top-level files in the snapshot, such as `home.pxar`, `catalog.pcat1`, and `index.json`.
- `restore <snapshot> index.json -` can retrieve machine-readable snapshot metadata via standard output.
- `catalog dump <snapshot>` can inspect the catalog, but its output is primarily useful for search and indexing rather than a polished file browser.

File-level restore:

- First implementation: restore selected paths with `restore --pattern`.
- Later implementation: use `catalog shell` behavior as a model for browsing, searching, selecting files, and restoring selected paths.
- Crumbs should hide shell-like terminology from the UI; present it as snapshots, folders, files, search, selected files, and restore destination.

Destination design:

- Default destination should be outside the original source tree, for example a folder chosen by the user under Downloads or a temporary restore folder.
- Restoring back into the original home folder should require an explicit confirmation and should keep overwrite options visible.
- The UI should summarize what will happen before starting: snapshot, archive, selected paths or full archive, destination, overwrite behavior, and metadata handling.

Later restore/browse features:

- Mount archives via FUSE, if Flatpak permissions and bundled tools allow it.
- Open mounted archives in the file manager.
- Unmount archives from Crumbs.
- Task log viewer.
- Task stop/cancel action.

FUSE mount flow, later:

- Create an app-owned mount point.
- Run `mount <snapshot> <archive-name> <mountpoint>`.
- Open the mount point read-only in the file manager.
- Track active mounts and expose an unmount action.
- Run `umount <mountpoint>` or the appropriate unmount command available in the Flatpak or runtime environment.

FUSE may be difficult in a Flatpak. The MVP should not depend on it; direct restore to a user-chosen folder is the safer first usable path.

## Flatpak Packaging Implications

Crumbs needs a deliberate answer for `proxmox-backup-client` availability.

Preferred path:

- Bundle the statically linked `proxmox-backup-client` if licensing and release
  packaging allow it.

Fallback path:

- Use host integration only for development or advanced users.

Flatpak permissions likely needed for the MVP:

- Network access
- Read-only access to selected source folders
- Secret Service access
- Background portal access once scheduling exists

Full-system or block-device backups are outside a pure Flatpak MVP and would
require a privileged host helper or system service.
