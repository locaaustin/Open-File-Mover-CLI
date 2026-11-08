//! End-to-end tests that drive the real `ofm` binary as a subprocess, so the
//! argument parsing, exit codes and JSON output are all covered as the user
//! actually experiences them.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Path to the binary cargo just built next to the test executable.
fn binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("test executable has a path");
    path.pop(); // deps/
    path.pop(); // debug/
    path.push(format!("ofm{}", std::env::consts::EXE_SUFFIX));
    assert!(path.exists(), "expected the ofm binary at {path:?}");
    path
}

fn ofm(args: &[&str]) -> Output {
    Command::new(binary())
        .args(args)
        // Keep tests off the user's real activity log.
        .arg("--no-log")
        .arg("--no-progress")
        .arg("--color")
        .arg("never")
        .output()
        .expect("ofm should start")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn seed(root: &Path) {
    fs::create_dir_all(root.join("src/photos")).unwrap();
    fs::create_dir_all(root.join("src/docs")).unwrap();
    fs::write(root.join("src/readme.txt"), b"top level").unwrap();
    fs::write(root.join("src/photos/one.jpg"), b"jpeg-one").unwrap();
    fs::write(root.join("src/photos/two.jpg"), b"jpeg-two").unwrap();
    fs::write(root.join("src/docs/notes.txt"), b"notes").unwrap();
    fs::write(root.join("src/docs/sheet.csv"), b"a,b,c").unwrap();
}

fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[test]
fn help_and_version_succeed() {
    let help = ofm(&["--help"]);
    assert!(help.status.success());
    let text = stdout_of(&help);
    assert!(text.contains("Open File Mover"));
    assert!(text.contains("move"));
    assert!(text.contains("clean-downloads"));

    let version = ofm(&["--version"]);
    assert!(version.status.success());
    assert!(stdout_of(&version).contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn bare_invocation_prints_help_and_succeeds() {
    let output = ofm(&[]);
    assert!(output.status.success(), "a bare run should not be an error");
    assert!(stdout_of(&output).contains("Usage"));
}

#[test]
fn unknown_flags_exit_with_a_usage_code() {
    let output = ofm(&["move", "--nonsense"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn moving_a_folder_relocates_the_whole_tree() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let destination = dir.path().join("dest");
    fs::create_dir_all(&destination).unwrap();

    let output = ofm(&["move", &s(&dir.path().join("src")), &s(&destination)]);
    assert!(output.status.success(), "{}", stdout_of(&output));

    assert!(destination.join("src/photos/one.jpg").exists());
    assert!(destination.join("src/docs/sheet.csv").exists());
    assert_eq!(
        fs::read_to_string(destination.join("src/readme.txt")).unwrap(),
        "top level"
    );
    assert!(!dir.path().join("src").exists(), "source should be gone");
}

#[test]
fn a_missing_destination_directory_is_created() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let destination = dir.path().join("brand/new/place");

    let output = ofm(&[
        "move",
        &s(&dir.path().join("src")),
        "--to",
        &s(&destination),
        "--contents",
    ]);
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert!(destination.join("readme.txt").exists());
}

#[test]
fn dry_run_changes_nothing_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let destination = dir.path().join("dest");

    let output = ofm(&[
        "move",
        &s(&dir.path().join("src")),
        &s(&destination),
        "--dry-run",
    ]);
    assert!(output.status.success());
    let text = stdout_of(&output);
    assert!(text.contains("Dry run"), "{text}");
    assert!(dir.path().join("src/readme.txt").exists());
    assert!(!destination.exists());
}

#[test]
fn copy_keeps_the_originals() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let destination = dir.path().join("dest");

    let output = ofm(&["copy", &s(&dir.path().join("src")), &s(&destination)]);
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert!(dir.path().join("src/readme.txt").exists());
    assert!(destination.join("src/readme.txt").exists());
}

#[test]
fn extension_filter_moves_only_matching_files() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let destination = dir.path().join("dest");

    let output = ofm(&[
        "move",
        &s(&dir.path().join("src")),
        "--to",
        &s(&destination),
        "--ext",
        "jpg",
        "--flatten",
    ]);
    assert!(output.status.success(), "{}", stdout_of(&output));

    assert!(destination.join("one.jpg").exists());
    assert!(destination.join("two.jpg").exists());
    assert!(!destination.join("readme.txt").exists());
    // Everything that did not match stays where it was.
    assert!(dir.path().join("src/readme.txt").exists());
    assert!(dir.path().join("src/docs/notes.txt").exists());
}

#[test]
fn group_by_extension_sorts_into_buckets() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let destination = dir.path().join("sorted");

    let output = ofm(&[
        "move",
        &s(&dir.path().join("src")),
        "--to",
        &s(&destination),
        "--group-by",
        "ext",
    ]);
    assert!(output.status.success(), "{}", stdout_of(&output));

    assert!(destination.join("jpg/one.jpg").exists());
    assert!(destination.join("txt/readme.txt").exists());
    assert!(destination.join("csv/sheet.csv").exists());
}

