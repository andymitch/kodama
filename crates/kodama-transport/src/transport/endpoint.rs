//! Iroh endpoint wrapper for Kodama

use anyhow::Result;
use iroh::endpoint::{Connection, Endpoint, RelayMode};
use iroh::{PublicKey, SecretKey};
use std::path::Path;
use tracing::{info, warn};

use kodama_core::ALPN;

/// Wrapper around iroh Endpoint with Kodama-specific configuration
pub struct KodamaEndpoint {
    endpoint: Endpoint,
}

impl KodamaEndpoint {
    /// Create a new endpoint, loading or generating a secret key
    ///
    /// If `key_path` is provided and exists, loads the key from disk.
    /// If `key_path` is provided but doesn't exist, generates a new key and saves it.
    /// If `key_path` is None, generates an ephemeral key.
    pub async fn new(key_path: Option<&Path>) -> Result<Self> {
        let secret_key = match key_path {
            Some(path) if path.exists() => {
                let bytes = std::fs::read(path)?;
                let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
                    anyhow::anyhow!("Invalid key file length, expected 32 bytes")
                })?;
                SecretKey::from_bytes(&bytes)
            }
            Some(path) => {
                let key = SecretKey::generate(&mut rand::rng());
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(path, key.to_bytes())?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
                }
                info!("Generated new secret key at {}", path.display());
                key
            }
            None => SecretKey::generate(&mut rand::rng()),
        };

        let endpoint = Endpoint::builder()
            .secret_key(secret_key)
            .relay_mode(RelayMode::Default)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await?;

        Ok(Self { endpoint })
    }

    /// Get this endpoint's public key (used as EndpointId for connections)
    pub fn public_key(&self) -> PublicKey {
        self.endpoint.secret_key().public()
    }

    /// Get the underlying iroh Endpoint
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Connect to a remote endpoint by public key
    ///
    /// Uses iroh's discovery services to find the relay URL and direct addresses.
    pub async fn connect(&self, remote: PublicKey) -> Result<Connection> {
        let conn = self.endpoint.connect(remote, ALPN).await?;
        Ok(conn)
    }

    /// Accept an incoming connection
    ///
    /// Returns None only if the endpoint is closed.
    /// Transient handshake failures are logged and retried.
    pub async fn accept(&self) -> Option<Connection> {
        loop {
            let incoming = self.endpoint.accept().await?;
            match incoming.await {
                Ok(conn) => return Some(conn),
                Err(e) => {
                    warn!("Incoming connection handshake failed: {e}");
                    continue;
                }
            }
        }
    }

    /// Close the endpoint gracefully
    pub async fn close(&self) {
        self.endpoint.close().await;
    }
}
