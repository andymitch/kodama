use std::collections::VecDeque;
use std::time::Instant;

use iroh::PublicKey;
use kodama_server::{PeerRole, RouterHandle, RouterStats, StorageStats};

const MAX_LOG_LINES: usize = 200;

pub struct App {
    pub server_public_key: String,
    pub router_handle: RouterHandle,
    pub stats: RouterStats,
    pub peers: Vec<(PublicKey, PeerRole)>,
    pub log_messages: VecDeque<String>,
    pub uptime_start: Instant,
    pub storage_stats: Option<StorageStats>,
}

impl App {
    pub fn new(server_public_key: String, router_handle: RouterHandle) -> Self {
        Self {
            server_public_key,
            router_handle,
            stats: RouterStats {
                cameras_connected: 0,
                clients_connected: 0,
                frames_received: 0,
                frames_broadcast: 0,
            },
            peers: Vec::new(),
            log_messages: VecDeque::with_capacity(MAX_LOG_LINES),
            uptime_start: Instant::now(),
            storage_stats: None,
        }
    }

    pub async fn refresh(&mut self) {
        self.stats = self.router_handle.stats().await;
        self.peers = self.router_handle.peers().await;
    }

    pub fn push_log(&mut self, msg: String) {
        if self.log_messages.len() >= MAX_LOG_LINES {
            self.log_messages.pop_front();
        }
        self.log_messages.push_back(msg);
    }

    pub fn uptime_str(&self) -> String {
        let secs = self.uptime_start.elapsed().as_secs();
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        let secs = secs % 60;
        if hours > 0 {
            format!("{}h{:02}m{:02}s", hours, mins, secs)
        } else {
            format!("{}m{:02}s", mins, secs)
        }
    }
}