#[test]
fn conflicting_names_are_numbered_by_default() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let destination = dir.path().join("dest");
    fs::create_dir_all(&destination).unwrap();
    fs::write(destination.join("readme.txt"), b"already here").unwrap();

    let output = ofm(&[
        "move",
        &s(&dir.path().join("src")),
        "--to",
        &s(&destination),
        "--contents",
        "--flatten",
    ]);
    assert!(output.status.success(), "{}", stdout_of(&output));

    assert_eq!(
        fs::read_to_string(destination.join("readme.txt")).unwrap(),
        "already here",
        "the existing file must not be clobbered"
    );
    assert_eq!(
        fs::read_to_string(destination.join("readme (1).txt")).unwrap(),
        "top level"
    );
}

#[test]
fn on_conflict_skip_leaves_the_source_in_place() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let destination = dir.path().join("dest");
    fs::create_dir_all(&destination).unwrap();
    fs::write(destination.join("readme.txt"), b"already here").unwrap();

    let output = ofm(&[
        "move",
        &s(&dir.path().join("src")),
        "--to",
        &s(&destination),
        "--contents",
        "--flatten",
        "--on-conflict",
        "skip",
    ]);
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert!(dir.path().join("src/readme.txt").exists());
    assert!(!destination.join("readme (1).txt").exists());
}

#[test]
fn on_conflict_overwrite_replaces_the_file() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let destination = dir.path().join("dest");
    fs::create_dir_all(&destination).unwrap();
    fs::write(destination.join("readme.txt"), b"stale").unwrap();

    let output = ofm(&[
        "move",
        &s(&dir.path().join("src")),
        "--to",
        &s(&destination),
        "--contents",
        "--flatten",
        "--on-conflict",
        "overwrite",
    ]);
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert_eq!(
        fs::read_to_string(destination.join("readme.txt")).unwrap(),
        "top level"
    );
}

#[test]
fn json_output_is_machine_readable() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let destination = dir.path().join("dest");

    let output = ofm(&[
        "move",
        &s(&dir.path().join("src")),
        &s(&destination),
        "--json",
    ]);
    assert!(output.status.success(), "{}", stdout_of(&output));

    let value: serde_json::Value = serde_json::from_str(&stdout_of(&output))
        .expect("the --json output should be a single JSON document");
    assert_eq!(value["command"], "move");
    assert_eq!(value["dry_run"], false);
    assert_eq!(value["failed"], 0);
}

#[test]
fn a_missing_source_is_an_error_with_exit_code_two() {
    let dir = tempfile::tempdir().unwrap();
    let output = ofm(&[
        "move",
        &s(&dir.path().join("does-not-exist")),
        &s(&dir.path().join("dest")),
    ]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error:"), "{stderr}");
}

#[test]
fn moving_a_folder_into_itself_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let source = dir.path().join("src");
    let output = ofm(&["move", &s(&source), &s(&source.join("inner"))]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("inside the source"));
    assert!(source.join("readme.txt").exists());
}

