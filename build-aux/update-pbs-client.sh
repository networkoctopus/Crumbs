#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
PINS_FILE="${ROOT_DIR}/build-aux/pbs-client.env"
MANIFEST="${ROOT_DIR}/io.github.networkoctopus.Crumbs.Devel.yml"
PATCH_FILE="${ROOT_DIR}/build-aux/pbs-client-path-dependencies.patch"
PAGE_SIZE_PATCH_FILE="${ROOT_DIR}/build-aux/pbs-client-page-size.patch"
LOCK_FILE="${ROOT_DIR}/build-aux/pbs-client.Cargo.lock"
METAINFO="${ROOT_DIR}/data/io.github.networkoctopus.Crumbs.metainfo.xml"
THIRD_PARTY_NOTICES="${ROOT_DIR}/THIRD_PARTY_NOTICES.md"

# The fingerprint published for the Proxmox Debian 13 (Trixie) archive key.
PROXMOX_KEY_FINGERPRINT=24B30F06ECC1836A4E5EFECBA7BCD1420BFE778E
PBS_CLIENT_REPOSITORY=http://download.proxmox.com/debian/pbs-client
PBS_CLIENT_DISTRIBUTION=trixie
PBS_CLIENT_COMPONENT=main

for command in awk cargo curl dpkg git gpg gpgv gzip python3 sha256sum; do
    if ! command -v "${command}" >/dev/null 2>&1; then
        echo "Missing required command: ${command}" >&2
        exit 1
    fi
done

# shellcheck source=build-aux/pbs-client.env
source "${PINS_FILE}"

WORK_DIR=$(mktemp -d "${TMPDIR:-/tmp}/crumbs-pbs-update.XXXXXX")
trap 'rm -rf "${WORK_DIR}"' EXIT

echo "Checking the signed Proxmox client-only repository..."
curl -fsSL \
    https://enterprise.proxmox.com/debian/proxmox-archive-keyring-trixie.gpg \
    -o "${WORK_DIR}/proxmox-archive-keyring.gpg"

if ! gpg --batch --show-keys --with-colons "${WORK_DIR}/proxmox-archive-keyring.gpg" \
    | awk -F: '$1 == "fpr" { print $10 }' \
    | grep -Fx "${PROXMOX_KEY_FINGERPRINT}" >/dev/null; then
    echo "The downloaded Proxmox archive key has an unexpected fingerprint." >&2
    exit 1
fi

curl -fsSL \
    "${PBS_CLIENT_REPOSITORY}/dists/${PBS_CLIENT_DISTRIBUTION}/InRelease" \
    -o "${WORK_DIR}/InRelease"
gpgv --keyring "${WORK_DIR}/proxmox-archive-keyring.gpg" "${WORK_DIR}/InRelease"

PACKAGES_PATH="${PBS_CLIENT_COMPONENT}/binary-amd64/Packages.gz"
PACKAGES_SHA256=$(awk -v path="${PACKAGES_PATH}" \
    '$3 == path && length($1) == 64 { print $1; exit }' "${WORK_DIR}/InRelease")
if [[ -z "${PACKAGES_SHA256}" ]]; then
    echo "Could not find ${PACKAGES_PATH} in the signed repository metadata." >&2
    exit 1
fi

curl -fsSL \
    "${PBS_CLIENT_REPOSITORY}/dists/${PBS_CLIENT_DISTRIBUTION}/${PACKAGES_PATH}" \
    -o "${WORK_DIR}/Packages.gz"
printf '%s  %s\n' "${PACKAGES_SHA256}" "${WORK_DIR}/Packages.gz" | sha256sum --check --status

mapfile -t AVAILABLE_VERSIONS < <(
    gzip -dc "${WORK_DIR}/Packages.gz" \
        | awk 'BEGIN { RS=""; FS="\n" }
            {
                package="";
                version="";
                for (i=1; i<=NF; i++) {
                    if ($i ~ /^Package: /) package=substr($i, 10);
                    if ($i ~ /^Version: /) version=substr($i, 10);
                }
                if (package == "proxmox-backup-client") print version;
            }'
)

