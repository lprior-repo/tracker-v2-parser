use std::path::Path;

use anyhow::{Result, ensure};

use super::model::{self, Configuration};

fn escaped(value: &str) -> Result<String> {
    model::valid_text(value)?;
    ensure!(
        value.len() <= 16_384,
        "LaunchAgent string exceeds its byte limit"
    );
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}

pub(super) fn render(
    configuration: &Configuration,
    config: &Path,
    logs: &Path,
    label: &str,
    search_path: &str,
) -> Result<String> {
    let launcher = escaped(model::path_text(&configuration.launcher)?)?;
    let config_text = escaped(model::path_text(config)?)?;
    let working =
        escaped(model::path_text(config.parent().ok_or_else(|| {
            anyhow::anyhow!("worker configuration has no parent")
        })?)?)?;
    let stdout = escaped(model::path_text(&logs.join("stdout.log"))?)?;
    let stderr = escaped(model::path_text(&logs.join("stderr.log"))?)?;
    let label = escaped(label)?;
    let search_path = escaped(search_path)?;
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array><string>{launcher}</string><string>worker-launch</string><string>--config</string><string>{config_text}</string></array>
<key>WorkingDirectory</key><string>{working}</string>
<key>EnvironmentVariables</key><dict><key>PATH</key><string>{search_path}</string><key>RUST_LOG</key><string>info</string></dict>
<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
<key>ThrottleInterval</key><integer>10</integer><key>ExitTimeOut</key><integer>120</integer><key>Umask</key><integer>63</integer>
<key>StandardOutPath</key><string>{stdout}</string><key>StandardErrorPath</key><string>{stderr}</string>
</dict></plist>
"#
    );
    ensure!(body.len() <= 65_536, "worker LaunchAgent exceeds 64 KiB");
    Ok(body)
}
