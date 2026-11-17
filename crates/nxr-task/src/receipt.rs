//! One versioned execution receipt.
//!
//! Static resolution names the declared shell and marks every toolchain field
//! `not entered`. Verified resolution copies a probe taken inside the entered
//! shell. Supervisor events are appended as already stored. This module does
//! not parse run logs.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::catalog::OperationRecord;

pub const RECEIPT_SCHEMA: &str = "execution-receipt-v1";
pub const NOT_ENTERED: &str = "not entered";
const ENTERED: &str = "entered";

/// A toolchain field. `status` is [`NOT_ENTERED`] until a shell probe fills it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolObservation {
    pub status: String,
    pub path: Option<String>,
    pub version: Option<String>,
}

/// One supervisor event, copied from the stored cursor and message.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReceiptEvent {
    pub cursor: u64,
    pub message: String,
}

/// Shared B2/B3 receipt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    pub schema: String,
    pub operation_id: String,
    pub shell: Option<String>,
    pub shell_entered: bool,
    pub rustc: ToolObservation,
    pub cargo: ToolObservation,
    pub wrapper: ToolObservation,
    pub linker: ToolObservation,
    pub target_directory: ToolObservation,
    pub cache_identity: ToolObservation,
    pub events: Vec<ReceiptEvent>,
}

/// Paths and versions captured by one process inside the entered shell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShellProbe {
    pub rustc_path: String,
    pub rustc_version: String,
    pub cargo_path: String,
    pub cargo_version: String,
    pub wrapper_path: String,
    pub linker_path: String,
    pub target_directory: String,
    pub cache_identity: String,
}

/// Receipt for a declaration whose shell was not entered.
#[must_use]
pub fn static_receipt(record: &OperationRecord) -> ExecutionReceipt {
    ExecutionReceipt {
        schema: RECEIPT_SCHEMA.to_owned(),
        operation_id: record.operation_id.clone(),
        shell: record.shell.clone(),
        shell_entered: false,
        rustc: not_entered(),
        cargo: not_entered(),
        wrapper: not_entered(),
        linker: not_entered(),
        target_directory: not_entered(),
        cache_identity: not_entered(),
        events: Vec::new(),
    }
}

/// Receipt whose toolchain fields are the probe from the entered shell.
///
/// # Errors
///
/// Returns an error when rustc or cargo paths are empty. Wrapper, linker,
/// target directory, and cache identity may be empty and become
/// [`NOT_ENTERED`]. The probe already requires each non-empty path to match
/// the process that printed it.
pub fn verified_receipt(
    record: &OperationRecord,
    probe: &ShellProbe,
) -> Result<ExecutionReceipt, String> {
    Ok(ExecutionReceipt {
        schema: RECEIPT_SCHEMA.to_owned(),
        operation_id: record.operation_id.clone(),
        shell: record.shell.clone(),
        shell_entered: true,
        rustc: entered(&probe.rustc_path, Some(probe.rustc_version.clone()))?,
        cargo: entered(&probe.cargo_path, Some(probe.cargo_version.clone()))?,
        wrapper: optional_entered(&probe.wrapper_path, None),
        linker: optional_entered(&probe.linker_path, None),
        target_directory: optional_entered(&probe.target_directory, None),
        cache_identity: optional_entered(&probe.cache_identity, None),
        events: Vec::new(),
    })
}

/// Append stored supervisor events. The messages are not parsed.
pub fn attach_events(receipt: &mut ExecutionReceipt, events: Vec<ReceiptEvent>) {
    receipt.events = events;
}