if [[ ${#AVAILABLE_VERSIONS[@]} -eq 0 ]]; then
    echo "The stable repository contains no proxmox-backup-client package." >&2
    exit 1
fi

LATEST_DEBIAN_VERSION=${AVAILABLE_VERSIONS[0]}
for version in "${AVAILABLE_VERSIONS[@]:1}"; do
    if dpkg --compare-versions "${version}" gt "${LATEST_DEBIAN_VERSION}"; then
        LATEST_DEBIAN_VERSION=${version}
    fi
done
LATEST_VERSION=${LATEST_DEBIAN_VERSION%-*}

echo "Bundled PBS client: ${PBS_CLIENT_DEBIAN_VERSION}"
echo "Stable PBS client:  ${LATEST_DEBIAN_VERSION}"

if [[ "${LATEST_DEBIAN_VERSION}" == "${PBS_CLIENT_DEBIAN_VERSION}" ]]; then
    echo "Crumbs already bundles the latest stable PBS client."
    exit 0
fi

if dpkg --compare-versions "${LATEST_DEBIAN_VERSION}" lt "${PBS_CLIENT_DEBIAN_VERSION}"; then
    echo "Refusing to downgrade PBS from ${PBS_CLIENT_DEBIAN_VERSION} to ${LATEST_DEBIAN_VERSION}." >&2
    exit 1
fi

echo "Finding the tested ARM source-pin set for ${LATEST_DEBIAN_VERSION}..."
git clone --quiet https://github.com/wofferl/proxmox-backup-arm64.git "${WORK_DIR}/arm-pins"

PIN_HISTORY_COMMIT=""
while IFS= read -r commit; do
    candidate_build="${WORK_DIR}/build-${commit}.sh"
    git -C "${WORK_DIR}/arm-pins" show "${commit}:build.sh" > "${candidate_build}"
    if grep -Fx "PROXMOX_BACKUP_VER=\"${LATEST_DEBIAN_VERSION}\"" "${candidate_build}" >/dev/null; then
        PIN_HISTORY_COMMIT=${commit}
        PIN_BUILD_FILE=${candidate_build}
        break
    fi
done < <(
    git -C "${WORK_DIR}/arm-pins" log --all --reverse \
        -S "PROXMOX_BACKUP_VER=\"${LATEST_DEBIAN_VERSION}\"" \
        --format=%H -- build.sh
)

if [[ -z "${PIN_HISTORY_COMMIT}" ]]; then
    echo "No source-pin set for ${LATEST_DEBIAN_VERSION} is available in proxmox-backup-arm64 yet." >&2
    echo "The updater will try again on its next scheduled run." >&2
    exit 1
fi

read_pin() {
    local name=$1
    local value
    value=$(sed -n "s/^${name}=\"\([0-9a-f]\{40\}\)\".*/\1/p" "${PIN_BUILD_FILE}" | head -n 1)
    if [[ ! "${value}" =~ ^[0-9a-f]{40}$ ]]; then
        echo "Could not read ${name} from proxmox-backup-arm64 ${PIN_HISTORY_COMMIT}." >&2
        exit 1
    fi
    printf '%s\n' "${value}"
}

NEW_PBS_BACKUP_COMMIT=$(read_pin PROXMOX_BACKUP_GIT)
NEW_PROXMOX_COMMIT=$(read_pin PROXMOX_GIT)
NEW_PROXMOX_FUSE_COMMIT=$(read_pin PROXMOX_FUSE_GIT)
NEW_PXAR_COMMIT=$(read_pin PXAR_GIT)
NEW_PATHPATTERNS_COMMIT=$(read_pin PATHPATTERNS_GIT)

checkout_commit() {
    local url=$1
    local commit=$2
    local destination=$3
    git init --quiet "${destination}"
    git -C "${destination}" remote add origin "${url}"
    git -C "${destination}" fetch --quiet --depth=1 origin "${commit}"
    git -C "${destination}" checkout --quiet --detach FETCH_HEAD
}

SOURCE_ROOT="${WORK_DIR}/sources"
mkdir -p "${SOURCE_ROOT}"
checkout_commit https://git.proxmox.com/git/proxmox-backup.git \
    "${NEW_PBS_BACKUP_COMMIT}" "${SOURCE_ROOT}/proxmox-backup"
checkout_commit https://git.proxmox.com/git/proxmox.git \
    "${NEW_PROXMOX_COMMIT}" "${SOURCE_ROOT}/proxmox"
checkout_commit https://git.proxmox.com/git/proxmox-fuse.git \
    "${NEW_PROXMOX_FUSE_COMMIT}" "${SOURCE_ROOT}/proxmox-fuse"
checkout_commit https://git.proxmox.com/git/pxar.git \
    "${NEW_PXAR_COMMIT}" "${SOURCE_ROOT}/pxar"
checkout_commit https://git.proxmox.com/git/pathpatterns.git \
    "${NEW_PATHPATTERNS_COMMIT}" "${SOURCE_ROOT}/pathpatterns"

CHANGELOG_VERSION=$(sed -n '1s/^[^(]*(\([^)]*\)).*/\1/p' \
    "${SOURCE_ROOT}/proxmox-backup/debian/changelog")
if [[ "${CHANGELOG_VERSION}" != "${LATEST_DEBIAN_VERSION}" ]]; then
    echo "PBS commit ${NEW_PBS_BACKUP_COMMIT} identifies as ${CHANGELOG_VERSION}, not ${LATEST_DEBIAN_VERSION}." >&2
    exit 1
fi

# Keep the downstream index-reader fix explicit. If Proxmox changes these
# readers (or incorporates an equivalent fix), fail the update rather than
# silently publishing an unpatched client.
patch --forward --fuzz=0 --strip=1 --directory="${SOURCE_ROOT}/proxmox-backup" \
    --input="${PAGE_SIZE_PATCH_FILE}"

echo "Regenerating the dependency-routing patch and Cargo lockfile..."
python3 - "${SOURCE_ROOT}/proxmox-backup/Cargo.toml" <<'PY'
from pathlib import Path
import re
import sys

cargo_toml = Path(sys.argv[1])
text = cargo_toml.read_text()

# Keep only workspace members needed by the standalone backup client.
for member in ("proxmox-file-restore", "proxmox-restore-daemon"):
    text, count = re.subn(rf'^\s*"{re.escape(member)}",\n', "", text, flags=re.MULTILINE)
    if count != 1:
        raise SystemExit(f"Expected one {member} workspace member, found {count}")

# These server-only workspace dependencies require Debian libraries that are
# deliberately absent from the Flatpak SDK.
for dependency in ("apt-pkg-native", "handlebars"):
    text = re.sub(
        rf"^{re.escape(dependency)}\s*=.*\n",
        "",
        text,
        count=1,
        flags=re.MULTILINE,
    )

# The root package is the server. Its dependencies are unnecessary when Cargo
# builds only the proxmox-backup-client workspace member.
dependencies_start = text.find("\n[dependencies]\n")
overrides_start = text.find("\n# Local path overrides", dependencies_start)
if dependencies_start < 0 or overrides_start < 0:
    raise SystemExit("Could not locate the root dependency and local override sections")
text = text[:dependencies_start] + "\n" + text[overrides_start:]

# This is the client dependency set carried by the original Proxmox routing
# patch. Enable only overrides whose source directories are present.
local_dependencies = {
    "pbs-api-types",
    "proxmox-apt-api-types",
    "proxmox-async",
    "proxmox-auth-api",
    "proxmox-base64",
    "proxmox-borrow",
    "proxmox-compression",
    "proxmox-config-digest",
    "proxmox-fuse",
    "proxmox-http",
    "proxmox-http-error",
    "proxmox-human-byte",
    "proxmox-io",
    "proxmox-lang",
    "proxmox-log",
    "proxmox-notify",
    "proxmox-rate-limiter",
    "proxmox-router",
    "proxmox-s3-client",
    "proxmox-schema",
    "proxmox-section-config",
    "proxmox-serde",
    "proxmox-shared-memory",
    "proxmox-sortable-macro",
    "proxmox-sys",
    "proxmox-syslog-api",
    "proxmox-systemd",
    "proxmox-time",
    "proxmox-uuid",
    "proxmox-worker-task",
    "pathpatterns",
    "pxar",
}

lines = text.splitlines(keepends=True)
enabled = set()
override_pattern = re.compile(
    r'^#(?P<name>[a-zA-Z0-9_-]+)\s*=\s*\{\s*path\s*=\s*"(?P<path>[^"]+)"'
)
for index, line in enumerate(lines):
    match = override_pattern.match(line)
    if not match or match.group("name") not in local_dependencies:
        continue
    dependency_path = (cargo_toml.parent / match.group("path")).resolve()
    if not dependency_path.is_dir():
        raise SystemExit(f"Local dependency path is absent: {dependency_path}")
    lines[index] = line[1:]
    enabled.add(match.group("name"))

# PBS 4.2.3 introduced proxmox-syslog-api without a commented local override.
if "proxmox-syslog-api.workspace = true" in "".join(lines) and "proxmox-syslog-api" not in enabled:
    dependency_path = (cargo_toml.parent / "../proxmox/proxmox-syslog-api").resolve()
    if not dependency_path.is_dir():
        raise SystemExit(f"Local dependency path is absent: {dependency_path}")
    marker_index = lines.index("[patch.crates-io]\n") + 1
    lines.insert(
        marker_index,
        'proxmox-syslog-api = { path = "../proxmox/proxmox-syslog-api" }\n',
    )
    enabled.add("proxmox-syslog-api")

if len(enabled) < 20:
    raise SystemExit(f"Only {len(enabled)} local client dependencies were enabled")

cargo_toml.write_text("".join(lines))
PY

{
    printf '%s\n' \
        'From 0000000000000000000000000000000000000000 Mon Sep 17 00:00:00 2001' \
        'From: Crumbs update workflow <noreply@git.networkoctopus.com>' \
        'Subject: [PATCH] route PBS client dependencies to local source trees' \
        '' \
        'Generated from the Proxmox dependency-routing patch for the pinned PBS release.' \
        '---'
    # Zero context keeps the generated patch free of whitespace-only context
    # lines while remaining exact for the pinned source commit.
    git -C "${SOURCE_ROOT}/proxmox-backup" diff --unified=0 --binary -- Cargo.toml
} > "${PATCH_FILE}"

(
    # Run from the common source root, matching Flatpak Builder. This avoids
    # the Debian-only Cargo registry replacement in proxmox-backup/.cargo.
    cd "${SOURCE_ROOT}"
    REPOID="${NEW_PBS_BACKUP_COMMIT}" cargo generate-lockfile \
        --manifest-path=proxmox-backup/Cargo.toml
)
cp "${SOURCE_ROOT}/proxmox-backup/Cargo.lock" "${LOCK_FILE}"

python3 - \
    "${PINS_FILE}" "${MANIFEST}" "${METAINFO}" "${THIRD_PARTY_NOTICES}" \
    "${PBS_CLIENT_VERSION}" "${PBS_CLIENT_DEBIAN_VERSION}" \
    "${PBS_BACKUP_COMMIT}" "${PROXMOX_COMMIT}" "${PROXMOX_FUSE_COMMIT}" \
    "${PXAR_COMMIT}" "${PATHPATTERNS_COMMIT}" \
    "${LATEST_VERSION}" "${LATEST_DEBIAN_VERSION}" \
    "${NEW_PBS_BACKUP_COMMIT}" "${NEW_PROXMOX_COMMIT}" "${NEW_PROXMOX_FUSE_COMMIT}" \
    "${NEW_PXAR_COMMIT}" "${NEW_PATHPATTERNS_COMMIT}" <<'PY'
from pathlib import Path
import sys

(
    pins_path,
    manifest_path,
    metainfo_path,
    notices_path,
    old_version,
    old_debian_version,
    old_pbs,
    old_proxmox,
    old_fuse,
    old_pxar,
    old_pathpatterns,
    new_version,
    new_debian_version,
    new_pbs,
    new_proxmox,
    new_fuse,
    new_pxar,
    new_pathpatterns,
) = sys.argv[1:]

replacements = {
    old_pbs: new_pbs,
    old_proxmox: new_proxmox,
    old_fuse: new_fuse,
    old_pxar: new_pxar,
    old_pathpatterns: new_pathpatterns,
}

manifest = Path(manifest_path)
text = manifest.read_text()
for old, new in replacements.items():
    if old not in text:
        raise SystemExit(f"Expected old source pin {old} is absent from {manifest}")
    text = text.replace(old, new)
manifest.write_text(text)

Path(pins_path).write_text(
    "# Bundled Proxmox Backup Client release and its reproducible source pins.\n"
    "# Updated together by build-aux/update-pbs-client.sh.\n"
    f"PBS_CLIENT_VERSION={new_version}\n"
    f"PBS_CLIENT_DEBIAN_VERSION={new_debian_version}\n"
    f"PBS_BACKUP_COMMIT={new_pbs}\n"
    f"PROXMOX_COMMIT={new_proxmox}\n"
    f"PROXMOX_FUSE_COMMIT={new_fuse}\n"
    f"PXAR_COMMIT={new_pxar}\n"
    f"PATHPATTERNS_COMMIT={new_pathpatterns}\n"
)

metainfo = Path(metainfo_path)
text = metainfo.read_text()
old_label = f"Bundles Proxmox Backup Client {old_version}."
new_label = f"Bundles Proxmox Backup Client {new_version}."
if old_label in text:
    text = text.replace(old_label, new_label)
else:
    marker = "<p>Initial development Flatpak repository build.</p>"
    if marker not in text:
        raise SystemExit(f"Could not find the release description in {metainfo}")
    text = text.replace(marker, marker + f"\n        <p>{new_label}</p>", 1)
metainfo.write_text(text)

notices = Path(notices_path)
text = notices.read_text()
version_label = f"Proxmox Backup Server v{old_version}"
if version_label not in text:
    raise SystemExit(f"Expected {version_label} in {notices}")
text = text.replace(version_label, f"Proxmox Backup Server v{new_version}", 1)
for old, new in replacements.items():
    if old not in text:
        raise SystemExit(f"Expected old source pin {old} is absent from {notices}")
    text = text.replace(old, new)
notices.write_text(text)
PY

git -C "${ROOT_DIR}" diff --check
echo "Prepared a PBS client update to ${LATEST_DEBIAN_VERSION}."
echo "Pin metadata source: https://github.com/wofferl/proxmox-backup-arm64/commit/${PIN_HISTORY_COMMIT}"
