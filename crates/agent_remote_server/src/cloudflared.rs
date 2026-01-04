use anyhow::{Result, anyhow};
use log::{info, warn};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command as TokioCommand};
use tokio::sync::RwLock;

#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;

/// Cloudflared tunnel manager
pub struct CloudflaredTunnel {
    process: Arc<RwLock<Option<Child>>>,
    public_url: Arc<RwLock<Option<String>>>,
}

impl CloudflaredTunnel {
    /// Create a new cloudflared tunnel manager
    pub fn new() -> Self {
        Self {
            process: Arc::new(RwLock::new(None)),
            public_url: Arc::new(RwLock::new(None)),
        }
    }

    /// Check if cloudflared is installed
    pub fn is_installed() -> bool {
        Command::new("cloudflared")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    }

    /// Install cloudflared for the current platform
    pub async fn install() -> Result<()> {
        info!("Installing cloudflared...");

        #[cfg(target_os = "linux")]
        {
            Self::install_linux().await
        }

        #[cfg(target_os = "macos")]
        {
            Self::install_macos().await
        }

        #[cfg(target_os = "windows")]
        {
            Self::install_windows().await
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            Err(anyhow!(
                "Unsupported platform for automatic cloudflared installation"
            ))
        }
    }

    #[cfg(target_os = "linux")]
    async fn install_linux() -> Result<()> {
        // Try to find a GUI sudo helper for password prompts
        let askpass = Self::find_askpass_helper();

        // Try to detect the package manager and install accordingly
        if Command::new("which").arg("dnf").status().is_ok() {
            info!("Detected dnf package manager");

            let mut cmd = TokioCommand::new("sudo");
            if let Some(ref askpass_path) = askpass {
                cmd.env("SUDO_ASKPASS", askpass_path);
                cmd.arg("-A"); // Use askpass
            }

            let status = cmd
                .args(["dnf", "install", "-y", "cloudflared"])
                .status()
                .await?;

            if status.success() {
                info!("Successfully installed cloudflared via dnf");
                return Ok(());
            }
            warn!("Failed to install via dnf, trying alternative method");
        }

        if Command::new("which").arg("apt-get").status().is_ok() {
            info!("Detected apt package manager");
            let askpass = Self::find_askpass_helper();

            // Download and install .deb package
            let arch = std::env::consts::ARCH;
            let deb_url = match arch {
                "x86_64" => {
                    "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64.deb"
                }
                "aarch64" => {
                    "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-arm64.deb"
                }
                _ => return Err(anyhow!("Unsupported architecture: {}", arch)),
            };

            let temp_dir = std::env::temp_dir();
            let deb_path = temp_dir.join("cloudflared.deb");

            // Download the package
            let status = TokioCommand::new("curl")
                .args(["-L", "-o", deb_path.to_str().unwrap(), deb_url])
                .status()
                .await?;

            if !status.success() {
                return Err(anyhow!("Failed to download cloudflared package"));
            }

            // Install the package
            let mut cmd = TokioCommand::new("sudo");
            if let Some(ref askpass_path) = askpass {
                cmd.env("SUDO_ASKPASS", askpass_path);
                cmd.arg("-A"); // Use askpass
            }

            let status = cmd
                .args(["dpkg", "-i", deb_path.to_str().unwrap()])
                .status()
                .await?;

            if status.success() {
                info!("Successfully installed cloudflared via apt");
                return Ok(());
            }
        }

        // Fallback: direct binary download
        info!("Installing cloudflared via direct binary download");
        Self::install_binary_linux().await
    }

    #[cfg(target_os = "linux")]
    async fn install_binary_linux() -> Result<()> {
        let askpass = Self::find_askpass_helper();
        let arch = std::env::consts::ARCH;
        let binary_url = match arch {
            "x86_64" => {
                "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64"
            }
            "aarch64" => {
                "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-arm64"
            }
            _ => return Err(anyhow!("Unsupported architecture: {}", arch)),
        };

        let install_path = PathBuf::from("/usr/local/bin/cloudflared");

        // Download the binary
        let mut cmd = TokioCommand::new("sudo");
        if let Some(ref askpass_path) = askpass {
            cmd.env("SUDO_ASKPASS", askpass_path);
            cmd.arg("-A"); // Use askpass
        }

        let status = cmd
            .args([
                "curl",
                "-L",
                "-o",
                install_path.to_str().unwrap(),
                binary_url,
            ])
            .status()
            .await?;

        if !status.success() {
            return Err(anyhow!("Failed to download cloudflared binary"));
        }

        // Make it executable
        let mut cmd = TokioCommand::new("sudo");
        if let Some(ref askpass_path) = askpass {
            cmd.env("SUDO_ASKPASS", askpass_path);
            cmd.arg("-A"); // Use askpass
        }

        let status = cmd
            .args(["chmod", "+x", install_path.to_str().unwrap()])
            .status()
            .await?;

        if status.success() {
            info!("Successfully installed cloudflared binary");
            Ok(())
        } else {
            Err(anyhow!("Failed to make cloudflared executable"))
        }
    }

