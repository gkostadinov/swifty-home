//! Risk tiers and parameter schemas, shared by the hub's catalog and the
//! custom actions a plugin declares.

use serde::Serialize;
use serde_json::{json, Value};

use crate::Examples;

/// How much an action can do, and so what it takes to run it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// 0: reads. Needs a session.
    Read,
    /// 1: comfort. Needs a session.
    Comfort,
    /// 2: consequential. Prepare, then confirm.
    Consequential,
    /// 3: critical. Prepare, then a passkey assertion bound to the action.
    Critical,
    /// "S": moves the system toward safety (the printer emergency stop). Needs
    /// a session, and voice can run it when it is named.
    SafeDirection,
}

impl Tier {
    /// The value the console uses for this tier.
    pub fn wire(self) -> Value {
        match self {
            Tier::Read => json!(0),
            Tier::Comfort => json!(1),
            Tier::Consequential => json!(2),
            Tier::Critical => json!(3),
            Tier::SafeDirection => json!("S"),
        }
    }

    /// The tier's name, as it is serialised.
    pub fn name(self) -> &'static str {
        match self {
            Tier::Read => "read",
            Tier::Comfort => "comfort",
            Tier::Consequential => "consequential",
            Tier::Critical => "critical",
            Tier::SafeDirection => "safe_direction",
        }
    }

    /// The tier as one character, as it is stored.
    pub fn code(self) -> &'static str {
        match self {
            Tier::Read => "0",
            Tier::Comfort => "1",
            Tier::Consequential => "2",
            Tier::Critical => "3",
            Tier::SafeDirection => "S",
        }
    }
}

/// What a parameter may hold.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    /// `true` or `false`.
    Bool,
    /// A whole number in a range.
    Int {
        /// The lowest value.
        min: i64,
        /// The highest value.
        max: i64,
    },
    /// One of these words.
    Enum(&'static [&'static str]),
    /// A short human label: printable characters only.
    Label {
        /// The most characters it may have.
        max_len: usize,
    },
}

/// One parameter of an action.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ParamDef {
    /// The parameter's name in the action's `params`.
    pub name: &'static str,
    /// What it may hold.
    pub kind: ParamKind,
    /// Whether it must be given.
    pub required: bool,
    /// One sentence for the person filling it in.
    pub help: &'static str,
}

impl Examples for Tier {
    fn examples() -> Vec<Self> {
        vec![Tier::Read, Tier::Comfort, Tier::Consequential, Tier::Critical, Tier::SafeDirection]
    }
}

impl Examples for ParamKind {
    fn examples() -> Vec<Self> {
        vec![
            ParamKind::Bool,
            ParamKind::Int { min: 0, max: 100 },
            ParamKind::Enum(&["slow", "fast"]),
            ParamKind::Label { max_len: 40 },
        ]
    }
}
