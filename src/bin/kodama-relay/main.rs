//! Kodama Relay Binary
//!
//! Standalone relay server for NAT traversal assistance.
//!
//! ## Usage
//!
//! ```bash
//! kodama-relay
//! ```

use anyhow::Result;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("kodama=info".parse().unwrap()),
        )
        .init();

    info!("Kodama Relay starting");
    info!("Relay functionality not yet implemented");

    // TODO: Implement relay server for NAT traversal assistance
    // This will help cameras and clients that cannot establish direct connections

    Ok(())
}
