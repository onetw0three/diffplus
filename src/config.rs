//! User configuration loading and command-line precedence.

use crate::cli::{Args, Color, JvmMode, NativeMode};
use anyhow::{bail, Context, Result};
use clap::{parser::ValueSource, ArgMatches};
use serde::{de, Deserialize, Deserializer};
use std::path::{Path, PathBuf};

const APPLICATION_DIRECTORY: &str = "diffplus";
const CONFIG_FILENAME: &str = "config.toml";

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    tui: Option<bool>,
    output: Option<PathBuf>,
    color: Option<Color>,
    context: Option<usize>,
    max_file_size: Option<SizeLimit>,
    max_expanded_size: Option<SizeLimit>,
    max_depth: Option<usize>,
    jvm: Option<JvmMode>,
    jadx_path: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    workspace_dir: Option<PathBuf>,
    no_cache: Option<bool>,
    native: Option<NativeMode>,
    ida_path: Option<PathBuf>,
    diaphora_script: Option<PathBuf>,
    diaphora_path: Option<PathBuf>,
    python_path: Option<PathBuf>,
    strip_top_level: Option<bool>,
    quiet: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SizeLimit {
    Bytes(u64),
    Unlimited,
}

impl SizeLimit {
    fn value(self) -> u64 {
        match self {
            Self::Bytes(value) => value,
            Self::Unlimited => u64::MAX,
        }
    }
}

impl<'de> Deserialize<'de> for SizeLimit {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Value {
            Bytes(u64),
            Enabled(bool),
            Name(String),
        }

        match Value::deserialize(deserializer)? {
            Value::Bytes(value) => Ok(Self::Bytes(value)),
            Value::Enabled(false) => Ok(Self::Unlimited),
            Value::Name(value) if value.eq_ignore_ascii_case("none") => Ok(Self::Unlimited),
            Value::Enabled(true) => Err(de::Error::custom(
                "size limit must be a byte count, false, or \"none\"",
            )),
            Value::Name(value) => Err(de::Error::custom(format!(
                "unknown size limit {value:?}; expected a byte count, false, or \"none\""
            ))),
        }
    }
}

pub(crate) fn merge(args: &mut Args, matches: &ArgMatches) -> Result<()> {
    let explicit_path = args.config.clone();
    let path = if args.no_config {
        return Ok(());
    } else if let Some(path) = explicit_path.as_deref() {
        expand_home(path)
    } else if let Some(path) = default_path() {
        path
    } else {
        return Ok(());
    };

    if !path.exists() {
        if explicit_path.is_some() {
            bail!("configuration file not found: {}", path.display());
        }
        return Ok(());
    }
    if !path.is_file() {
        bail!("configuration path is not a file: {}", path.display());
    }

    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("reading configuration {}", path.display()))?;
    let config: Config = toml::from_str(&contents)
        .with_context(|| format!("parsing configuration {}", path.display()))?;
    apply(args, matches, config);
    Ok(())
}

fn apply(args: &mut Args, matches: &ArgMatches, config: Config) {
    macro_rules! set {
        ($field:ident) => {
            if !from_command_line(matches, stringify!($field)) {
                if let Some(value) = config.$field {
                    args.$field = value;
                }
            }
        };
    }
    macro_rules! set_path {
        ($field:ident) => {
            if !from_command_line(matches, stringify!($field)) {
                if let Some(value) = config.$field {
                    args.$field = expand_home(&value);
                }
            }
        };
    }
    macro_rules! set_optional_path {
        ($field:ident) => {
            if !from_command_line(matches, stringify!($field)) {
                if let Some(value) = config.$field {
                    args.$field = Some(expand_home(&value));
                }
            }
        };
    }
    macro_rules! set_size_limit {
        ($field:ident) => {
            if !from_command_line(matches, stringify!($field)) {
                if let Some(value) = config.$field {
                    args.$field = value.value();
                }
            }
        };
    }

    set!(tui);
    set_path!(output);
    set!(color);
    set!(context);
    set_size_limit!(max_file_size);
    set_size_limit!(max_expanded_size);
    set!(max_depth);
    set!(jvm);
    set_path!(jadx_path);
    set_optional_path!(cache_dir);
    set_optional_path!(workspace_dir);
    set!(no_cache);
    set!(native);
    set_optional_path!(ida_path);
    set_optional_path!(diaphora_script);
    set_optional_path!(diaphora_path);
    set_path!(python_path);
    set!(strip_top_level);
    set!(quiet);
}

fn from_command_line(matches: &ArgMatches, id: &str) -> bool {
    matches.value_source(id) == Some(ValueSource::CommandLine)
}

fn default_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|path| Path::new(path).is_absolute())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|root| root.join(APPLICATION_DIRECTORY).join(CONFIG_FILENAME))
}

fn expand_home(path: &Path) -> PathBuf {
    let mut components = path.components();
    let Some(first) = components.next() else {
        return path.to_path_buf();
    };
    if first.as_os_str() != "~" {
        return path.to_path_buf();
    }
    let Some(home) = std::env::var_os("HOME") else {
        return path.to_path_buf();
    };
    components.fold(PathBuf::from(home), |result, component| {
        result.join(component.as_os_str())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, FromArgMatches};

    fn parsed(arguments: &[&str]) -> (Args, ArgMatches) {
        let matches = Args::command().try_get_matches_from(arguments).unwrap();
        let args = Args::from_arg_matches(&matches).unwrap();
        (args, matches)
    }

    #[test]
    fn config_sets_defaults_but_cli_wins() {
        let (mut args, matches) = parsed(&[
            "diffplus",
            "old",
            "new",
            "--max-depth",
            "1",
            "--ida-path",
            "/cli/ida64",
        ]);
        let config: Config = toml::from_str(
            r#"
max_depth = 4
ida_path = "/config/ida64"
diaphora_path = "~/diaphora"
quiet = true
"#,
        )
        .unwrap();

        apply(&mut args, &matches, config);

        assert_eq!(args.max_depth, 1);
        assert_eq!(args.ida_path, Some(PathBuf::from("/cli/ida64")));
        assert!(args
            .diaphora_path
            .as_deref()
            .is_some_and(|path| !path.starts_with("~")));
        assert!(args.quiet);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<Config>("ida_paht = '/opt/ida/ida64'").is_err());
    }

    #[test]
    fn size_limits_accept_bytes_false_or_none() {
        let config: Config = toml::from_str(
            r#"
max_file_size = false
max_expanded_size = "none"
"#,
        )
        .unwrap();

        assert_eq!(config.max_file_size, Some(SizeLimit::Unlimited));
        assert_eq!(config.max_expanded_size, Some(SizeLimit::Unlimited));

        let config: Config = toml::from_str("max_file_size = 4096").unwrap();
        assert_eq!(config.max_file_size, Some(SizeLimit::Bytes(4096)));
    }

    #[test]
    fn disabled_size_limits_are_applied_as_unlimited() {
        let (mut args, matches) = parsed(&["diffplus", "old", "new"]);
        let config: Config = toml::from_str(
            r#"
max_file_size = "NONE"
max_expanded_size = false
"#,
        )
        .unwrap();

        apply(&mut args, &matches, config);

        assert_eq!(args.max_file_size, u64::MAX);
        assert_eq!(args.max_expanded_size, u64::MAX);
    }

    #[test]
    fn invalid_size_limit_values_are_rejected() {
        assert!(toml::from_str::<Config>("max_file_size = true").is_err());
        assert!(toml::from_str::<Config>("max_file_size = 'unbounded'").is_err());
    }
}
