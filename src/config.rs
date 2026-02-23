use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub targets: HashMap<String, Target>,
    pub problems: HashMap<String, Problem>,
}

#[derive(Debug, Deserialize)]
pub struct Target {
    pub root: String,
    pub ssh: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Problem {
    pub run_name: String,
    pub parameters: HashMap<String, Vec<toml::Value>>,
    pub phases: Phases,
}

#[derive(Debug, Deserialize)]
pub struct Phases {
    pub build: Option<Phase>,
    pub run: Phase,
    pub analyze: Option<Phase>,
}

#[derive(Debug, Deserialize)]
pub struct Phase {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
}

use anyhow::Result;
use std::fs;

impl Config {
    pub fn load(path: &str) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}
