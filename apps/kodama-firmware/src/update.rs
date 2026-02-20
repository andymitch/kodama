//! OTA firmware update: download, verify, replace, restart.
//!
//! The update flow:
//! 1. Stream download to `<binary>.new` with incremental SHA256
//! 2. Verify hash matches expected value
//! 3. chmod 755 the new binary
//! 4. Rename current binary to `<binary>.prev` (backup for rollback)
//! 5. Rename staging binary to current path (atomic on same filesystem)
//! 6. Exit with code 42 — systemd `Restart=always` brings up the new binary

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::Instant;
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

/// Maximum download size (100 MB)
const MAX_DOWNLOAD_SIZE: u64 = 100 * 1024 * 1024;

/// Exit code used to signal "restart with new binary"
pub const UPDATE_EXIT_CODE: i32 = 42;

pub struct UpdateState {
    url: String,
    expected_sha256: String,
    binary_path: PathBuf,
    staging_path: PathBuf,
    backup_path: PathBuf,
}

impl UpdateState {
    pub fn new(url: String, sha256: String) -> Result<Self> {
        let binary_path =
            std::env::current_exe().context("Failed to determine current executable path")?;

        let dir = binary_path
            .parent()
            .context("Binary has no parent directory")?;
        let staging_path = dir.join("kodama-firmware.new");
        let backup_path = dir.join("kodama-firmware.prev");

        Ok(Self {
            url,
            expected_sha256: sha256.to_lowercase(),
            binary_path,
            staging_path,
            backup_path,
        })
    }

    /// Run the full update pipeline. On success, calls `std::process::exit(42)`.
    pub async fn execute(self) -> Result<()> {
        info!(
            "Starting OTA update: url={}, expected_sha256={}",
            self.url, self.expected_sha256
        );

        // Step 1: Stream download with incremental hash
        self.download_and_verify().await?;

        // Step 2: Set executable permission
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(&self.staging_path, perms)
                .context("Failed to chmod staging binary")?;
        }

        // Step 3: Backup current binary
        if self.binary_path.exists() {
            std::fs::rename(&self.binary_path, &self.backup_path)
                .context("Failed to backup current binary")?;
            info!("Backed up current binary to {:?}", self.backup_path);
        }

        // Step 4: Move staging to current (atomic on same filesystem)
        if let Err(e) = std::fs::rename(&self.staging_path, &self.binary_path) {
            // Rollback: restore backup
            warn!("Failed to install new binary: {}. Rolling back.", e);
            if self.backup_path.exists() {
                let _ = std::fs::rename(&self.backup_path, &self.binary_path);
            }
            bail!("Failed to install new binary: {}", e);
        }

        info!(
            "OTA update installed successfully. Restarting with exit code {}.",
            UPDATE_EXIT_CODE
        );

        // Step 5: Exit — systemd restarts us with the new binary
        std::process::exit(UPDATE_EXIT_CODE);
    }

    async fn download_and_verify(&self) -> Result<()> {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .context("Failed to build HTTP client")?;

        let response = client
            .get(&self.url)
            .send()
            .await
            .context("Failed to start download")?
            .error_for_status()
            .context("Download returned error status")?;

        // Check content-length if available
        if let Some(len) = response.content_length() {
            if len > MAX_DOWNLOAD_SIZE {
                bail!(
                    "Download too large: {} bytes (max {} bytes)",
                    len,
                    MAX_DOWNLOAD_SIZE
                );
            }
        }

        // Clean up any leftover staging file
        let _ = tokio::fs::remove_file(&self.staging_path).await;

        let mut file = tokio::fs::File::create(&self.staging_path)
            .await
            .context("Failed to create staging file")?;

        let mut hasher = Sha256::new();
        let mut total_bytes: u64 = 0;
        let start = Instant::now();
        let mut last_log = Instant::now();

        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("Error reading download stream")?;
            total_bytes += chunk.len() as u64;

            if total_bytes > MAX_DOWNLOAD_SIZE {
                // Clean up
                drop(file);
                let _ = tokio::fs::remove_file(&self.staging_path).await;
                bail!("Download exceeded max size ({} bytes)", MAX_DOWNLOAD_SIZE);
            }

            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .context("Failed to write to staging file")?;

            // Progress log every 5s
            if last_log.elapsed().as_secs() >= 5 {
                let elapsed = start.elapsed().as_secs_f64();
                let mbps = (total_bytes as f64 * 8.0) / (elapsed * 1_000_000.0);
                info!(
                    "Download progress: {:.1} MB ({:.2} Mbps)",
                    total_bytes as f64 / (1024.0 * 1024.0),
                    mbps,
                );
                last_log = Instant::now();
            }
        }

        file.flush().await?;
        file.sync_all()
            .await
            .context("Failed to fsync staging file")?;
        drop(file);

        let elapsed = start.elapsed();
        info!(
            "Download complete: {} bytes in {:.1}s",
            total_bytes,
            elapsed.as_secs_f64()
        );

        // Verify hash
        let actual_hash = hex::encode(hasher.finalize());
        if actual_hash != self.expected_sha256 {
            let _ = tokio::fs::remove_file(&self.staging_path).await;
            bail!(
                "SHA256 mismatch: expected {}, got {}",
                self.expected_sha256,
                actual_hash
            );
        }

        info!("SHA256 verified: {}", actual_hash);
        Ok(())
    }
}
