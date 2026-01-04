use anyhow::{Result, anyhow};
use log::{debug, info, warn};
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
        }

        info!("Starting cloudflared tunnel to localhost:{}", local_port);

        let mut child = TokioCommand::new("cloudflared")
            .args([
                "tunnel",
                "--url",
                &format!("http://localhost:{}", local_port),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Capture stdout to find the public URL
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to capture stdout"))?;
        let mut reader = BufReader::new(stdout).lines();

        // Store the process
        *self.process.write().await = Some(child);

        // Read output until we find the public URL
        let public_url_lock = Arc::clone(&self.public_url);
        tokio::spawn(async move {
            while let Ok(Some(line)) = reader.next_line().await {
                debug!("cloudflared: {}", line);

                // Look for the URL in the output
                // Cloudflared outputs something like: "https://random-subdomain.trycloudflare.com"
                if line.contains("trycloudflare.com") || line.contains("https://") {
                    if let Some(url) = extract_url_from_line(&line) {
                        info!("Cloudflared tunnel established: {}", url);
                        *public_url_lock.write().await = Some(url.clone());
                    }
                }
            }
        });

        // Wait a bit for the tunnel to establish and URL to be captured
        for _ in 0..30 {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            if let Some(url) = self.public_url.read().await.clone() {
                return Ok(url);
            }
        }

        Err(anyhow!(
            "Failed to get public URL from cloudflared within timeout"
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
    // Try to find URLs in various formats
    let patterns = [
        r"https://[a-zA-Z0-9-]+\.trycloudflare\.com",
        r"https://[a-zA-Z0-9.-]+\.trycloudflare\.com",
    ];

    for pattern in &patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            if let Some(captures) = re.find(line) {
                return Some(captures.as_str().to_string());
            }
        }
    }

    // Fallback: simple string search
    if let Some(start) = line.find("https://") {
        let url_part = &line[start..];
        if let Some(end) = url_part.find(char::is_whitespace) {
            return Some(url_part[..end].to_string());
        } else {
            return Some(url_part.to_string());
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
