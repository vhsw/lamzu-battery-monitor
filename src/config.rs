use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use flexi_logger::{Cleanup, Criterion, FileSpec, Logger, LoggerHandle, Naming, WriteMode};
use log::{info, warn};
use serde::Deserialize;

pub const APP_NAME: &str = "Lamzu Battery Monitor";

const DEFAULT_POLL_INTERVAL_SECS: u64 = 300;
const DEFAULT_LOW_PCT: u8 = 20;
const DEFAULT_CRITICAL_PCT: u8 = 10;

#[derive(Debug, Clone, Deserialize, Default)]
struct ConfigFile {
    poll_interval_secs: Option<u64>,
    low_battery_pct: Option<u8>,
    critical_battery_pct: Option<u8>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub poll_interval: Duration,
    pub low_pct: u8,
    pub critical_pct: u8,
}

impl Config {
    fn defaults() -> Self {
        Self {
            poll_interval: Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS),
            low_pct: DEFAULT_LOW_PCT,
            critical_pct: DEFAULT_CRITICAL_PCT,
        }
    }

    fn sanitize(&mut self) {
        if self.poll_interval.as_secs() == 0 {
            self.poll_interval = Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS);
        }

        if self.low_pct == 0 || self.low_pct > 100 {
            self.low_pct = DEFAULT_LOW_PCT;
        }

        if self.critical_pct > 100 {
            self.critical_pct = DEFAULT_CRITICAL_PCT;
        }

        if self.critical_pct >= self.low_pct {
            self.critical_pct = self.low_pct.saturating_sub(1);
        }
    }
}

pub fn load_config(app_dir: &Path) -> Config {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("config.toml"));
        }
    }

    candidates.push(app_dir.join("config.toml"));

    for path in candidates {
        if !path.is_file() {
            continue;
        }

        match fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<ConfigFile>(&text) {
                Ok(file_cfg) => {
                    let mut cfg = Config::defaults();

                    if let Some(secs) = file_cfg.poll_interval_secs {
                        cfg.poll_interval = Duration::from_secs(secs);
                    }

                    if let Some(low) = file_cfg.low_battery_pct {
                        cfg.low_pct = low;
                    }

                    if let Some(critical) = file_cfg.critical_battery_pct {
                        cfg.critical_pct = critical;
                    }

                    cfg.sanitize();

                    info!("Loaded configuration from {}", path.display());
                    return cfg;
                }
                Err(e) => {
                    warn!(
                        "Config file {} is invalid ({}); trying next",
                        path.display(),
                        e
                    );
                }
            },
            Err(e) => {
                warn!(
                    "Could not read config file {} ({}); trying next",
                    path.display(),
                    e
                );
            }
        }
    }

    let mut cfg = Config::defaults();
    cfg.sanitize();

    info!(
        "No valid config.toml found; defaulting to {}s polling, low {}%, critical {}%",
        cfg.poll_interval.as_secs(),
        cfg.low_pct,
        cfg.critical_pct
    );

    cfg
}

pub fn app_data_dir() -> PathBuf {
    if let Ok(local) = env::var("LOCALAPPDATA") {
        let p = PathBuf::from(local).join("LamzuBatteryMonitor");
        if fs::create_dir_all(&p).is_ok() {
            return p;
        }
    }

    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("LamzuBatteryMonitor");
            if fs::create_dir_all(&p).is_ok() {
                return p;
            }
        }
    }

    let p = env::temp_dir().join("LamzuBatteryMonitor");
    let _ = fs::create_dir_all(&p);
    p
}

pub fn init_logging(log_dir: &Path) -> Result<LoggerHandle> {
    fs::create_dir_all(log_dir)
        .with_context(|| format!("creating log directory {}", log_dir.display()))?;

    let logger = Logger::try_with_str("info")
        .map_err(|e| anyhow!("failed to create logger specification: {e}"))?
        .log_to_file(
            FileSpec::default()
                .directory(log_dir)
                .basename("lamzu-battery-monitor"),
        )
        .write_mode(WriteMode::Direct)
        .rotate(
            Criterion::Size(10 * 1024 * 1024),
            Naming::Timestamps,
            Cleanup::KeepLogFiles(3),
        )
        .start()
        .map_err(|e| anyhow!("failed to start logger: {e}"))?;

    Ok(logger)
}
