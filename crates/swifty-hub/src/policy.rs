//! The risk-tier policy: which route each tier may take, on each channel.
//!
//! These rules live on the server. A hold-to-confirm or a covered switch in the
//! console is a client gesture and proves nothing here.

use serde::Serialize;
use swifty_plugin_api::Tier;

use crate::runtime::index::DeviceIndex;

/// A single-use confirmation for a tier 2 action lasts this long.
pub const CONFIRMATION_TTL_MS: i64 = 30_000;
/// A step-up challenge lasts this long once issued.
pub const STEP_UP_TTL_MS: i64 = 60_000;
/// A tier 3 action must have its step-up started within this long.
pub const CRITICAL_PREPARE_TTL_MS: i64 = 120_000;

/// Who someone is in this house. There is one owner, made by first enrollment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Owner,
    Family,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Family => "family",
        }
    }

    /// Anything unrecognised is the lesser role.
    pub fn from_stored(text: &str) -> Self {
        if text == "owner" {
            Role::Owner
        } else {
            Role::Family
        }
    }
}

/// Everything a family member may run. Anything not named here is owner-only, so
/// a new catalog entry starts out closed.
const FAMILY_ACTIONS: &[&str] = &[
    "state.read",
    "light.power",
    "light.brightness",
    "light.hue",
    "lights.power",
    "lights.brightness",
    "scene.apply",
    "scene.movie",
    "room.power",
    "room.brightness",
    "room.scene",
    "speaker.volume",
    "speaker.mute",
    "speaker.night",
    "speaker.play",
    "speaker.pause",
    "tv.power",
    "tv.input",
    "tv.key",
    "pets.feed",
    "pets.clean",
];

/// The scenes a person may keep as a favourite.
pub const SCENE_FAVOURITES: &[&str] = &["evening", "focus", "all_off", "movie"];

/// The group of every light, which a family member may read.
const FAMILY_GROUP: &str = "all_bulbs";

/// Whether this person may read this device or see it listed. A family member sees
/// what the index says is for the family: lights, music, TV, pets and the camera,
/// going by each device's class. Anything the index does not know is not seen.
pub fn may_see(index: &DeviceIndex, role: Role, target: &str) -> bool {
    role == Role::Owner || index.family_may_see(target) || target == FAMILY_GROUP
}

/// Whether this person may run this action on this target at all. Checked on the
/// server, for every channel, before anything else about the request.
pub fn may_run(index: &DeviceIndex, role: Role, action: &str, target: &str) -> bool {
    match role {
        Role::Owner => true,
        Role::Family => FAMILY_ACTIONS.contains(&action) && (action != "state.read" || may_see(index, role, target)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// One request, with a session.
    Direct,
    /// Prepare, then confirm with the single-use token.
    Confirm,
    /// Prepare, then confirm with a passkey assertion bound to the action.
    StepUp,
}

impl Flow {
    pub fn name(self) -> &'static str {
        match self {
            Flow::Direct => "direct",
            Flow::Confirm => "prepare_confirm",
            Flow::StepUp => "prepare_step_up",
        }
    }
}

pub fn flow(tier: Tier) -> Flow {
    match tier {
        Tier::Read | Tier::Comfort | Tier::SafeDirection => Flow::Direct,
        Tier::Consequential => Flow::Confirm,
        Tier::Critical => Flow::StepUp,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Panel,
    Palette,
    Voice,
    /// Proposed by the model. It follows every rule voice follows.
    Assistant,
}

impl Channel {
    pub fn name(self) -> &'static str {
        match self {
            Channel::Panel => "panel",
            Channel::Palette => "palette",
            Channel::Voice => "voice",
            Channel::Assistant => "assistant",
        }
    }

    /// Only the two client-selectable channels parse. `voice` can only be set
    /// by the server's own voice route.
    pub fn parse_client(text: Option<&str>) -> Option<Self> {
        match text {
            None | Some("panel") => Some(Channel::Panel),
            Some("palette") => Some(Channel::Palette),
            _ => None,
        }
    }

    pub fn from_stored(text: &str) -> Self {
        match text {
            "palette" => Channel::Palette,
            "voice" => Channel::Voice,
            "assistant" => Channel::Assistant,
            _ => Channel::Panel,
        }
    }
}

/// What the voice route may do with a resolved action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceRoute {
    Execute,
    /// Prepared, and it still needs a tap to confirm.
    PrepareForTap,
    /// Prepared only. Voice can never supply the step-up.
    PrepareForStepUp,
}

