use anyhow::{Result, anyhow};
use log::{debug, info, warn};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command as TokioCommand};
use tokio::sync::RwLock;

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
        // Try to detect the package manager and install accordingly
        if Command::new("which").arg("dnf").status().is_ok() {
            info!("Detected dnf package manager");
            let status = TokioCommand::new("sudo")
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
            let status = TokioCommand::new("sudo")
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
        let status = TokioCommand::new("sudo")
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
        let status = TokioCommand::new("sudo")
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

    #[cfg(target_os = "macos")]
    async fn install_macos() -> Result<()> {
        // Try Homebrew first
        if Command::new("which").arg("brew").status().is_ok() {
            info!("Installing cloudflared via Homebrew");
            let status = TokioCommand::new("brew")
                .args(["install", "cloudflared"])
                .status()
                .await?;

            if status.success() {
                info!("Successfully installed cloudflared via Homebrew");
                return Ok(());
            }
        }

        // Fallback: direct binary download
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
        let install_path = PathBuf::from("/usr/local/bin/cloudflared");

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
                "-xzf",
                archive_path.to_str().unwrap(),
                "-C",
                temp_dir.to_str().unwrap(),
            ])
            .status()
            .await?;

        if !status.success() {
            return Err(anyhow!("Failed to extract cloudflared archive"));
        }

        // Move to install location
        let binary_path = temp_dir.join("cloudflared");
        let status = TokioCommand::new("sudo")
            .args([
                "mv",
                binary_path.to_str().unwrap(),
                install_path.to_str().unwrap(),
            ])
            .status()
            .await?;

        if !status.success() {
            return Err(anyhow!("Failed to move cloudflared to install location"));
        }

        // Make it executable
        let status = TokioCommand::new("sudo")
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

    #[cfg(target_os = "windows")]
    async fn install_windows() -> Result<()> {
        // Try Chocolatey first
        if Command::new("where").arg("choco").status().is_ok() {
            info!("Installing cloudflared via Chocolatey");
            let status = TokioCommand::new("choco")
                .args(["install", "cloudflared", "-y"])
                .status()
                .await?;

            if status.success() {
                info!("Successfully installed cloudflared via Chocolatey");
                return Ok(());
            }
        }

        // Try Scoop
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

        // Fallback: direct binary download
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

        let install_dir = PathBuf::from(
            std::env::var("PROGRAMFILES").unwrap_or_else(|_| "C:\\Program Files".to_string()),
        )
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

        // Add to PATH (requires admin privileges)
        info!(
            "Cloudflared installed to {}. You may need to add it to PATH manually or restart your terminal.",
            install_path.display()
        );

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
