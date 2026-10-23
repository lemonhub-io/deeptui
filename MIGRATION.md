# Required migration to DeepTUI v0.1.0-beta

DeepTUI **v0.1.0-beta is the only supported release**. The previous ragent
releases v0.1.0 through v0.6.0, their download assets, and their tags have been
removed. Their larger version numbers belong to the retired release sequence.

## Existing Linux and macOS installations

Run the required-version installer:

```sh
curl -fsSL https://raw.githubusercontent.com/lemonhub-io/deeptui/v0.1.0-beta/install.sh | bash
```

Use the same installation directory as before. For a custom directory:

```sh
curl -fsSL https://raw.githubusercontent.com/lemonhub-io/deeptui/v0.1.0-beta/install.sh | bash -s -- --bin-dir /your/bin
```

The installer verifies the download checksum and exact `0.1.0-beta` binary
version before replacing a working installation. It then:

- Keeps existing `deeptui/config.toml` and `deeptui/stats.tsv` files.
- Copies legacy `ragent/config.toml` and `ragent/stats.tsv` to their DeepTUI
  locations when the new files do not exist. Migrated files have mode `0600`.
- Replaces a recognized retired `ragent` binary in that same install directory
  with a launcher that executes the adjacent `deeptui` binary. Existing command
  arguments and `RAGENT_*` model, endpoint, key, and stats overrides continue to
  work through this launcher; `DEEPTUI_*` overrides take precedence.

Verify the result:

```sh
deeptui --version       # deeptui 0.1.0-beta
ragent --version        # deeptui 0.1.0-beta, if the old binary was migrated
```

The legacy source files remain in place for user reference. Existing DeepTUI
configuration always wins. Do not run an old binary from another directory;
remove that copy through its original package manager or deployment system.

For an older CPU or a platform without a prebuilt asset, add `--from-source`.
This requires Rust and builds the beta from its source tag.

## Windows

Download the `deeptui-v0.1.0-beta-x86_64-pc-windows-msvc.zip` asset from the
[v0.1.0-beta release](https://github.com/lemonhub-io/deeptui/releases/tag/v0.1.0-beta).
Verify the archive against `SHA256SUMS.txt`, extract it, and put `deeptui.exe` on
PATH. Remove the retired `ragent.exe` from PATH. Copy the existing ragent config
and data into the corresponding deeptui directories if no DeepTUI files exist.

## Managed installations and enforcement limits

Previously downloaded binaries perform optional update checks, which users can
disable. They call the configured model provider directly and have no controlled
backend that can reject an old client. Deleting releases cannot disable those
binaries or erase copies already on someone else's machine.

Device administrators can require the upgrade through their existing software
deployment system: deploy the beta, replace or remove retired binaries, and
verify `deeptui --version`. The legacy updater cannot perform the version reset:
it considers `0.1.0-beta` older than the retired stable versions, so users must
run the new installer instead of `ragent --self-update`.

The new installer and updater reject unsupported target versions. This is an
installation/support policy, not a remote kill switch for old downloaded copies.
