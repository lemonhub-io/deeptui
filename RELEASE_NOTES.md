# DeepTUI v0.1.1-beta

This beta adds model setup and switching, resumable conversations, and more
controls for long-running coding tasks.

- **Model and provider picker:** discover compatible models from models.dev,
  paste a provider key into a masked field, and switch models with `/model`.
- **Resumable project sessions:** continue the latest conversation with
  `--continue` (`-c`), or choose one with `/resume`.
- **Slash commands:** manual compaction, undo/retry, context and cost reports,
  clipboard copy, and command completion in the composer.
- **Task limits:** configurable model-round and task-spending caps, repeated
  tool-result warnings, and a compact work log that survives context compaction.
- **More resilient tools:** repair near-JSON arguments, recover from whitespace
  and indentation mismatches in edits, preserve CRLF files, and improve shell
  output cleanup and Unix process-group cancellation.
- **Thinking-model support:** carry streamed reasoning between tool calls within
  the active turn, then remove it before saving the finished conversation.

## Install or upgrade

Linux and macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/lemonhub-io/deeptui/v0.1.1-beta/install.sh | bash
```

The installer verifies the archive checksum and exact binary version. It
preserves existing DeepTUI settings and usage history, and migrates recognized
legacy ragent installations. Use the original install directory with
`--bin-dir` if it differs from the default.

**Users of v0.1.0-beta must rerun the installer.** That release's updater is
pinned to v0.1.0-beta and cannot select this version. Existing downloaded
binaries have no remote enforcement mechanism.

Windows users can download the `x86_64-pc-windows-msvc.zip` asset below and verify
it against `SHA256SUMS.txt`. See [MIGRATION.md](https://github.com/lemonhub-io/deeptui/blob/v0.1.1-beta/MIGRATION.md)
for migration instructions.

Prebuilt downloads cover Linux x86_64 (GNU and static musl), Linux aarch64,
macOS x86_64/aarch64, and Windows x86_64. Source builds remain available through
`install.sh --from-source`.
