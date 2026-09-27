use std::{
    fs, io,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use tempfile::TempDir;
use tokio::time::Instant;

pub struct SteveProcess {
    child: Child,
    _temp: TempDir,
    log_path: PathBuf,
}

pub struct Listeners {
    pub inference: SocketAddr,
    pub management: SocketAddr,
}

impl SteveProcess {
    pub fn start() -> io::Result<Self> {
        Self::start_with_upstream_urls(None, None)
    }

    pub fn start_with_upstream_urls(
        openai_upstream_url: Option<&str>,
        anthropic_upstream_url: Option<&str>,
    ) -> io::Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let db_path = root.join("steve.db");
        let mut server_config = String::from(
            "[server]\ninference_bind = \"127.0.0.1:0\"\nmanagement_bind = \"127.0.0.1:0\"\n",
        );
        if let Some(url) = openai_upstream_url {
            server_config.push_str(&format!(
                "openai_upstream_url = {}\n",
                toml::Value::String(url.into())
            ));
        }
        if let Some(url) = anthropic_upstream_url {
            server_config.push_str(&format!(
                "anthropic_upstream_url = {}\n",
                toml::Value::String(url.into())
            ));
        }
        let config = format!(
            "{server_config}\n[database]\nurl = {}\n\
             [object_storage]\nkind = \"fs\"\nroot = {}\n\
             [queues]\naccounting_journal = {}\n\
             [logging]\nlevel = \"info\"\njson = true\n",
            toml::Value::String(format!("sqlite://{}?mode=rwc", db_path.display())),
            toml::Value::String(root.join("objects").display().to_string()),
            toml::Value::String(root.join("accounting-overflow.jsonl").display().to_string()),
        );
        let config_path = root.join("config.toml");
        fs::write(&config_path, config)?;
        let log_path = root.join("steve.log");
        let stdout = fs::File::create(&log_path)?;
        let stderr = stdout.try_clone()?;
        let child = clean_command(&config_path)
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()?;

        Ok(Self {
            child,
            _temp: temp,
            log_path,
        })
    }

    pub async fn wait_ready(&mut self, timeout: Duration) -> Result<Listeners, String> {
        let deadline = Instant::now() + timeout;
        loop {
            let logs = self.logs();
            for line in logs.lines() {
                let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                let fields = &event["fields"];
                if fields.get("event").and_then(|v| v.as_str()) != Some("listeners_ready") {
                    continue;
                }
                let parse = |key: &str| {
                    fields
                        .get(key)
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| format!("listeners_ready missing {key}"))?
                        .parse::<SocketAddr>()
                        .map_err(|err| format!("invalid {key} listener address: {err}"))
                };
                return Ok(Listeners {
                    inference: parse("inference")?,
                    management: parse("management")?,
                });
            }

            if let Some(status) = self.child.try_wait().map_err(|err| err.to_string())? {
                return Err(format!(
                    "Steve exited with {status}; logs:\n{}",
                    self.diagnostic_logs()
                ));
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for listeners_ready; logs:\n{}",
                    self.diagnostic_logs()
                ));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    pub fn database_path(&self) -> PathBuf {
        self._temp.path().join("steve.db")
    }

    fn logs(&self) -> String {
        fs::read_to_string(&self.log_path).unwrap_or_default()
    }

    fn diagnostic_logs(&self) -> String {
        let logs = fs::read(&self.log_path).unwrap_or_default();
        let start = logs.len().saturating_sub(8 * 1024);
        String::from_utf8_lossy(&logs[start..]).into_owned()
    }
}

impl Drop for SteveProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn clean_command(config_path: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_steve"));
    command.arg("--config").arg(config_path).arg("serve");
    for key in std::env::vars_os().map(|(key, _)| key).filter(|key| {
        let key = key.to_string_lossy();
        key.starts_with("STEVE_") || key.starts_with("RUST_LOG")
    }) {
        command.env_remove(key);
    }
    command
}