#[test]
fn a_destination_is_required() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let output = ofm(&["move", &s(&dir.path().join("src"))]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("destination"));
}

#[test]
fn exclude_globs_keep_folders_out() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    fs::create_dir_all(dir.path().join("src/node_modules/pkg")).unwrap();
    fs::write(dir.path().join("src/node_modules/pkg/index.js"), b"x").unwrap();
    let destination = dir.path().join("dest");

    let output = ofm(&[
        "copy",
        &s(&dir.path().join("src")),
        "--to",
        &s(&destination),
        "--exclude",
        "**/node_modules/**",
    ]);
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert!(destination.join("src/readme.txt").exists());
    assert!(!destination.join("src/node_modules/pkg/index.js").exists());
}

#[test]
fn min_size_filter_selects_large_files_only() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/small.bin"), vec![0u8; 10]).unwrap();
    fs::write(dir.path().join("src/large.bin"), vec![0u8; 5000]).unwrap();
    let destination = dir.path().join("dest");

    let output = ofm(&[
        "move",
        &s(&dir.path().join("src")),
        "--to",
        &s(&destination),
        "--min-size",
        "1k",
        "--flatten",
    ]);
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert!(destination.join("large.bin").exists());
    assert!(!destination.join("small.bin").exists());
    assert!(dir.path().join("src/small.bin").exists());
}

#[test]
fn verify_passes_on_a_real_copy() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    let payload: Vec<u8> = (0..300_000u32).map(|n| (n % 256) as u8).collect();
    fs::write(dir.path().join("src/big.bin"), &payload).unwrap();
    let destination = dir.path().join("dest");

    let output = ofm(&[
        "copy",
        &s(&dir.path().join("src")),
        "--to",
        &s(&destination),
        "--verify",
    ]);
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert_eq!(fs::read(destination.join("src/big.bin")).unwrap(), payload);
}

#[test]
fn many_files_move_correctly_under_parallelism() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    for n in 0..500 {
        fs::write(
            dir.path().join(format!("src/file-{n:04}.dat")),
            format!("payload {n}"),
        )
        .unwrap();
    }
    let destination = dir.path().join("dest");

    let output = ofm(&[
        "move",
        &s(&dir.path().join("src")),
        "--to",
        &s(&destination),
        "--contents",
        "--workers",
        "8",
    ]);
    assert!(output.status.success(), "{}", stdout_of(&output));

    let landed = fs::read_dir(&destination).unwrap().count();
    assert_eq!(landed, 500);
    assert_eq!(
        fs::read_to_string(destination.join("file-0321.dat")).unwrap(),
        "payload 321"
    );
}

#[test]
fn depth_limits_how_far_it_descends() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let destination = dir.path().join("dest");

    let output = ofm(&[
        "copy",
        &s(&dir.path().join("src")),
        "--to",
        &s(&destination),
        "--depth",
        "0",
    ]);
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert!(destination.join("src/readme.txt").exists());
    assert!(!destination.join("src/photos/one.jpg").exists());
}

#[test]
fn several_sources_land_in_one_destination() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("a")).unwrap();
    fs::create_dir_all(dir.path().join("b")).unwrap();
    fs::write(dir.path().join("a/one.txt"), b"1").unwrap();
    fs::write(dir.path().join("b/two.txt"), b"2").unwrap();
    let destination = dir.path().join("dest");

    let output = ofm(&[
        "move",
        &s(&dir.path().join("a")),
        &s(&dir.path().join("b")),
        "--to",
        &s(&destination),
    ]);
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert!(destination.join("a/one.txt").exists());
    assert!(destination.join("b/two.txt").exists());
}

