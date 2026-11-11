//! Every plugin in this build. A plugin that is not listed here does not exist.
//!
//! Each first-party plugin sits behind a cargo feature of the same name, and
//! `all-plugins` (the default) turns every one on. `cargo build -p swifty
//! --no-default-features --features wiz,hue` builds a hub with two.

use swifty_plugin_api::Plugin;

/// Every plugin compiled into this build, in a fixed order.
pub fn compiled_in() -> Vec<&'static dyn Plugin> {
    vec![
        #[cfg(feature = "wiz")]
        &swifty_plugin_wiz::PLUGIN,
        #[cfg(feature = "hue")]
        &swifty_plugin_hue::PLUGIN,
        #[cfg(feature = "sonos")]
        &swifty_plugin_sonos::PLUGIN,
        #[cfg(feature = "webos")]
        &swifty_plugin_webos::PLUGIN,
        #[cfg(feature = "tuya")]
        &swifty_plugin_tuya::PLUGIN,
        #[cfg(feature = "toshiba")]
        &swifty_plugin_toshiba::PLUGIN,
        #[cfg(feature = "petkit")]
        &swifty_plugin_petkit::PLUGIN,
        #[cfg(feature = "moonraker")]
        &swifty_plugin_moonraker::PLUGIN,
        #[cfg(feature = "proxmox")]
        &swifty_plugin_proxmox::PLUGIN,
        #[cfg(feature = "plex")]
        &swifty_plugin_plex::PLUGIN,
        #[cfg(feature = "camera")]
        &swifty_plugin_camera::PLUGIN,
        #[cfg(feature = "glimdock")]
        &swifty_plugin_glimdock::PLUGIN,
        #[cfg(feature = "mock")]
        &swifty_plugin_mock::PLUGIN,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_plugin_is_listed_once_and_speaks_this_api() {
        let plugins = compiled_in();
        let mut ids: Vec<&str> = plugins.iter().map(|plugin| plugin.manifest().id).collect();
        for plugin in &plugins {
            assert_eq!(plugin.manifest().api, swifty_plugin_api::API_VERSION, "{}", plugin.manifest().id);
        }
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "a plugin id is listed twice");
    }

    #[cfg(feature = "all-plugins")]
    #[test]
    fn the_default_build_has_every_first_party_plugin() {
        let ids: Vec<&str> = compiled_in().iter().map(|plugin| plugin.manifest().id).collect();
        assert_eq!(
            ids,
            [
                "wiz",
                "hue",
                "sonos",
                "webos",
                "tuya",
                "toshiba",
                "petkit",
                "moonraker",
                "proxmox",
                "plex",
                "camera",
                "glimdock",
                "mock"
            ]
        );
    }
}