    /// Find a GUI askpass helper for sudo password prompts
    ///
    /// Linux requires explicit askpass helper configuration for GUI password prompts.
    /// macOS and Windows handle password prompts automatically:
    /// - macOS: Uses osascript for GUI prompts, Homebrew handles elevation automatically
    /// - Windows: Chocolatey uses UAC, Scoop doesn't need elevation (user-level install)
    /// - Windows binary install: Goes to user directory, no elevation needed
    #[cfg(target_os = "linux")]
    fn find_askpass_helper() -> Option<String> {
        // Try common GUI askpass helpers in order of preference
        let helpers = [
            "ksshaskpass",          // KDE
            "ssh-askpass",          // Generic
            "gnome-ssh-askpass",    // GNOME
            "lxqt-openssh-askpass", // LXQt
            "x11-ssh-askpass",      // X11
        ];

        for helper in &helpers {
            if let Ok(output) = Command::new("which").arg(helper).output() {
                if output.status.success() {
                    if let Ok(path) = String::from_utf8(output.stdout) {
                        let path = path.trim();
                        if !path.is_empty() {
                            info!("Found askpass helper: {}", path);
                            return Some(path.to_string());
                        }
                    }
                }
            }
        }

        // Try to find zenity as fallback
        if let Ok(output) = Command::new("which").arg("zenity").output() {
            if output.status.success() {
                // Create a wrapper script for zenity
                let script = r#"#!/bin/sh
zenity --password --title="Sudo Password Required"
"#;
                let temp_dir = std::env::temp_dir();
                let script_path = temp_dir.join("zed-askpass.sh");

                if let Ok(()) = std::fs::write(&script_path, script) {
                    if let Ok(()) = std::fs::set_permissions(
                        &script_path,
                        std::fs::Permissions::from_mode(0o755),
                    ) {
                        info!("Created zenity askpass wrapper: {}", script_path.display());
                        return Some(script_path.to_string_lossy().to_string());
                    }
                }
            }
        }

        warn!("No GUI askpass helper found - sudo may fail without interactive terminal");
        None
    }

    #[cfg(not(target_os = "linux"))]
    fn find_askpass_helper() -> Option<String> {
        None
    }

    #[cfg(target_os = "macos")]
    async fn install_macos() -> Result<()> {
        // Try Homebrew first
        if Command::new("which").arg("brew").status().is_ok() {
            info!("Installing cloudflared via Homebrew");

            // macOS uses osascript for GUI prompts - no special setup needed
            // The system will automatically prompt for password if needed
            let status = TokioCommand::new("brew")
                .args(["install", "cloudflared"])
                .status()
                .await?;

            if status.success() {
                info!("Successfully installed cloudflared via Homebrew");
                return Ok(());
            }
        }

        // Fallback: direct binary download (no sudo needed, installs to user dir)
        info!("Installing cloudflared via direct binary download");
        Self::install_binary_macos().await
    }

    #[cfg(target_os = "macos")]
    async fn install_binary_macos() -> Result<()> {
        let arch = std::env::consts::ARCH;
        let binary_url = match arch {
            "x86_64" => {
                "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-darwin-amd64.tgz"
            }
            "aarch64" => {
                "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-darwin-amd64.tgz"
            }
            _ => return Err(anyhow!("Unsupported architecture: {}", arch)),
        };

        let temp_dir = std::env::temp_dir();
        let archive_path = temp_dir.join("cloudflared.tgz");

        // Install to user's local bin instead of system-wide (no sudo needed)
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let local_bin = PathBuf::from(home).join(".local/bin");
        std::fs::create_dir_all(&local_bin)?;
        let install_path = local_bin.join("cloudflared");

        // Download the archive
        let status = TokioCommand::new("curl")
            .args(["-L", "-o", archive_path.to_str().unwrap(), binary_url])
            .status()
            .await?;

        if !status.success() {
            return Err(anyhow!("Failed to download cloudflared archive"));
        }

        // Extract the archive
        let status = TokioCommand::new("tar")
            .args([
                "xzf",
                archive_path.to_str().unwrap(),
                "-C",
                temp_dir.to_str().unwrap(),
            ])
            .status()
            .await?;

        if !status.success() {
            return Err(anyhow!("Failed to extract cloudflared archive"));
        }

        // Move to install location (no sudo needed)
        let binary_path = temp_dir.join("cloudflared");
        std::fs::rename(&binary_path, &install_path)?;

        // Make it executable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&install_path, std::fs::Permissions::from_mode(0o755))?;
        }

