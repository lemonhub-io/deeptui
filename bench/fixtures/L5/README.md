# notes task

check.py defines the contract for a CLI note manager you must build.

- Implement `notes.py` (stdlib only) with subcommands:
  `add TEXT`, `list`, `search QUERY`, `delete ID`
- Storage: JSON list in the file given by env var `NOTES_FILE` (create if missing).
- IDs auto-increment starting at 1. `list` prints `ID: text` lines.
- Unknown/invalid commands exit non-zero.
- Also write `test_notes.py` with real unit tests covering all subcommands, and run them.
- Finish when `python3 check.py` prints ALL OK.
