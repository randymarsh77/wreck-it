use crate::types::Config;
use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
}

fn config_file_path() -> Result<PathBuf> {
    let home = home_dir().context("Could not determine user home directory")?;
    Ok(config_file_path_for_home(&home))
}

fn config_file_path_for_home(home: &Path) -> PathBuf {
    home.join(".wreck-it").join("config.json")
}

pub fn load_user_config() -> Result<Config> {
    let path = config_file_path()?;
    load_user_config_from_path(&path)
}

fn load_user_config_from_path(path: &Path) -> Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }

    let content = fs::read_to_string(path).context("Failed to read user config file")?;
    let config = serde_json::from_str(&content).context("Failed to parse user config file")?;
    Ok(config)
}

pub fn save_user_config(config: &Config) -> Result<()> {
    let path = config_file_path()?;
    save_user_config_to_path(path.as_path(), config)
}

fn save_user_config_to_path(path: &Path, config: &Config) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("Failed to create user config directory")?;
    }
    let content =
        serde_json::to_string_pretty(config).context("Failed to serialize user config file")?;
    fs::write(path, content).context("Failed to write user config file")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ModelProvider;
    use tempfile::tempdir;

    #[test]
    fn test_load_user_config_defaults_when_missing() {
        let dir = tempdir().unwrap();
        let config_file = config_file_path_for_home(dir.path());

        let loaded = load_user_config_from_path(&config_file).unwrap();
        assert_eq!(loaded.max_iterations, 100);
        assert_eq!(loaded.model_provider, ModelProvider::Copilot);
    }

    #[test]
    fn test_save_and_load_user_config() {
        let dir = tempdir().unwrap();
        let config_file = config_file_path_for_home(dir.path());
        let config = Config {
            max_iterations: 10,
            model_provider: ModelProvider::Llama,
            ..Config::default()
        };

        save_user_config_to_path(&config_file, &config).unwrap();
        let loaded = load_user_config_from_path(&config_file).unwrap();

        assert_eq!(loaded.max_iterations, 10);
        assert_eq!(loaded.model_provider, ModelProvider::Llama);
    }
}