        info!(
            "Successfully installed cloudflared to {}",
            install_path.display()
        );
        info!("Note: ~/.local/bin should be in your PATH");
        Ok(())
    }

    #[cfg(target_os = "windows")]
    async fn install_windows() -> Result<()> {
        // Try Chocolatey first (handles elevation automatically)
        if Command::new("where").arg("choco").status().is_ok() {
            info!("Installing cloudflared via Chocolatey");
            // Chocolatey will prompt for elevation if needed via UAC
            let status = TokioCommand::new("choco")
                .args(["install", "cloudflared", "-y"])
                .status()
                .await?;

            if status.success() {
                info!("Successfully installed cloudflared via Chocolatey");
                return Ok(());
            }
        }

        // Try Scoop (user-level install, no elevation needed)
        if Command::new("where").arg("scoop").status().is_ok() {
            info!("Installing cloudflared via Scoop");
            let status = TokioCommand::new("scoop")
                .args(["install", "cloudflared"])
                .status()
                .await?;

            if status.success() {
                info!("Successfully installed cloudflared via Scoop");
                return Ok(());
            }
        }

        // Fallback: direct binary download to user directory (no elevation needed)
        info!("Installing cloudflared via direct binary download");
        Self::install_binary_windows().await
    }

    #[cfg(target_os = "windows")]
    async fn install_binary_windows() -> Result<()> {
        let arch = std::env::consts::ARCH;
        let binary_url = match arch {
            "x86_64" => {
                "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-windows-amd64.exe"
            }
            _ => return Err(anyhow!("Unsupported architecture: {}", arch)),
        };

        // Install to user's local directory (no elevation needed)
        let local_appdata = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| {
            let userprofile =
                std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\Users\\Default".to_string());
            format!("{}\\AppData\\Local", userprofile)
        });

        let install_dir = PathBuf::from(local_appdata)
            .join("Programs")
            .join("cloudflared");
        let install_path = install_dir.join("cloudflared.exe");

        // Create install directory
        std::fs::create_dir_all(&install_dir)?;

        // Download the binary
        let status = TokioCommand::new("curl")
            .args(["-L", "-o", install_path.to_str().unwrap(), binary_url])
            .status()
            .await?;

        if !status.success() {
            return Err(anyhow!("Failed to download cloudflared binary"));
        }

        info!("Cloudflared installed to {}", install_path.display());
        info!("Note: {} should be in your PATH", install_dir.display());

        Ok(())
    }

    /// Start a cloudflared tunnel to the specified local port
    pub async fn start_tunnel(&self, local_port: u16) -> Result<String> {
        if !Self::is_installed() {
            info!("Cloudflared not found, attempting to install...");
            Self::install().await?;

            // Verify installation
            if !Self::is_installed() {
                return Err(anyhow!("Failed to install cloudflared"));
            }
            info!("Cloudflared installation completed successfully");
        }

        info!("Starting cloudflared tunnel to localhost:{}", local_port);
        info!("This may take 5-15 seconds to establish the tunnel...");

        let mut child = TokioCommand::new("cloudflared")
            .args([
                "tunnel",
                "--url",
                &format!("http://localhost:{}", local_port),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Capture both stdout and stderr to find the public URL
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to capture stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("Failed to capture stderr"))?;

        // Store the process
        *self.process.write().await = Some(child);

        // Read both stdout and stderr until we find the public URL
        let public_url_lock = Arc::clone(&self.public_url);
        let public_url_lock_stderr = Arc::clone(&self.public_url);

        // Read stdout
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                info!("cloudflared stdout: {}", line);

                // Look for the URL in the output
                if line.contains("trycloudflare.com") {
                    info!("Found potential URL in stdout: {}", line);
                    if let Some(url) = extract_url_from_line(&line) {
                        info!("=================================================");
                        info!("Cloudflared tunnel established: {}", url);
                        info!("=================================================");
                        *public_url_lock.write().await = Some(url.clone());
                    } else {
                        warn!("Line contained trycloudflare.com but URL extraction failed");
                    }
                }
            }
        });

        // Read stderr (cloudflared often outputs the URL here)
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                info!("cloudflared stderr: {}", line);

                // Look for the URL in the output
                if line.contains("trycloudflare.com") {
                    info!("Found potential URL in stderr: {}", line);
                    if let Some(url) = extract_url_from_line(&line) {
                        info!("=================================================");
                        info!("Cloudflared tunnel established: {}", url);
                        info!("=================================================");
                        *public_url_lock_stderr.write().await = Some(url.clone());
                    } else {
                        warn!("Line contained trycloudflare.com but URL extraction failed");
                    }
                }
            }
        });

        // Wait longer for the tunnel to establish and URL to be captured
        // Cloudflared can take 5-15 seconds to establish a tunnel
        info!("Waiting for tunnel URL (checking every 100ms for up to 30 seconds)...");
        for i in 0..300 {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            if let Some(url) = self.public_url.read().await.clone() {
                info!("Tunnel ready after {} ms", i * 100);
                return Ok(url);
            }

            // Log progress every 5 seconds
            if i > 0 && i % 50 == 0 {
                info!(
                    "Still waiting for tunnel URL... ({} seconds elapsed)",
                    i / 10
                );
            }
        }

        Err(anyhow!(
            "Failed to get public URL from cloudflared within timeout (30 seconds). \
             Check if cloudflared is working correctly by running: cloudflared tunnel --url http://localhost:{}",
            local_port
        ))
    }

    /// Get the public URL if tunnel is running
    pub async fn get_public_url(&self) -> Option<String> {
        self.public_url.read().await.clone()
    }

    /// Try to get the public URL without blocking (returns immediately)
    pub fn try_get_public_url(&self) -> Result<Option<String>> {
        match self.public_url.try_read() {
            Ok(guard) => Ok(guard.clone()),
            Err(_) => Ok(None),
        }
    }

    /// Stop the cloudflared tunnel
    pub async fn stop(&self) {
        if let Some(mut child) = self.process.write().await.take() {
            info!("Stopping cloudflared tunnel");
            let _ = child.kill().await;
        }
        *self.public_url.write().await = None;
    }

    /// Check if tunnel is running
    pub async fn is_running(&self) -> bool {
        self.process.read().await.is_some()
    }
}

