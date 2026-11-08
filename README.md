# Open File Mover CLI

#### v2.0.0 — rewritten in Rust

[![Unlicense](https://img.shields.io/badge/License-Unlicense-blue.svg)](https://unlicense.org/)

Move mountains of files from the command line.

`ofm` is a small, single-binary command-line tool for shifting large numbers of
files around on Windows: data migrations, camera dumps, backup wrangling, and
folders with tens of thousands of files in them.

```
ofm move D:\camera-dump E:\archive
```

## Why it is fast

A move inside one volume does not need to touch your data at all — it is a
directory entry update. `ofm` leans on that as hard as it can:

| What you ask for | What actually happens |
| --- | --- |
| Move a folder on the same drive | **One rename syscall**, whatever is inside it |
| Move files on the same drive | One rename per file, in parallel |
| Move to another drive or a share | Parallel streaming copy, then the original is removed |

Measured on the author's machine, moving a folder of **20,005 files**:

| Approach | Time |
| --- | --- |
| Per-file move (what most tools do) | 158 s |
| `ofm move` (whole-folder rename) | **6 ms** |

Cross-volume numbers from the same machine: 20,005 files / 100 MiB copied and
byte-for-byte verified in 13.4 s; 1.49 GiB of large files in 0.7 s
(2.2 GiB/s, NVMe to NVMe).

## Install

You need the [Rust toolchain](https://rustup.rs/) (and, on Windows, the MSVC
build tools that `rustup` points you at).

```bash
cargo install --path .
```

That puts `ofm` in `~/.cargo/bin`, which `rustup` adds to your `PATH`. To build
without installing:

```bash
cargo build --release
```

The binary lands at `target/release/ofm.exe` and is about 2.7 MB with no
runtime dependencies — no Python, no DLLs to ship.

## Commands

| Command | What it does |
| --- | --- |
| `ofm move <SRC>... <DEST>` | Move files and folders |
| `ofm copy <SRC>... <DEST>` | Same engine, originals left alone |
| `ofm trash <PATH>...` | Send to the Recycle Bin (undoable) |
| `ofm empty-recycle` | Permanently empty the Recycle Bin |
| `ofm clean-downloads` | Clear out the Downloads folder |
| `ofm doctor` | Environment, log location, Recycle Bin state |
| `ofm completions <SHELL>` | Generate a completion script |

Short aliases from v1 still work: `mv`, `cp`, `er`, `ed`, `move-folder`,
`empty-downloads`.

### Moving and copying

The destination is the last path, or `--to` if you prefer to be explicit:

```bash
ofm move C:\inbox D:\sorted
ofm move C:\inbox --to D:\sorted
ofm move C:\one C:\two C:\three --to D:\all-of-it
```

By default a source folder is moved *into* the destination, so
`ofm move C:\inbox D:\sorted` produces `D:\sorted\inbox`. To move what is
inside it instead, use `--contents`.

**Picking what moves**

```bash
# Only photos
ofm move C:\dump --to E:\photos --ext jpg,png,raw

# Only big, old files
ofm move C:\logs --to D:\cold --min-size 100MB --older-than 90d

# Everything except the junk
ofm copy C:\project --to D:\backup --exclude '**/node_modules/**' --exclude '*.tmp'

# Just the top level
ofm move C:\inbox --to D:\out --depth 0
```

`--ext`, `--include`, `--exclude`, `--min-size`, `--max-size`, `--newer-than`,
`--older-than`, `--depth` and `--skip-hidden` all compose. Sizes take `10MB`,
`1.5GiB`, `512k`; ages take `30d`, `6h`, `2w` or a `2026-01-31` date.

**Deciding where things land**

```bash
# Discard the folder structure
ofm move C:\dump --to E:\flat --flatten

# One folder per extension: E:\sorted\jpg\, E:\sorted\pdf\
ofm move C:\dump --to E:\sorted --group-by ext

# One folder per month: E:\sorted\2026-11\
ofm move C:\dump --to E:\sorted --group-by date
```

`--group-by` takes `ext`, `date` or `year`.

**When something is already there**

`--on-conflict` decides, and defaults to `rename`:

| Policy | Behaviour |
| --- | --- |
| `rename` (default) | Incoming file becomes `report (1).txt` |
| `skip` | Leave the existing file, leave the source where it is |
| `overwrite` | Replace it |
| `newer` | Replace it only if the source is newer |

Names are deduplicated across the whole run, so flattening two folders that
both contain `notes.txt` gives you `notes.txt` and `notes (1).txt` rather than
one file silently eating the other.

### Clearing out Downloads

```bash
ofm clean-downloads --dry-run              # always start here
ofm clean-downloads --older-than 30d       # only stale things
ofm clean-downloads --ext iso,msi,exe      # only installers
ofm clean-downloads --to D:\quarantine     # move instead of delete
ofm clean-downloads --permanent            # actually delete
```

The default sends entries to the **Recycle Bin**, not oblivion, and the real
Downloads location is read from the shell, so a relocated Downloads folder
works.

## Safety

Moving a lot of files is exactly when you least want a surprise.

- **`--dry-run` on everything.** The planner builds the full list of operations
  without touching anything, and the dry run prints that same list. It is a
  preview, not an estimate.
- **Nothing destructive happens silently.** `trash`, `empty-recycle` and
  `clean-downloads --permanent` ask first. With no terminal to ask (a script, a
  pipe) they refuse rather than guess; pass `--yes` when you mean it.
- **Deletion defaults to recoverable.** The Recycle Bin is the default; you
  have to ask for `--permanent`.
- **Moving a folder into itself is refused**, rather than producing infinite
  recursion or eating the source.
- **Hidden and system files come along.** A mover that quietly skipped `.git`
  or `desktop.ini` would leave you with half a folder. Use `--skip-hidden` if
  you want the other behaviour.
- **`--verify`** re-reads each copy and compares it byte for byte against the
  source *before* the original is removed. Worth it for cross-volume moves of
  anything you care about.
- **Ctrl-C stops cleanly** at the next file boundary, so you never get a
  half-written file at the destination. Press it twice to quit immediately.
- **Errors do not abort the run.** One unreadable file does not strand the
  other 19,999; everything that failed is listed at the end. `--fail-fast`
  reverses that.
- **Exit codes**: `0` success, `1` finished with failures, `2` bad usage or a
  fatal error, `130` cancelled.

## Scripting

`--json` prints one machine-readable document instead of the table:

```bash
ofm move C:\inbox --to D:\out --json
```

```json
{
  "command": "move",
  "dry_run": false,
  "files": 20005,
  "renamed": 20005,
  "copied": 0,
  "bytes": 0,
  "skipped": 0,
  "failed": 0,
  "elapsed_ms": 109,
  "workers": 16
}
```

Other useful global flags: `--quiet`, `--verbose`, `--no-progress`,
`--color never`, `--log <FILE>`, `--no-log`.

Activity is appended to
`%LOCALAPPDATA%\open-file-mover\activity.log` (rotated at 5 MB).
`ofm doctor` tells you exactly where.

## Performance notes

`ofm` picks its own worker count: one per CPU for same-volume renames, but
capped at 4 for cross-volume copies, where piling on threads makes a disk seek
rather than read. Override with `-j/--workers N`.

If a transfer is slower than you expect, `ofm doctor` and `--dry-run` together
will usually explain it — most often the move is crossing a volume boundary, or
a filter is preventing the whole-folder rename. `--no-fast-path` forces
per-file handling if you ever need it.

## Development

```bash
cargo test            # 107 tests
cargo clippy --all-targets
cargo build --release
```

Layout:

```
src/
  cli.rs            command-line surface (one source of truth for --help)
  engine/
    filters.rs      which files get picked up
    plan.rs         build the operation list; never mutates anything
    exec.rs         run it in parallel; rename, or copy + verify + remove
  commands/         one module per command family
  platform/         Windows shell API (Recycle Bin, Downloads, elevation)
  ui/               banner, colour, prompts, progress bar
  report.rs         the summary, human and JSON
  log.rs            activity log
  util.rs           sizes, ages, paths, volumes
tests/cli.rs        end-to-end tests driving the real binary
```

Planning and execution are kept apart on purpose: it is what makes `--dry-run`
exact, and it means the interesting logic is testable without moving real
files.

## What changed in v2

The v1 tool was a Python script. This is a full rewrite.

- **Rust, single binary.** No Python, no `pip install winshell`.
- **Built for bulk.** Whole-folder renames, parallel transfers, progress and
  throughput. v1 moved one folder per invocation with `shutil.move`.
- **Filtering and routing**: `--ext`, globs, size and age bounds, `--flatten`,
  `--group-by`, `--on-conflict`.
- **`--dry-run` and `--json`** on everything.
- **No more mandatory Administrator.** v1 refused to run unelevated even to
  move a file in your own home folder. Elevation is now only needed for files
  that genuinely require it, and `ofm doctor` reports your status.
- **`clean-downloads` is no longer a silent permanent delete.** It previews,
  asks, and recycles by default. v1 `rm -rf`'d your Downloads folder with no
  confirmation.
- **Logging that works.** v1 wrote to a `logs/` directory it never created, so
  logging raised an exception.

## Contributors

Thank you to everyone who worked on this project across its iterations:

@AustinCGomez
@burd5
@sorena-dev
@muddi900
@malikrohail

## License

Released into the public domain under the [Unlicense](https://unlicense.org/).
