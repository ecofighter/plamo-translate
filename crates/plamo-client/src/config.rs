use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid config {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid config {path}: {key} must be a non-empty string")]
    Invalid { path: PathBuf, key: &'static str },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub model: Option<String>,
    pub revision: Option<String>,
    pub idle_timeout_secs: Option<u64>,
    pub default_target: String,
    pub glossary_file: Option<PathBuf>,
    pub socket_path: PathBuf,
    pub config_file: Option<PathBuf>,
}

#[derive(Deserialize, Default)]
struct RawConfig {
    model: Option<String>,
    revision: Option<String>,
    idle_timeout_secs: Option<u64>,
    default_target: Option<String>,
    glossary_file: Option<String>,
    socket_path: Option<String>,
}

impl RawConfig {
    fn reject_empty_strings(&self, path: &Path) -> Result<(), ConfigError> {
        let fields = [
            ("model", &self.model),
            ("revision", &self.revision),
            ("glossary_file", &self.glossary_file),
            ("socket_path", &self.socket_path),
        ];
        match fields.into_iter().find(|(_, v)| v.as_deref() == Some("")) {
            Some((key, _)) => Err(ConfigError::Invalid {
                path: path.to_path_buf(),
                key,
            }),
            None => Ok(()),
        }
    }
}

pub fn default_socket_path(home: &Path) -> PathBuf {
    home.join("Library/Application Support/plamo-translate/daemon.sock")
}

fn expand_tilde(s: &str, home: &Path) -> PathBuf {
    if s == "~" {
        home.to_path_buf()
    } else if let Some(rest) = s.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(s)
    }
}

fn non_empty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.is_empty())
}