/// Run `enter` and read the toolchain from that process.
///
/// # Errors
///
/// Returns an error when the shell cannot be started, the probe text is
/// incomplete, or a printed path differs from the process that ran.
pub fn probe_shell(enter: &Path) -> Result<ShellProbe, String> {
    let output = Command::new(enter)
        .arg("sh")
        .arg("-c")
        .arg(PROBE_SCRIPT)
        .output()
        .map_err(|err| format!("enter shell: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "enter shell failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let text = String::from_utf8(output.stdout).map_err(|err| err.to_string())?;
    parse_probe(&text)
}

const PROBE_SCRIPT: &str = r#"
rustc_path=$(command -v rustc) || exit 1
cargo_path=$(command -v cargo) || exit 1
rustc_version=$("$rustc_path" --version) || exit 1
cargo_version=$("$cargo_path" --version) || exit 1
rustc_self="$rustc_path"
cargo_self="$cargo_path"
if [ -z "${RUSTC_WRAPPER:-}" ]; then
  wrapper_path=
  wrapper_self=
else
  wrapper_path=$(command -v "$RUSTC_WRAPPER" || true)
  if [ -n "$wrapper_path" ]; then
    "$wrapper_path" --version >/dev/null 2>&1 || true
    wrapper_self="$wrapper_path"
  else
    wrapper_path=
    wrapper_self=
  fi
fi
linker_path=$(command -v cc || true)
if [ -n "$linker_path" ]; then
  "$linker_path" --version >/dev/null 2>&1 || true
  linker_self="$linker_path"
else
  linker_path=
  linker_self=
fi
printf 'rustc_path=%s\n' "$rustc_path"
printf 'rustc_self=%s\n' "$rustc_self"
printf 'rustc_version=%s\n' "$rustc_version"
printf 'cargo_path=%s\n' "$cargo_path"
printf 'cargo_self=%s\n' "$cargo_self"
printf 'cargo_version=%s\n' "$cargo_version"
printf 'wrapper_path=%s\n' "$wrapper_path"
printf 'wrapper_self=%s\n' "$wrapper_self"
printf 'linker_path=%s\n' "$linker_path"
printf 'linker_self=%s\n' "$linker_self"
printf 'target_directory=%s\n' "${CARGO_TARGET_DIR:-}"
printf 'cache_identity=%s\n' "${NXR_CACHE_IDENTITY:-}"
"#;

fn parse_probe(text: &str) -> Result<ShellProbe, String> {
    let mut fields = std::collections::BTreeMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        fields.insert(key.to_owned(), value.to_owned());
    }
    let rustc_path = matched(&fields, "rustc_path", "rustc_self")?;
    let cargo_path = matched(&fields, "cargo_path", "cargo_self")?;
    let wrapper_path = matched_allow_empty(&fields, "wrapper_path", "wrapper_self")?;
    let linker_path = matched_allow_empty(&fields, "linker_path", "linker_self")?;
    Ok(ShellProbe {
        rustc_path,
        rustc_version: required(&fields, "rustc_version")?,
        cargo_path,
        cargo_version: required(&fields, "cargo_version")?,
        wrapper_path,
        linker_path,
        target_directory: optional_field(&fields, "target_directory"),
        cache_identity: optional_field(&fields, "cache_identity"),
    })
}

fn matched(
    fields: &std::collections::BTreeMap<String, String>,
    path_key: &str,
    self_key: &str,
) -> Result<String, String> {
    let path = required(fields, path_key)?;
    let printed = required(fields, self_key)?;
    if path != printed {
        return Err(format!("{path_key} does not match the process that ran"));
    }
    Ok(path)
}

fn matched_allow_empty(
    fields: &std::collections::BTreeMap<String, String>,
    path_key: &str,
    self_key: &str,
) -> Result<String, String> {
    let path = optional_field(fields, path_key);
    let printed = optional_field(fields, self_key);
    if path != printed {
        return Err(format!("{path_key} does not match the process that ran"));
    }
    Ok(path)
}

fn required(
    fields: &std::collections::BTreeMap<String, String>,
    key: &str,
) -> Result<String, String> {
    let value = fields.get(key).map(String::as_str).unwrap_or("").trim();
    if value.is_empty() {
        return Err(format!("{key} was not entered"));
    }
    Ok(value.to_owned())
}

fn optional_field(fields: &std::collections::BTreeMap<String, String>, key: &str) -> String {
    fields
        .get(key)
        .map(|value| value.trim().to_owned())
        .unwrap_or_default()
}

fn not_entered() -> ToolObservation {
    ToolObservation {
        status: NOT_ENTERED.to_owned(),
        path: None,
        version: None,
    }
}

fn entered(path: &str, version: Option<String>) -> Result<ToolObservation, String> {
    if path.is_empty() {
        return Err("toolchain path was not entered".to_owned());
    }
    Ok(ToolObservation {
        status: ENTERED.to_owned(),
        path: Some(path.to_owned()),
        version,
    })
}

fn optional_entered(path: &str, version: Option<String>) -> ToolObservation {
    if path.is_empty() {
        not_entered()
    } else {
        ToolObservation {
            status: ENTERED.to_owned(),
            path: Some(path.to_owned()),
            version,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Declaration, OperationKind, WorkspaceInstance, operation_record};
    use std::io::Write;

    fn record() -> OperationRecord {
        let workspace = WorkspaceInstance {
            workspace_id: "nxr".to_owned(),
            workspace_root: "/tmp/nxr".to_owned(),
            source_revision: Some("abc".to_owned()),
        };
        let mut declaration = Declaration::named(OperationKind::Task, "fmt-check", "flake.nix");
        declaration.shell = Some("rust".to_owned());
        declaration.rustc_wrapper = Some("sccache".to_owned());
        operation_record(&workspace, &declaration)
    }

    #[test]
    fn static_receipt_does_not_invent_a_compiler() {
        let receipt = static_receipt(&record());
        assert_eq!(receipt.schema, RECEIPT_SCHEMA);
        assert_eq!(receipt.shell.as_deref(), Some("rust"));
        assert!(!receipt.shell_entered);
        assert_eq!(receipt.rustc.status, NOT_ENTERED);
        assert!(receipt.rustc.path.is_none());
        assert_eq!(receipt.cargo.status, NOT_ENTERED);
        assert_eq!(receipt.wrapper.status, NOT_ENTERED);
        assert_eq!(receipt.target_directory.status, NOT_ENTERED);
        assert_eq!(receipt.cache_identity.status, NOT_ENTERED);
        assert!(receipt.events.is_empty());
    }

    #[test]
    fn fixture_shell_probe_matches_the_process() {
        let enter =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/receipt-shell/enter.sh");
        let probe = probe_shell(&enter).expect("fixture shell");
        let receipt = verified_receipt(&record(), &probe).expect("receipt");
        assert!(receipt.shell_entered);
        assert_eq!(receipt.rustc.status, "entered");
        assert_eq!(
            receipt.rustc.path.as_deref(),
            Some(probe.rustc_path.as_str())
        );
        assert_eq!(
            receipt.rustc.version.as_deref(),
            Some("rustc 1.98.0-fixture")
        );
        assert_eq!(
            receipt.cargo.path.as_deref(),
            Some(probe.cargo_path.as_str())
        );
        assert_eq!(
            receipt.wrapper.path.as_deref(),
            Some(probe.wrapper_path.as_str())
        );
        assert!(
            probe
                .wrapper_path
                .ends_with("fixtures/receipt-shell/bin/wrapper")
        );
        assert_eq!(
            receipt.linker.path.as_deref(),
            Some(probe.linker_path.as_str())
        );
        assert!(
            receipt
                .target_directory
                .path
                .as_deref()
                .unwrap_or("")
                .ends_with("fixtures/receipt-shell/target")
        );
        assert_eq!(
            receipt.cache_identity.path.as_deref(),
            Some("fixture-cache")
        );
        let mut with_events = receipt;
        attach_events(
            &mut with_events,
            vec![ReceiptEvent {
                cursor: 1,
                message: "started".to_owned(),
            }],
        );
        assert_eq!(with_events.events[0].message, "started");
    }

    #[test]
    fn probe_without_rustc_wrapper_leaves_wrapper_not_entered() {
        let fixture_bin =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/receipt-shell/bin");
        let fixture_bin = std::fs::canonicalize(&fixture_bin).expect("fixture bin");
        let enter_path = std::env::temp_dir().join(format!(
            "nxr-receipt-enter-no-wrapper-{}.sh",
            std::process::id()
        ));
        {
            let mut file = std::fs::File::create(&enter_path).expect("temp enter");
            writeln!(
                file,
                "#!/bin/sh\nexport PATH=\"{}:$PATH\"\nunset RUSTC_WRAPPER\nexec \"$@\"",
                fixture_bin.display()
            )
            .expect("write enter");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&enter_path)
                .expect("enter metadata")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&enter_path, perms).expect("chmod enter");
        }

        let probe = probe_shell(&enter_path).expect("probe without wrapper");
        assert!(probe.wrapper_path.is_empty());
        let receipt = verified_receipt(&record(), &probe).expect("receipt");
        assert!(receipt.shell_entered);
        assert_eq!(receipt.wrapper.status, NOT_ENTERED);
        assert!(receipt.wrapper.path.is_none());
        assert_eq!(receipt.rustc.status, "entered");
        assert_eq!(
            receipt.rustc.version.as_deref(),
            Some("rustc 1.98.0-fixture")
        );

        let _ = std::fs::remove_file(&enter_path);
    }
}
