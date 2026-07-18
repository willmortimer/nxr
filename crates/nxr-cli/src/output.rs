//! User-facing renderers for list output.

use std::io::{self, Write};

use nxr_core::sanitize::sanitize_terminal_text;
use nxr_core::{App, AppList};

/// Write human-readable app listing to `stdout`.
///
/// # Errors
///
/// Returns an I/O error when writing to `writer` fails.
pub fn write_human_list(writer: &mut impl Write, system: &str, apps: &[App]) -> io::Result<()> {
    writeln!(writer, "Available apps for {system}")?;
    writeln!(writer)?;

    if apps.is_empty() {
        return Ok(());
    }

    // Leave at least one padding space before descriptions when a name fills the column.
    let max_name_len = apps
        .iter()
        .map(|app| app.name.len())
        .max()
        .unwrap_or_default();
    let name_width = max_name_len.max(11).max(max_name_len.saturating_add(1));

    for app in apps {
        write!(writer, "  {:<name_width$}", app.name)?;
        if let Some(description) = &app.description {
            writeln!(writer, "{}", sanitize_terminal_text(description))?;
        } else {
            writeln!(writer)?;
        }
    }

    Ok(())
}

/// Serialize list output as JSON to `stdout`.
///
/// # Errors
///
/// Returns an error when JSON serialization fails or writing to `writer` fails.
pub fn write_json_list(
    writer: &mut impl Write,
    flake: &str,
    system: &str,
    apps: &[App],
) -> Result<(), JsonListError> {
    let envelope = AppList::from_apps(flake, system, apps.iter().cloned());
    let json = serde_json::to_string_pretty(&envelope)?;
    writeln!(writer, "{json}")?;
    Ok(())
}

/// Errors while emitting JSON list output.
#[derive(Debug, thiserror::Error)]
pub enum JsonListError {
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use nxr_core::App;

    use super::{write_human_list, write_json_list};

    fn sample_apps() -> Vec<App> {
        vec![
            App {
                name: "dev".to_owned(),
                attr_path: "apps.aarch64-darwin.dev".to_owned(),
                flake_ref: ".".to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: Some("Start local development services".to_owned()),
                is_default: false,
                metadata: BTreeMap::new(),
            },
            App {
                name: "lint".to_owned(),
                attr_path: "apps.aarch64-darwin.lint".to_owned(),
                flake_ref: ".".to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: Some("Run static analysis".to_owned()),
                is_default: false,
                metadata: BTreeMap::new(),
            },
            App {
                name: "test".to_owned(),
                attr_path: "apps.aarch64-darwin.test".to_owned(),
                flake_ref: ".".to_owned(),
                system: "aarch64-darwin".to_owned(),
                description: Some("Run the test suite".to_owned()),
                is_default: false,
                metadata: BTreeMap::new(),
            },
        ]
    }

    #[test]
    fn human_list_matches_cli_contract_example() {
        let mut output = Vec::new();
        write_human_list(&mut output, "aarch64-darwin", &sample_apps()).expect("write human list");

        let rendered = String::from_utf8(output).expect("utf-8");
        let expected = "\
Available apps for aarch64-darwin

  dev        Start local development services
  lint       Run static analysis
  test       Run the test suite
";
        assert_eq!(rendered, expected);
    }

    #[test]
    fn human_list_empty_apps_prints_header_only() {
        let mut output = Vec::new();
        write_human_list(&mut output, "aarch64-darwin", &[]).expect("write human list");

        let rendered = String::from_utf8(output).expect("utf-8");
        assert_eq!(rendered, "Available apps for aarch64-darwin\n\n");
    }

    #[test]
    fn json_list_uses_schema_field_names() {
        let mut output = Vec::new();
        write_json_list(&mut output, ".", "aarch64-darwin", &sample_apps())
            .expect("write json list");

        let value: serde_json::Value =
            serde_json::from_slice(&output).expect("parse json list output");
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["flake"], ".");
        assert_eq!(value["system"], "aarch64-darwin");

        let apps = value["apps"].as_array().expect("apps array");
        assert_eq!(apps.len(), 3);
        assert_eq!(apps[0]["name"], "dev");
        assert_eq!(apps[0]["attr_path"], "apps.aarch64-darwin.dev");
        assert_eq!(apps[0]["description"], "Start local development services");
        assert_eq!(apps[0]["default"], false);
    }

    #[test]
    fn human_list_keeps_gap_when_name_fills_min_width() {
        let apps = vec![App {
            name: "root-marker".to_owned(),
            attr_path: "apps.aarch64-darwin.root-marker".to_owned(),
            flake_ref: ".".to_owned(),
            system: "aarch64-darwin".to_owned(),
            description: Some("Confirm the flake root marker file is readable".to_owned()),
            is_default: false,
            metadata: BTreeMap::new(),
        }];

        let mut output = Vec::new();
        write_human_list(&mut output, "aarch64-darwin", &apps).expect("write human list");
        let rendered = String::from_utf8(output).expect("utf-8");
        assert!(
            rendered.contains("root-marker Confirm"),
            "expected a space between name and description, got:\n{rendered}"
        );
    }

    #[test]
    fn human_list_sanitizes_control_sequences_in_descriptions() {
        let apps = vec![App {
            name: "evil".to_owned(),
            attr_path: "apps.aarch64-darwin.evil".to_owned(),
            flake_ref: ".".to_owned(),
            system: "aarch64-darwin".to_owned(),
            description: Some("\u{1b}[31mhidden\u{1b}[0m".to_owned()),
            is_default: false,
            metadata: BTreeMap::new(),
        }];

        let mut output = Vec::new();
        write_human_list(&mut output, "aarch64-darwin", &apps).expect("write human list");
        let rendered = String::from_utf8(output).expect("utf-8");
        assert!(rendered.contains("  evil       hidden"));
        assert!(!rendered.contains('\u{1b}'));
    }
}