impl Drop for CloudflaredTunnel {
    fn drop(&mut self) {
        // Try to kill the process synchronously if possible
        if let Some(mut child) = self
            .process
            .try_write()
            .ok()
            .and_then(|mut guard| guard.take())
        {
            let _ = child.start_kill();
        }
    }
}

/// Extract URL from cloudflared output line
fn extract_url_from_line(line: &str) -> Option<String> {
    // Cloudflared outputs the tunnel URL with a subdomain pattern
    // Valid format: https://<random-subdomain>.trycloudflare.com
    // Invalid: https://api.trycloudflare.com (this is their API, not a tunnel)

    // Strict regex: must have subdomain before .trycloudflare.com
    // Pattern matches: subdomain with letters, numbers, hyphens
    // Must NOT match: api.trycloudflare.com or just trycloudflare.com
    let patterns = [r"https://[a-zA-Z0-9][a-zA-Z0-9-]+\.trycloudflare\.com"];

    for pattern in &patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            if let Some(captures) = re.find(line) {
                let url = captures.as_str();
                // Explicitly reject api.trycloudflare.com
                if url.contains("api.trycloudflare.com") {
                    warn!("Rejected api.trycloudflare.com (not a tunnel URL): {}", url);
                    continue;
                }
                info!("Extracted URL via regex: {}", url);
                return Some(url.to_string());
            }
        }
    }

    // Fallback: manual parsing with strict validation
    if let Some(start) = line.find("https://") {
        let url_part = &line[start..];

        // Find the end of the URL
        let end_chars = [' ', '\t', '\n', '"', '\'', '|', ']', ')', '\x1b'];
        let end = url_part
            .find(|c: char| end_chars.contains(&c))
            .unwrap_or(url_part.len());

        let url = &url_part[..end];

        // Strict validation:
        // 1. Must contain trycloudflare.com
        // 2. Must NOT be api.trycloudflare.com
        // 3. Must have a subdomain (contains at least 3 parts when split by .)
        if url.contains("trycloudflare.com") && !url.contains("api.trycloudflare.com") {
            // Check that it has a subdomain (e.g., "random-name.trycloudflare.com")
            let host_part = url
                .trim_start_matches("https://")
                .trim_start_matches("http://");
            let parts: Vec<&str> = host_part.split('.').collect();
            if parts.len() >= 3 && parts[0] != "api" && !parts[0].is_empty() {
                info!("Extracted URL via fallback: {}", url);
                return Some(url.to_string());
            } else {
                warn!(
                    "URL validation failed - not enough parts or invalid subdomain: {}",
                    url
                );
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_url_from_line() {
        let line =
            "2024-01-01T12:00:00Z INF | https://random-subdomain.trycloudflare.com | foo bar";
        let url = extract_url_from_line(line);
        assert!(url.is_some());
        assert!(url.unwrap().contains("trycloudflare.com"));
    }

    #[test]
    fn test_extract_url_simple() {
        let line = "Your quick tunnel is available at https://test-abc123.trycloudflare.com";
        let url = extract_url_from_line(line);
        assert_eq!(
            url,
            Some("https://test-abc123.trycloudflare.com".to_string())
        );
    }

    #[tokio::test]
    async fn test_tunnel_creation() {
        let tunnel = CloudflaredTunnel::new();
        assert!(!tunnel.is_running().await);
        assert!(tunnel.get_public_url().await.is_none());
    }
}