#[test]
fn a_single_file_can_be_moved() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("lonely.txt"), b"solo").unwrap();
    let destination = dir.path().join("dest");

    let output = ofm(&["move", &s(&dir.path().join("lonely.txt")), &s(&destination)]);
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert_eq!(
        fs::read_to_string(destination.join("lonely.txt")).unwrap(),
        "solo"
    );
}

#[test]
fn clean_downloads_dry_run_touches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let downloads = dir.path().join("Downloads");
    fs::create_dir_all(&downloads).unwrap();
    fs::write(downloads.join("installer.exe"), b"binary").unwrap();

    let output = ofm(&["clean-downloads", "--dir", &s(&downloads), "--dry-run"]);
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert!(stdout_of(&output).contains("Would"));
    assert!(downloads.join("installer.exe").exists());
}

#[test]
fn clean_downloads_can_move_entries_aside() {
    let dir = tempfile::tempdir().unwrap();
    let downloads = dir.path().join("Downloads");
    fs::create_dir_all(&downloads).unwrap();
    fs::write(downloads.join("installer.exe"), b"binary").unwrap();
    fs::write(downloads.join("keep.txt"), b"text").unwrap();
    let attic = dir.path().join("attic");

    let output = ofm(&[
        "clean-downloads",
        "--dir",
        &s(&downloads),
        "--ext",
        "exe",
        "--to",
        &s(&attic),
        "--yes",
    ]);
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert!(attic.join("installer.exe").exists());
    assert!(downloads.join("keep.txt").exists());
}

#[test]
fn downloads_is_the_same_command_as_clean_downloads() {
    let dir = tempfile::tempdir().unwrap();
    let downloads = dir.path().join("Downloads");
    fs::create_dir_all(&downloads).unwrap();
    fs::write(downloads.join("installer.exe"), b"binary").unwrap();

    let output = ofm(&["downloads", "--dir", &s(&downloads), "--dry-run", "--json"]);
    assert!(output.status.success(), "{}", stdout_of(&output));
    let text = stdout_of(&output);
    assert!(
        text.contains("\"command\": \"clean-downloads\""),
        "expected the clean-downloads summary, got: {text}"
    );
    assert!(
        downloads.join("installer.exe").exists(),
        "a dry run must not remove anything"
    );
}

#[test]
fn moving_downloads_aside_keeps_nested_folders_whole() {
    // `--to` used to be a bare rename, which cannot cross volumes and so
    // failed outright for the quarantine-to-another-drive case it exists for.
    // It now goes through the transfer engine; this covers the tree handling.
    let dir = tempfile::tempdir().unwrap();
    let downloads = dir.path().join("Downloads");
    fs::create_dir_all(downloads.join("archive/inner")).unwrap();
    fs::write(downloads.join("archive/inner/deep.txt"), b"deep").unwrap();
    fs::write(downloads.join("archive/top.txt"), b"top").unwrap();
    fs::write(downloads.join("loose.bin"), b"loose").unwrap();
    let attic = dir.path().join("attic");

    let output = ofm(&[
        "downloads",
        "--dir",
        &s(&downloads),
        "--to",
        &s(&attic),
        "--yes",
    ]);
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert_eq!(
        fs::read_to_string(attic.join("archive/inner/deep.txt")).unwrap(),
        "deep"
    );
    assert!(attic.join("archive/top.txt").exists());
    assert!(attic.join("loose.bin").exists());
    assert!(
        !downloads.join("archive").exists(),
        "the source tree should be gone after a move"
    );
    let text = stdout_of(&output);
    // Two top-level entries: the `archive` tree and the loose file.
    assert!(
        text.contains("Moved 2 entries"),
        "expected both top-level entries to be counted, got: {text}"
    );
}

