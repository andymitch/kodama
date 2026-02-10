//! Kodama Firmware Binary
//!
//! Runs as either a camera or a relay, selected at runtime.
//!
//! ## Usage
//!
//! ```bash
//! # Camera mode (default)
//! KODAMA_SERVER_KEY=<key> kodama-firmware
//!
//! # Relay mode
//! kodama-firmware --mode relay
//! KODAMA_MODE=relay kodama-firmware
//!
//! # Camera with test source
//! KODAMA_SERVER_KEY=<key> kodama-firmware --test-source
//! ```

mod camera;
mod relay;
mod update;

use anyhow::Result;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // Determine mode: --mode relay/camera, or KODAMA_MODE env, default camera
    let mode = if let Some(pos) = args.iter().position(|a| a == "--mode") {
        args.get(pos + 1)
            .map(|s| s.to_string())
            .unwrap_or_else(|| "camera".to_string())
    } else {
        std::env::var("KODAMA_MODE").unwrap_or_else(|_| "camera".to_string())
    };

    match mode.as_str() {
        "relay" => relay::run(),
        _ => camera::run(),
    }
}
