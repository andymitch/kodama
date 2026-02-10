//! Identity and key management for Kodama
//!
//! Handles Ed25519 key generation, persistence, and NodeId derivation.

use anyhow::Result;
use iroh::{PublicKey, SecretKey};
use std::path::Path;
use tracing::info;

/// A keypair for Kodama identity
#[derive(Clone)]
pub struct KeyPair {
    secret: SecretKey,
}

impl KeyPair {
    /// Generate a new random keypair
    pub fn generate() -> Self {
        Self {
            secret: SecretKey::generate(&mut rand::rng()),
        }
    }

    /// Load a keypair from a file, or generate and save if it doesn't exist
    pub fn load_or_generate(path: &Path) -> Result<Self> {
        if path.exists() {
            Self::load(path)
        } else {
            let keypair = Self::generate();
            keypair.save(path)?;
            info!("Generated new secret key at {}", path.display());
            Ok(keypair)
        }
    }

    /// Load a keypair from a file
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
            anyhow::anyhow!("Invalid key file length, expected 32 bytes")
        })?;
        Ok(Self {
            secret: SecretKey::from_bytes(&bytes),
        })
    }

    /// Save the keypair to a file
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(path, self.secret.to_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    /// Get the secret key
    pub fn secret(&self) -> &SecretKey {
        &self.secret
    }

    /// Get the public key
    pub fn public(&self) -> PublicKey {
        self.secret.public()
    }
}

/// High-level identity wrapper
pub struct Identity {
    keypair: KeyPair,
}

impl Identity {
    /// Create a new ephemeral identity
    pub fn ephemeral() -> Self {
        Self {
            keypair: KeyPair::generate(),
        }
    }

    /// Load or create a persistent identity
    pub fn persistent(key_path: &Path) -> Result<Self> {
        Ok(Self {
            keypair: KeyPair::load_or_generate(key_path)?,
        })
    }

    /// Get the public key (NodeId)
    pub fn public_key(&self) -> PublicKey {
        self.keypair.public()
    }

    /// Get the secret key for endpoint binding
    pub fn secret_key(&self) -> &SecretKey {
        self.keypair.secret()
    }

    /// Get the underlying keypair
    pub fn keypair(&self) -> &KeyPair {
        &self.keypair
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_keypair_generate() {
        let kp1 = KeyPair::generate();
        let kp2 = KeyPair::generate();
        assert_ne!(kp1.public(), kp2.public());
    }

    #[test]
    fn test_keypair_persistence() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.key");

        let kp1 = KeyPair::generate();
        kp1.save(&path).unwrap();

        let kp2 = KeyPair::load(&path).unwrap();
        assert_eq!(kp1.public(), kp2.public());
    }

    #[test]
    fn test_identity_ephemeral() {
        let id1 = Identity::ephemeral();
        let id2 = Identity::ephemeral();
        assert_ne!(id1.public_key(), id2.public_key());
    }
}
