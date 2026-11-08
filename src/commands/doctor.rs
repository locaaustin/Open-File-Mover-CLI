//! `ofm doctor` - answers "is this thing set up properly?" without the user
//! having to guess where the log went or whether elevation matters.

use anyhow::Result;

use crate::log::Logger;
use crate::platform;
use crate::report::Summary;
use crate::ui::Ui;
use crate::util;

pub fn run(ui: &Ui, logger: &Logger) -> Result<Summary> {
    let mut rows: Vec<(String, String)> = vec![
        ("version".into(), env!("CARGO_PKG_VERSION").to_string()),
        ("platform".into(), platform::describe_platform()),
        (
            "cpu threads".into(),
            std::thread::available_parallelism()
                .map(|n| n.get().to_string())
                .unwrap_or_else(|_| "unknown".into()),
        ),
        (
            "executable".into(),
            std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "unknown".into()),
        ),
        (
            "activity log".into(),
            match logger.path() {
                Some(path) => path.display().to_string(),
                None => "disabled".into(),
            },
        ),
    ];

    match platform::downloads_dir() {
        Ok(path) => rows.push((
            "downloads".into(),
            format!(
                "{}{}",
                path.display(),
                if path.is_dir() { "" } else { "  (missing)" }
            ),
        )),
        Err(error) => rows.push(("downloads".into(), format!("unavailable: {error}"))),
    }

    match platform::recycle_bin_contents(None) {
        Ok(contents) => rows.push((
            "recycle bin".into(),
            format!(
                "{} items, {}",
                contents.items,
                util::format_bytes(contents.bytes)
            ),
        )),
        Err(error) => rows.push(("recycle bin".into(), format!("unavailable: {error}"))),
    }

    if ui.json {
        let map: serde_json::Map<String, serde_json::Value> = rows
            .into_iter()
            .map(|(k, v)| (k.replace(' ', "_"), serde_json::Value::String(v)))
            .collect();
        println!("{}", serde_json::to_string_pretty(&map)?);
        return Ok(Summary::simple("doctor", false));
    }

    // Pad before colouring, or the ANSI escapes eat the format width.
    let width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    ui.say(ui.bold("Open File Mover - environment"));
    for (key, value) in rows {
        ui.say(format!(
            "  {}  {}",
            ui.dim(&format!("{key:<width$}")),
            value
        ));
    }
    ui.say("");
    ui.say(
        ui.dim("Moves inside one volume are instant renames. Moves between volumes stream data,"),
    );
    ui.say(
        ui.dim("so those are the ones worth running with --verify. Elevation is only needed for"),
    );
    ui.say(ui.dim("files your account cannot otherwise touch."));

    Ok(Summary::simple("doctor", false))
}
