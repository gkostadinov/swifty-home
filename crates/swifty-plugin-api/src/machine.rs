//! What the [`MachineHost`](crate::MachineHost) extension passes: machines,
//! chart spans, consoles and further hosts.

use std::net::SocketAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::Secret;

/// A chart's time span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoryRange {
    /// The last hour.
    #[serde(rename = "1h")]
    Hour,
    /// The last day.
    #[serde(rename = "24h")]
    Day,
    /// The last week.
    #[serde(rename = "7d")]
    Week,
}

impl HistoryRange {
    /// The span named "1h", "24h" or "7d".
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "1h" => Some(Self::Hour),
            "24h" => Some(Self::Day),
            "7d" => Some(Self::Week),
            _ => None,
        }
    }

    /// The span's name: "1h", "24h" or "7d".
    pub fn name(self) -> &'static str {
        match self {
            Self::Hour => "1h",
            Self::Day => "24h",
            Self::Week => "7d",
        }
    }

    /// The span in milliseconds.
    pub fn span_ms(self) -> i64 {
        match self {
            Self::Hour => 3_600_000,
            Self::Day => 24 * 3_600_000,
            Self::Week => 7 * 24 * 3_600_000,
        }
    }

    /// How finely stored samples are averaged for a chart of this span.
    pub fn bucket_ms(self) -> i64 {
        match self {
            Self::Hour => 60_000,
            Self::Day => 5 * 60_000,
            Self::Week => 30 * 60_000,
        }
    }
}

/// One VM that a host reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeenMachine {
    /// The id of the host it runs on.
    pub host: String,
    /// The VM's number on that host.
    pub vmid: u32,
    /// The VM's name, as the host gave it.
    pub name: String,
}

/// A further host, after every check on what was asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtraHost {
    /// Its id.
    pub id: String,
    /// Where it answers.
    pub address: SocketAddr,
    /// Its node name.
    pub node: String,
    /// The file that holds its API token.
    pub token_file: PathBuf,
    /// The file that holds the CA its certificate chains to.
    pub ca_file: PathBuf,
}

/// What a host issued for one console. The ticket lives only in memory, so this
/// does not serialise.
#[derive(Debug, Clone)]
pub struct ConsoleGrant {
    /// The VM the console is for.
    pub target: String,
    /// The port the console relay listens on.
    pub port: u16,
    /// The single-use ticket for it.
    pub ticket: Secret,
}
