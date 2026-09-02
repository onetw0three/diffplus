//! User configuration loading and command-line precedence.

use crate::cli::{Args, Color, JvmMode, NativeMode};
use anyhow::{bail, Context, Result};
use clap::{parser::ValueSource, ArgMatches};
use serde::Deserialize;
use std::path::{Path, PathBuf};

const APPLICATION_DIRECTORY: &str = "artifact-diff";
const CONFIG_FILENAME: &str = "config.toml";

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    tui: Option<bool>,
    output: Option<PathBuf>,
    color: Option<Color>,
    context: Option<usize>,
    max_file_size: Option<u64>,
    max_expanded_size: Option<u64>,
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

    set!(tui);
    set_path!(output);
    set!(color);
    set!(context);
    set!(max_file_size);
    set!(max_expanded_size);
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
            "artifact-diff",
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
}
