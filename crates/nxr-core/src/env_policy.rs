//! Environment policy for planned and executed child processes.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::process::Command;

use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// How the child process inherits environment variables.
///
/// JSON: `"inherit"` or `{ "mode": "clean", "keep": [...], "set": {...}, "unset": [...] }`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentPolicy {
    /// Inherit the caller's full environment (default).
    Inherit,
    /// Start from the documented clean allowlist, then apply keep/set/unset.
    Clean {
        keep: Vec<String>,
        set: BTreeMap<String, String>,
        unset: Vec<String>,
    },
}

/// Default variables retained in `--clean-env` mode (inspectable allowlist).
pub const CLEAN_ENV_ALLOWLIST: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "TMPDIR",
    "TMP",
    "TEMP",
    "TERM",
    "COLORTERM",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "SSH_AUTH_SOCK",
    "XDG_RUNTIME_DIR",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "NIX_SSL_CERT_FILE",
    "NIX_PATH",
    "NIX_CONFIG",
    "SSL_CERT_FILE",
    "CURL_CA_BUNDLE",
    // Absolute `nix` still consults CA/store settings; PATH is intentionally
    // omitted so clean mode surfaces apps that rely on shell-polluted PATH.
];

impl EnvironmentPolicy {
    /// Build a clean policy from CLI keep/set/unset inputs.
    #[must_use]
    pub fn clean(
        keep: impl IntoIterator<Item = String>,
        set: impl IntoIterator<Item = (String, String)>,
        unset: impl IntoIterator<Item = String>,
    ) -> Self {
        Self::Clean {
            keep: keep.into_iter().collect(),
            set: set.into_iter().collect(),
            unset: unset.into_iter().collect(),
        }
    }

    /// Apply this policy to a [`Command`] before spawn.
    pub fn apply(&self, command: &mut Command) {
        match self {
            Self::Inherit => {}
            Self::Clean { keep, set, unset } => {
                command.env_clear();
                for (key, value) in std::env::vars_os() {
                    let Some(name) = key.to_str() else {
                        continue;
                    };
                    if unset.iter().any(|u| u == name) {
                        continue;
                    }
                    if is_allowlisted(name) || keep.iter().any(|k| k == name) {
                        command.env(key, value);
                    }
                }
                for (key, value) in set {
                    if unset.iter().any(|u| u == key) {
                        continue;
                    }
                    command.env(key, value);
                }
            }
        }
    }
}

fn is_allowlisted(name: &str) -> bool {
    CLEAN_ENV_ALLOWLIST.contains(&name)
}

impl Serialize for EnvironmentPolicy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Inherit => serializer.serialize_str("inherit"),
            Self::Clean { keep, set, unset } => {
                let mut map = serializer.serialize_map(Some(4))?;
                map.serialize_entry("mode", "clean")?;
                map.serialize_entry("keep", keep)?;
                map.serialize_entry("set", set)?;
                map.serialize_entry("unset", unset)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for EnvironmentPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PolicyVisitor;

        impl<'de> Visitor<'de> for PolicyVisitor {
            type Value = EnvironmentPolicy;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("\"inherit\" or a clean-env policy object")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                match value {
                    "inherit" => Ok(EnvironmentPolicy::Inherit),
                    other => Err(E::unknown_variant(other, &["inherit"])),
                }
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut mode: Option<String> = None;
                let mut keep = Vec::new();
                let mut set = BTreeMap::new();
                let mut unset = Vec::new();

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "mode" => mode = Some(map.next_value()?),
                        "keep" => keep = map.next_value()?,
                        "set" => set = map.next_value()?,
                        "unset" => unset = map.next_value()?,
                        other => {
                            return Err(de::Error::unknown_field(
                                other,
                                &["mode", "keep", "set", "unset"],
                            ));
                        }
                    }
                }

                match mode.as_deref() {
                    Some("clean") => Ok(EnvironmentPolicy::Clean { keep, set, unset }),
                    Some(other) => Err(de::Error::unknown_variant(other, &["clean"])),
                    None => Err(de::Error::missing_field("mode")),
                }
            }
        }

        deserializer.deserialize_any(PolicyVisitor)
    }
}

/// Parse a `KEY=VALUE` assignment for `--set-env`.
///
/// # Errors
///
/// Returns an error when `=` is missing or the key is empty.
pub fn parse_set_env(raw: &str) -> Result<(String, String), String> {
    let (key, value) = raw
        .split_once('=')
        .ok_or_else(|| format!("expected KEY=VALUE, got `{raw}`"))?;
    if key.is_empty() {
        return Err("environment variable name must not be empty".to_owned());
    }
    if key.contains('\0') || value.contains('\0') {
        return Err("environment assignment must not contain NUL".to_owned());
    }
    Ok((key.to_owned(), value.to_owned()))
}

/// Validate an environment variable name for `--keep-env` / `--unset-env`.
///
/// # Errors
///
/// Returns an error when the name is empty or contains `=`.
pub fn parse_env_name(raw: &str) -> Result<String, String> {
    if raw.is_empty() {
        return Err("environment variable name must not be empty".to_owned());
    }
    if raw.contains('=') {
        return Err(format!("expected a variable name, got `{raw}`"));
    }
    if raw.contains('\0') {
        return Err("environment variable name must not contain NUL".to_owned());
    }
    let _ = OsStr::new(raw);
    Ok(raw.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{CLEAN_ENV_ALLOWLIST, EnvironmentPolicy, parse_env_name, parse_set_env};

    #[test]
    fn inherit_round_trips_as_string() {
        let json = serde_json::to_string(&EnvironmentPolicy::Inherit).unwrap();
        assert_eq!(json, "\"inherit\"");
        let parsed: EnvironmentPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, EnvironmentPolicy::Inherit);
    }

    #[test]
    fn clean_round_trips_as_object() {
        let policy = EnvironmentPolicy::clean(
            ["HOME".to_owned()],
            [("CI".to_owned(), "1".to_owned())],
            ["TMPDIR".to_owned()],
        );
        let json = serde_json::to_value(&policy).unwrap();
        assert_eq!(json["mode"], "clean");
        assert_eq!(json["keep"][0], "HOME");
        assert_eq!(json["set"]["CI"], "1");
        assert_eq!(json["unset"][0], "TMPDIR");
        let parsed: EnvironmentPolicy = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, policy);
    }

    #[test]
    fn parse_set_env_requires_equals() {
        assert!(parse_set_env("NOVALUE").is_err());
        assert_eq!(
            parse_set_env("A=b=c").unwrap(),
            ("A".to_owned(), "b=c".to_owned())
        );
    }

    #[test]
    fn parse_env_name_rejects_assignment() {
        assert!(parse_env_name("A=B").is_err());
        assert_eq!(parse_env_name("HOME").unwrap(), "HOME");
    }

    #[test]
    fn allowlist_includes_documented_examples() {
        for name in ["HOME", "USER", "TMPDIR", "XDG_RUNTIME_DIR"] {
            assert!(CLEAN_ENV_ALLOWLIST.contains(&name), "{name}");
        }
        assert!(!CLEAN_ENV_ALLOWLIST.contains(&"PATH"));
    }
}