pub fn voice_route(tier: Tier) -> VoiceRoute {
    match tier {
        // The emergency stop only takes risk away, and it is said by its own name.
        Tier::Read | Tier::Comfort | Tier::SafeDirection => VoiceRoute::Execute,
        Tier::Consequential => VoiceRoute::PrepareForTap,
        Tier::Critical => VoiceRoute::PrepareForStepUp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::index::{self, fixture::DEVICES, DeviceEntry};

    #[test]
    fn tiers_map_to_flows() {
        assert_eq!(flow(Tier::Read), Flow::Direct);
        assert_eq!(flow(Tier::Comfort), Flow::Direct);
        assert_eq!(flow(Tier::SafeDirection), Flow::Direct);
        assert_eq!(flow(Tier::Consequential), Flow::Confirm);
        assert_eq!(flow(Tier::Critical), Flow::StepUp);
    }

    #[test]
    fn voice_finishes_tier_one_and_the_emergency_stop_and_prepares_the_rest() {
        assert_eq!(voice_route(Tier::Read), VoiceRoute::Execute);
        assert_eq!(voice_route(Tier::Comfort), VoiceRoute::Execute);
        assert_eq!(voice_route(Tier::Consequential), VoiceRoute::PrepareForTap);
        assert_eq!(voice_route(Tier::Critical), VoiceRoute::PrepareForStepUp);
        assert_eq!(voice_route(Tier::SafeDirection), VoiceRoute::Execute);
    }

    #[test]
    fn clients_cannot_claim_the_voice_channel() {
        assert_eq!(Channel::parse_client(None), Some(Channel::Panel));
        assert_eq!(Channel::parse_client(Some("palette")), Some(Channel::Palette));
        assert_eq!(Channel::parse_client(Some("voice")), None);
    }

    /// The seeded house with a light of each kind added after discovery.
    fn house() -> DeviceIndex {
        let mut entries: Vec<DeviceEntry> = DEVICES
            .iter()
            .map(|d| DeviceEntry { target: d.target.to_string(), name: d.name.to_string(), instance: d.instance })
            .collect();
        for (target, instance) in [("wiz-aabbccddeeff", "wiz"), ("hue-1", "hue")] {
            entries.push(DeviceEntry { target: target.to_string(), name: target.to_string(), instance });
        }
        index::legacy(&entries, &[])
    }

    #[test]
    fn family_is_closed_by_default() {
        let index = house();
        let may_run = |role, action, target| may_run(&index, role, action, target);
        for (action, target) in [
            ("light.power", "bulb1"),
            ("room.power", "abc"),
            ("speaker.volume", "sonos"),
            ("tv.power", "tv"),
            ("pets.feed", "feeder"),
            ("state.read", "camera"),
            ("state.read", "wiz-aabbccddeeff"),
            ("state.read", "hue-1"),
            ("light.power", "hue-1"),
            ("speaker.night", "sonos"),
            ("tv.key", "tv"),
        ] {
            assert!(may_run(Role::Family, action, target), "{action} {target}");
        }
        for (action, target) in [
            ("vm.start", "vm100"),
            ("vm.stop", "vm100"),
            ("host.reboot", "host"),
            ("console.open", "vm102"),
            ("passkey.register", "new"),
            ("passkey.revoke", "x"),
            ("machine.approve", "vm104"),
            ("user.invite", "new"),
            ("print.start", "printer"),
            ("print.estop", "printer"),
            ("router.wireguard_restart", "router"),
            ("state.read", "vm100"),
            ("state.read", "host"),
            ("state.read", "collector"),
            ("state.read", "printer"),
            ("state.read", "all_vms"),
            ("something.new", "x"),
        ] {
            assert!(!may_run(Role::Family, action, target), "{action} {target}");
            assert!(may_run(Role::Owner, action, target), "{action} {target}");
        }
    }

    #[test]
    fn family_sees_what_the_index_says_is_for_the_family() {
        let index = house();
        for target in ["bulb1", "all_bulbs", "hue", "hue-1", "wiz-aabbccddeeff", "sonos", "sub", "tv", "camera"] {
            assert!(may_see(&index, Role::Family, target), "{target}");
        }
        for target in ["vm100", "all_vms", "host", "printer", "collector", "plex", "router", "wiz-aabbccddeeee", "-"] {
            assert!(!may_see(&index, Role::Family, target), "{target}");
            assert!(may_see(&index, Role::Owner, target), "{target}");
        }
        assert!(!may_see(&DeviceIndex::default(), Role::Family, "bulb1"), "nothing the index lacks");
    }

    #[test]
    fn an_unknown_stored_role_is_the_lesser_one() {
        assert_eq!(Role::from_stored("owner"), Role::Owner);
        assert_eq!(Role::from_stored("family"), Role::Family);
        assert_eq!(Role::from_stored("admin"), Role::Family);
        assert_eq!(Role::from_stored(""), Role::Family);
    }

    #[test]
    fn the_windows_in_the_spec() {
        assert_eq!(CONFIRMATION_TTL_MS, 30_000);
        assert_eq!(STEP_UP_TTL_MS, 60_000);
    }
}