#[test]
fn declining_the_downloads_prompt_says_nothing_happened() {
    let dir = tempfile::tempdir().unwrap();
    let downloads = dir.path().join("Downloads");
    fs::create_dir_all(&downloads).unwrap();
    fs::write(downloads.join("installer.exe"), b"binary").unwrap();

    // No terminal on stdin, so the confirmation refuses rather than guessing.
    let output = ofm(&["downloads", "--dir", &s(&downloads)]);
    assert!(downloads.join("installer.exe").exists());
    let text = format!(
        "{}{}",
        stdout_of(&output),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains("refusing to guess"),
        "expected a refusal, got: {text}"
    );
}

#[test]
fn clean_downloads_refuses_without_a_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let downloads = dir.path().join("Downloads");
    fs::create_dir_all(&downloads).unwrap();
    fs::write(downloads.join("installer.exe"), b"binary").unwrap();

    // stdin is not a terminal here and --yes was not passed, so nothing
    // should be removed.
    let output = ofm(&["clean-downloads", "--dir", &s(&downloads), "--permanent"]);
    assert!(downloads.join("installer.exe").exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("refusing to guess"));
}

#[test]
fn empty_recycle_dry_run_reports_without_emptying() {
    let output = ofm(&["empty-recycle", "--dry-run"]);
    assert!(output.status.success(), "{}", stdout_of(&output));
    let text = stdout_of(&output);
    assert!(
        text.contains("Recycle Bin"),
        "expected a report about the bin, got: {text}"
    );
}

#[test]
fn recycle_is_the_same_command_as_empty_recycle() {
    // `ofm recycle` is what people reach for when they mean "empty the bin",
    // so it has to land on empty-recycle and not on `trash`.
    let output = ofm(&["recycle", "--dry-run", "--json"]);
    assert!(output.status.success(), "{}", stdout_of(&output));
    let text = stdout_of(&output);
    assert!(
        text.contains("\"command\": \"empty-recycle\""),
        "expected the empty-recycle summary, got: {text}"
    );
}

#[test]
fn trash_dry_run_lists_targets() {
    let dir = tempfile::tempdir().unwrap();
    let victim = dir.path().join("spare.txt");
    fs::write(&victim, b"x").unwrap();

    let output = ofm(&["trash", &s(&victim), "--dry-run"]);
    assert!(output.status.success(), "{}", stdout_of(&output));
    assert!(stdout_of(&output).contains("spare.txt"));
    assert!(victim.exists());
}

#[test]
fn doctor_describes_the_environment() {
    let output = ofm(&["doctor"]);
    assert!(output.status.success(), "{}", stdout_of(&output));
    let text = stdout_of(&output);
    assert!(text.contains("version"));
    assert!(text.contains("platform"));
}

#[test]
fn doctor_can_emit_json() {
    let output = ofm(&["doctor", "--json"]);
    assert!(output.status.success());
    let text = stdout_of(&output);
    // Exactly one JSON document, so the output can be piped into a parser.
    let value: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("expected one JSON document, got {error}: {text}"));
    assert!(value.get("version").is_some());
    assert!(value.get("platform").is_some());
}

#[test]
fn completions_are_generated_for_powershell() {
    let output = ofm(&["completions", "powershell"]);
    assert!(output.status.success());
    let script = stdout_of(&output);
    assert!(script.contains("ofm"));
    assert!(script.contains("Register-ArgumentCompleter"));
    // The script gets eval'd, so nothing may be appended to it.
    assert!(
        !script.contains("processed"),
        "a summary table would corrupt the completion script: {script}"
    );
}

#[test]
fn the_activity_log_records_a_transfer() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let destination = dir.path().join("dest");
    let log = dir.path().join("activity.log");

    // Note: this call bypasses the --no-log that `ofm()` adds.
    let output = Command::new(binary())
        .args([
            "move",
            &s(&dir.path().join("src")),
            &s(&destination),
            "--log",
            &s(&log),
            "--no-progress",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stdout_of(&output));

    let contents = fs::read_to_string(&log).unwrap();
    assert!(contents.contains("moved"), "{contents}");
}