impl Config {
    pub fn load() -> Result<Config, ConfigError> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        Config::load_from(&|k| std::env::var(k).ok(), &home)
    }

    pub fn load_from(
        env: &impl Fn(&str) -> Option<String>,
        home: &Path,
    ) -> Result<Config, ConfigError> {
        let mut candidates = Vec::new();
        if let Some(p) = non_empty(env("PLAMO_TRANSLATE_CONFIG")) {
            candidates.push(expand_tilde(&p, home));
        }
        let xdg_base = non_empty(env("XDG_CONFIG_HOME"))
            .map(|p| expand_tilde(&p, home))
            .unwrap_or_else(|| home.join(".config"));
        candidates.push(xdg_base.join("plamo-translate/config.toml"));

        let (raw, config_file) = match candidates.into_iter().find(|c| c.is_file()) {
            Some(path) => {
                let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Io {
                    path: path.clone(),
                    source,
                })?;
                let raw: RawConfig =
                    toml::from_str(&text).map_err(|source| ConfigError::Parse {
                        path: path.clone(),
                        source,
                    })?;
                raw.reject_empty_strings(&path)?;
                (raw, Some(path))
            }
            None => (RawConfig::default(), None),
        };

        let socket_path = non_empty(env("PLAMO_TRANSLATE_SOCKET"))
            .or(raw.socket_path)
            .map(|p| expand_tilde(&p, home))
            .unwrap_or_else(|| default_socket_path(home));

        Ok(Config {
            model: raw.model,
            revision: raw.revision,
            idle_timeout_secs: raw.idle_timeout_secs,
            default_target: raw.default_target.unwrap_or_else(|| "auto".into()),
            glossary_file: raw.glossary_file.map(|p| expand_tilde(&p, home)),
            socket_path,
            config_file,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    fn write(dir: &Path, rel: &str, body: &str) -> PathBuf {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn config_missing_file_uses_defaults() {
        let home = tempfile::tempdir().unwrap();
        let c = Config::load_from(&env_of(&[]), home.path()).unwrap();
        assert_eq!(c.default_target, "auto");
        assert_eq!(
            c.socket_path,
            home.path()
                .join("Library/Application Support/plamo-translate/daemon.sock")
        );
        assert_eq!(c.config_file, None);
        assert_eq!(c.model, None);
    }

    #[test]
    fn reads_xdg_default_location() {
        let home = tempfile::tempdir().unwrap();
        let p = write(
            home.path(),
            ".config/plamo-translate/config.toml",
            "default_target = \"en\"\nidle_timeout_secs = 0\n",
        );
        let c = Config::load_from(&env_of(&[]), home.path()).unwrap();
        assert_eq!(c.default_target, "en");
        assert_eq!(c.idle_timeout_secs, Some(0));
        assert_eq!(c.config_file, Some(p));
    }

    #[test]
    fn env_socket_overrides_config() {
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            ".config/plamo-translate/config.toml",
            "socket_path = \"/from/config.sock\"\n",
        );
        let c = Config::load_from(
            &env_of(&[("PLAMO_TRANSLATE_SOCKET", "/from/env.sock")]),
            home.path(),
        )
        .unwrap();
        assert_eq!(c.socket_path, PathBuf::from("/from/env.sock"));
        let c = Config::load_from(&env_of(&[]), home.path()).unwrap();
        assert_eq!(c.socket_path, PathBuf::from("/from/config.sock"));
    }

    #[test]
    fn env_config_path_wins_over_xdg() {
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            "xdg/plamo-translate/config.toml",
            "model = \"xdg\"\n",
        );
        let explicit = write(home.path(), "explicit.toml", "model = \"explicit\"\n");
        let xdg = home.path().join("xdg");
        let c = Config::load_from(
            &env_of(&[
                ("PLAMO_TRANSLATE_CONFIG", explicit.to_str().unwrap()),
                ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
            ]),
            home.path(),
        )
        .unwrap();
        assert_eq!(c.model.as_deref(), Some("explicit"));
        let c = Config::load_from(
            &env_of(&[("XDG_CONFIG_HOME", xdg.to_str().unwrap())]),
            home.path(),
        )
        .unwrap();
        assert_eq!(c.model.as_deref(), Some("xdg"));
    }

    #[test]
    fn tilde_expanded() {
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            ".config/plamo-translate/config.toml",
            "glossary_file = \"~/g.tsv\"\nsocket_path = \"~/s.sock\"\n",
        );
        let c = Config::load_from(&env_of(&[]), home.path()).unwrap();
        assert_eq!(c.glossary_file, Some(home.path().join("g.tsv")));
        assert_eq!(c.socket_path, home.path().join("s.sock"));
    }

    #[test]
    fn missing_explicit_path_falls_back_to_xdg() {
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            ".config/plamo-translate/config.toml",
            "socket_path = \"/from/xdg.sock\"\n",
        );
        let missing = home.path().join("nope.toml");
        let c = Config::load_from(
            &env_of(&[("PLAMO_TRANSLATE_CONFIG", missing.to_str().unwrap())]),
            home.path(),
        )
        .unwrap();
        assert_eq!(c.socket_path, PathBuf::from("/from/xdg.sock"));
        assert_eq!(
            c.config_file,
            Some(home.path().join(".config/plamo-translate/config.toml"))
        );
    }

    #[test]
    fn directory_explicit_path_is_skipped() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("adir");
        std::fs::create_dir_all(&dir).unwrap();
        let env = env_of(&[("PLAMO_TRANSLATE_CONFIG", dir.to_str().unwrap())]);
        let c = Config::load_from(&env, home.path()).unwrap();
        assert_eq!(c.config_file, None);
        write(
            home.path(),
            ".config/plamo-translate/config.toml",
            "model = \"xdg\"\n",
        );
        let c = Config::load_from(&env, home.path()).unwrap();
        assert_eq!(c.model.as_deref(), Some("xdg"));
    }

    #[test]
    fn empty_string_values_are_rejected() {
        for key in ["socket_path", "model", "revision", "glossary_file"] {
            let home = tempfile::tempdir().unwrap();
            write(
                home.path(),
                ".config/plamo-translate/config.toml",
                &format!("{key} = \"\"\n"),
            );
            assert!(
                matches!(
                    Config::load_from(&env_of(&[]), home.path()),
                    Err(ConfigError::Invalid { .. })
                ),
                "{key}"
            );
        }
    }

    #[test]
    fn invalid_toml_is_error() {
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            ".config/plamo-translate/config.toml",
            "model = [",
        );
        assert!(matches!(
            Config::load_from(&env_of(&[]), home.path()),
            Err(ConfigError::Parse { .. })
        ));
    }
}
