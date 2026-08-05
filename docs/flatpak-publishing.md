# Flatpak publishing

Crumbs can be built by GitHub Actions as a Flatpak and exported into the shared
`networkoctopus` OSTree Flatpak repository. Crumbs is one app published by that
repository; the repository identity is not app-specific.

## One-time GitHub setup

1. Create the shared Flatpak repository.

   Create a GitHub repository at `networkoctopus/flatpak`. In that repository,
   enable GitHub Pages with source **Deploy from a branch**, branch `gh-pages`,
   and folder `/ (root)`.

   The published remote URL is:

   ```text
   https://networkoctopus.github.io/flatpak/
   ```

2. Create a dedicated Flatpak signing key.

   Use a key that is only for signing this Flatpak repository. A no-passphrase
   key is simplest for CI because the private key is already protected by GitHub
   Actions secrets.

   ```bash
   gpg --quick-generate-key "Crumbs Flatpak <flatpak@networkoctopus.com>" ed25519 sign 2y
   gpg --list-secret-keys --keyid-format=long "Crumbs Flatpak"
   ```

   Copy the long key ID or fingerprint from the `sec` line.

3. Add the publishing token and signing key to GitHub Actions secrets.

   Create a fine-grained GitHub token that can write contents to the
   `networkoctopus/flatpak` repository, then add it as this Crumbs repository
   secret:

   ```text
   FLATPAK_PUBLISH_TOKEN
   ```

   Then add the signing key secrets.

   ```bash
   gpg --armor --export-secret-keys YOUR_KEY_ID > crumbs-flatpak-private.asc
   gh secret set FLATPAK_GPG_PRIVATE_KEY < crumbs-flatpak-private.asc
   gh secret set FLATPAK_GPG_KEY_ID --body YOUR_KEY_ID
   ```

   If the key has a passphrase, also add it as `FLATPAK_GPG_PASSPHRASE`. This
   is optional for a no-passphrase CI signing key.

   Keep an offline backup of the private key. Delete `crumbs-flatpak-private.asc`
   from your working directory after adding the secret.

4. Push to `main`.

   The `Flatpak` workflow will build the app, sign the repository, upload build
   artifacts, and deploy the repository to GitHub Pages.

## Installing from the GitHub Pages Flatpak repo

After the first successful signed deployment, install the remote with:

```bash
flatpak remote-add --user networkoctopus https://networkoctopus.github.io/flatpak/networkoctopus.flatpakrepo
flatpak install --user networkoctopus io.github.networkoctopus.Crumbs
flatpak run io.github.networkoctopus.Crumbs
```

If you previously used the app-specific `crumbs` remote, remove it after
switching:

```bash
flatpak remote-delete --user crumbs
```

Update later with:

```bash
flatpak update --user io.github.networkoctopus.Crumbs
```

## Local unsigned artifacts

Pull requests and runs without signing secrets still build an unsigned Flatpak
repository artifact and a single `.flatpak` bundle. Those artifacts are useful
for CI validation, but the public GitHub Pages remote is only deployed when the
GPG key secrets are present.

## Current caveat

The manifest currently allows Cargo network access during the Flatpak build.
That is acceptable for this GitHub-hosted development repository, but a future
Flathub submission should replace it with generated offline Cargo sources.
