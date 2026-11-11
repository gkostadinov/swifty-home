//! Actions a plugin declares in its manifest, added to the catalog with the hub's
//! floors applied.
//!
//! A plugin only asks for a tier. The hub decides it: at least Comfort; at least
//! Consequential when a repeat would do the thing twice; at least Critical on a VM,
//! a host or a router; never Read and never SafeDirection. A request below a floor
//! is not an error: the tier is raised, and both values are kept so the plugin page
//! can show them. Every custom action is owner-only, because the family's list
//! names standard actions only, and voice and Swifty never offer one.
//!
//! Each action becomes one `ActionDef` that lives as long as the process, so
//! `&'static ActionDef` holds for custom actions too and [`find`](super::find) stays
//! a plain function.

use std::sync::RwLock;

use swifty_plugin_api::{CapKind, CustomAction, DeviceClass, Plugin, Tier};

use super::{ActionDef, Executor, Targets, CATALOG};

/// The shortest and longest wait for a custom action's confirmation. A manifest's
/// value outside this is brought inside it.
pub const CONFIRM_TIMEOUT_RANGE_MS: (u64, u64) = (1_000, 180_000);

/// A custom action as the catalog holds it.
#[derive(Debug, Clone, Copy)]
pub struct Registered {
    /// The plugin that declared it.
    pub plugin: &'static str,
    /// The tier its manifest asked for.
    pub requested: Tier,
    /// The entry the catalog uses, with the tier the hub applied.
    pub def: &'static ActionDef,
}

static REGISTERED: RwLock<Vec<Registered>> = RwLock::new(Vec::new());

/// An order of tiers by what they take to run. SafeDirection is not on it: no
/// custom action may have it.
fn rank(tier: Tier) -> u8 {
    match tier {
        Tier::Read => 0,
        Tier::Comfort | Tier::SafeDirection => 1,
        Tier::Consequential => 2,
        Tier::Critical => 3,
    }
}

fn of_rank(rank: u8) -> Tier {
    match rank {
        0 | 1 => Tier::Comfort,
        2 => Tier::Consequential,
        _ => Tier::Critical,
    }
}

/// The tier the hub gives a custom action: what it asked for, raised to every
/// floor that applies to it.
pub fn floor(action: &CustomAction) -> Tier {
    let mut least = rank(Tier::Comfort);
    if !action.idempotent {
        least = least.max(rank(Tier::Consequential));
    }
    if matches!(action.class, DeviceClass::Vm | DeviceClass::Host | DeviceClass::Router) {
        least = least.max(rank(Tier::Critical));
    }
    of_rank(rank(action.tier).max(least))
}

/// Why this action cannot join the catalog, if it cannot: its id must be the
/// plugin's id, a dot and a verb, and must not be taken already.
fn problem(plugin: &str, action: &CustomAction) -> Option<String> {
    let Some(verb) = action.id.strip_prefix(plugin).and_then(|rest| rest.strip_prefix('.')) else {
        return Some(format!("{} does not start with \"{plugin}.\"", action.id));
    };
    let shaped = (1..=32).contains(&verb.len())
        && verb.as_bytes()[0].is_ascii_lowercase()
        && verb.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if !shaped {
        return Some(format!("{} has no verb of lower-case letters, digits and underscores", action.id));
    }
    if CATALOG.iter().any(|def| def.id == action.id || def.aliases.contains(&action.id)) {
        return Some(format!("{} is a standard action", action.id));
    }
    None
}

fn build(action: &'static CustomAction) -> ActionDef {
    let (shortest, longest) = CONFIRM_TIMEOUT_RANGE_MS;
    ActionDef {
        id: action.id,
        aliases: &[],
        label: action.label,
        tier: floor(action),
        // The device must also name this very action among its capabilities.
        targets: Targets::Capable(action.class, CapKind::Custom),
        params: action.params,
        description: action.description,
        consequences: action.consequences,
        idempotent: action.idempotent,
        confirm_timeout_ms: action.confirm_timeout_ms.clamp(shortest, longest),
        executor: Executor::Device,
        expect: Some(&action.expect),
    }
}

/// Adds each plugin's custom actions to the catalog, with the floors applied.
/// Append-only and idempotent: an action already there is left as it is, and an
/// id that is not the plugin's own or is taken is left out and logged.
pub fn register_plugins(plugins: &[&'static dyn Plugin]) {
    let mut registered = REGISTERED.write().unwrap_or_else(|poisoned| poisoned.into_inner());
    for plugin in plugins {
        let manifest = plugin.manifest();
        for action in manifest.actions {
            if let Some(known) = registered.iter().find(|known| known.def.id == action.id) {
                if known.plugin != manifest.id {
                    tracing::warn!(plugin = manifest.id, action = action.id, "another plugin declared this action");
                }
                continue;
            }
            if let Some(problem) = problem(manifest.id, action) {
                tracing::warn!(plugin = manifest.id, action = action.id, %problem, "a custom action was left out");
                continue;
            }
            let def: &'static ActionDef = Box::leak(Box::new(build(action)));
            if def.tier != action.tier {
                tracing::info!(
                    plugin = manifest.id,
                    action = action.id,
                    requested = action.tier.name(),
                    applied = def.tier.name(),
                    "a custom action's tier was raised to the hub's floor"
                );
            }
            registered.push(Registered { plugin: manifest.id, requested: action.tier, def });
        }
    }
}

/// Every custom action in the catalog, in the order it was added.
pub fn registered() -> Vec<Registered> {
    REGISTERED.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}

/// The custom action with this id.
pub(super) fn find(id: &str) -> Option<&'static ActionDef> {
    let registered = REGISTERED.read().unwrap_or_else(|poisoned| poisoned.into_inner());
    registered.iter().find(|known| known.def.id == id).map(|known| known.def)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use serde_json::{json, Value};
    use swifty_plugin_api::{
        Capability, ConfigProblem, DeviceDecl, Driver, Expect, InstanceView, LiveContext, Manifest, Op, ParamDef,
        ParamKind, PluginError, Reach, Readiness, Scalar, API_VERSION,
    };

    use super::*;
    use crate::catalog::{all, find, plan, Context};
    use crate::policy::{self, Role};
    use crate::runtime::index::{DeviceIndex, IndexedDevice};

    const fn action(id: &'static str, class: DeviceClass, tier: Tier, idempotent: bool) -> CustomAction {
        CustomAction {
            id,
            label: "Test action",
            description: "Does one test thing.",
            consequences: &["Nothing that matters."],
            class,
            tier,
            params: &[],
            idempotent,
            confirm_timeout_ms: 3_000,
            expect: Expect::FieldIs { field: "done", value: true },
        }
    }

    const LEVEL: &[ParamDef] = &[
        ParamDef { name: "level", kind: ParamKind::Int { min: 0, max: 10 }, required: true, help: "A level." },
        ParamDef { name: "quiet", kind: ParamKind::Bool, required: false, help: "Whether to be quiet." },
        ParamDef { name: "mode", kind: ParamKind::Enum(&["slow", "fast"]), required: false, help: "How." },
    ];

    const fn manifest(id: &'static str, actions: &'static [CustomAction]) -> Manifest {
        Manifest {
            id,
            name: "Test",
            version: "0.1.0",
            api: API_VERSION,
            summary: "A plugin that only exists in this test.",
            reach: Reach::Local,
            network: &[],
            classes: &[DeviceClass::Other],
            config_version: 1,
            config: &[],
            secrets: &[],
            actions,
            vocabulary: &[],
            nouns: &[],
            multi_instance: false,
        }
    }

    static FLOORS: Manifest = manifest(
        "floors",
        &[
            action("floors.read", DeviceClass::Other, Tier::Read, true),
            action("floors.safe", DeviceClass::Other, Tier::SafeDirection, true),
            action("floors.again", DeviceClass::Other, Tier::Comfort, false),
            action("floors.vm", DeviceClass::Vm, Tier::Comfort, true),
            action("floors.host", DeviceClass::Host, Tier::Consequential, true),
            action("floors.router", DeviceClass::Router, Tier::Comfort, true),
            action("floors.kept", DeviceClass::Light, Tier::Critical, true),
            action("floors.light", DeviceClass::Light, Tier::Consequential, true),
            action("floors.ok", DeviceClass::Light, Tier::Comfort, true),
            CustomAction { params: LEVEL, ..action("floors.level", DeviceClass::Other, Tier::Comfort, true) },
            CustomAction { confirm_timeout_ms: 0, ..action("floors.instant", DeviceClass::Other, Tier::Comfort, true) },
            // Not this plugin's, not a verb, or a standard action: each is left out.
            action("other.thing", DeviceClass::Other, Tier::Comfort, true),
            action("floors.Loud", DeviceClass::Other, Tier::Comfort, true),
            action("floors.", DeviceClass::Other, Tier::Comfort, true),
            action("floorsx.thing", DeviceClass::Other, Tier::Comfort, true),
        ],
    );
    static STANDARD: Manifest = manifest("light", &[action("light.power", DeviceClass::Light, Tier::Critical, true)]);
    /// A second plugin with the same id, which cannot change what the first declared.
    static SHADOW: Manifest = manifest("floors", &[action("floors.ok", DeviceClass::Light, Tier::Critical, true)]);

    pub(crate) struct TestPlugin(pub(crate) &'static Manifest);

    impl Plugin for TestPlugin {
        fn manifest(&self) -> &'static Manifest {
            self.0
        }
        fn validate(&self, _config: &Value) -> Result<(), ConfigProblem> {
            Ok(())
        }
        fn readiness(&self, _instance: &InstanceView<'_>) -> Readiness {
            Readiness::Ready
        }
        fn devices(&self, _instance: &InstanceView<'_>) -> Vec<DeviceDecl> {
            Vec::new()
        }
        fn refusal(&self, _instance: &InstanceView<'_>, _device: &str, _op: &Op) -> Option<String> {
            None
        }
        fn connect(&self, _ctx: LiveContext) -> Result<Arc<dyn Driver>, PluginError> {
            Err(PluginError("this test plugin has no driver".to_string()))
        }
    }

    static FLOORS_PLUGIN: TestPlugin = TestPlugin(&FLOORS);
    static STANDARD_PLUGIN: TestPlugin = TestPlugin(&STANDARD);
    static SHADOW_PLUGIN: TestPlugin = TestPlugin(&SHADOW);

    /// Adds the test plugins' actions to this process's catalog.
    pub(crate) fn register() {
        register_plugins(&[&FLOORS_PLUGIN, &STANDARD_PLUGIN]);
    }

    fn of_floors() -> Vec<Registered> {
        registered().into_iter().filter(|known| known.plugin == "floors").collect()
    }

    pub(crate) fn device(
        target: &str,
        plugin: &'static str,
        class: DeviceClass,
        capabilities: Vec<Capability>,
    ) -> IndexedDevice {
        IndexedDevice {
            target: target.to_string(),
            instance: plugin.to_string(),
            plugin,
            local_id: target.to_string(),
            class,
            name: format!("The {target}"),
            capabilities,
            audience: class.default_audience(),
            pending: false,
        }
    }

    pub(crate) fn names(action: &str) -> Vec<Capability> {
        vec![Capability::Custom { action: action.to_string() }]
    }

    #[test]
    fn each_floor_is_applied_and_the_request_is_kept_beside_it() {
        register();
        let tiers: Vec<(&str, Tier, Tier)> =
            of_floors().iter().map(|known| (known.def.id, known.requested, known.def.tier)).collect();
        assert_eq!(
            tiers,
            [
                ("floors.read", Tier::Read, Tier::Comfort),
                ("floors.safe", Tier::SafeDirection, Tier::Comfort),
                ("floors.again", Tier::Comfort, Tier::Consequential),
                ("floors.vm", Tier::Comfort, Tier::Critical),
                ("floors.host", Tier::Consequential, Tier::Critical),
                ("floors.router", Tier::Comfort, Tier::Critical),
                ("floors.kept", Tier::Critical, Tier::Critical),
                ("floors.light", Tier::Consequential, Tier::Consequential),
                ("floors.ok", Tier::Comfort, Tier::Comfort),
                ("floors.level", Tier::Comfort, Tier::Comfort),
                ("floors.instant", Tier::Comfort, Tier::Comfort),
            ]
        );
        for action in FLOORS.actions.iter().filter(|action| find(action.id).is_some()) {
            assert_eq!(find(action.id).unwrap().tier, floor(action), "{}", action.id);
        }
        // Never Read and never SafeDirection, whatever was asked.
        for known in registered() {
            assert!(!matches!(known.def.tier, Tier::Read | Tier::SafeDirection), "{}", known.def.id);
        }
    }

    #[test]
    fn an_entry_is_built_from_the_manifest_once_and_kept() {
        register();
        let level = find("floors.level").unwrap();
        assert!(level.is_custom());
        assert_eq!(level.expect, Some(&Expect::FieldIs { field: "done", value: true }));
        assert_eq!(level.targets, Targets::Capable(DeviceClass::Other, CapKind::Custom));
        assert_eq!((level.executor, level.idempotent, level.label), (Executor::Device, true, "Test action"));
        assert_eq!(level.params.len(), 3);
        assert!(std::ptr::eq(level, find("floors.level").unwrap()), "the same entry every time");
        assert_eq!(find("floors.instant").unwrap().confirm_timeout_ms, CONFIRM_TIMEOUT_RANGE_MS.0);

        // Registering again, or another plugin claiming the same action, changes nothing.
        let before = of_floors().len();
        register_plugins(&[&FLOORS_PLUGIN, &SHADOW_PLUGIN]);
        register();
        assert_eq!(of_floors().len(), before);
        assert_eq!(find("floors.ok").unwrap().tier, Tier::Comfort);

        // The standard table comes first, then the custom actions in the order they came.
        let every: Vec<&str> = all().into_iter().map(|def| def.id).collect();
        let standard: Vec<&str> = CATALOG.iter().map(|def| def.id).collect();
        assert_eq!(every[..CATALOG.len()], standard[..]);
        let custom: Vec<&str> = every[CATALOG.len()..].iter().copied().filter(|id| id.starts_with("floors.")).collect();
        assert_eq!(custom, of_floors().iter().map(|known| known.def.id).collect::<Vec<_>>());
    }

    #[test]
    fn an_action_that_is_not_the_plugins_own_is_left_out() {
        register();
        for id in ["other.thing", "floors.Loud", "floors.", "floorsx.thing"] {
            assert!(find(id).is_none(), "{id}");
        }
        let standard = find("light.power").unwrap();
        assert!(!standard.is_custom(), "a plugin cannot replace a standard action");
        assert_eq!(standard.tier, Tier::Comfort);
        assert!(!registered().iter().any(|known| known.plugin == "light"));
    }

    #[test]
    fn a_custom_action_reaches_only_a_device_of_its_plugin_that_names_it() {
        register();
        let ok = find("floors.ok").unwrap();
        let mut waiting = device("waiting", "floors", DeviceClass::Light, names("floors.ok"));
        waiting.pending = true;
        let index = DeviceIndex::new([
            device("gadget", "floors", DeviceClass::Light, names("floors.ok")),
            device("stranger", "elsewhere", DeviceClass::Light, names("floors.ok")),
            device("plain", "floors", DeviceClass::Light, vec![Capability::Power]),
            device("other-action", "floors", DeviceClass::Light, names("floors.kept")),
            device("socket", "floors", DeviceClass::Plug, names("floors.ok")),
            waiting,
        ]);
        assert_eq!(ok.admitted(&index), ["gadget"]);
        for target in ["stranger", "plain", "other-action", "socket", "waiting", "nothing"] {
            assert!(ok.admits(&index, target).is_err(), "{target}");
        }
        assert!(ok.waits_for_approval(&index, "waiting"));
        assert!(find("light.power").unwrap().admits(&index, "gadget").is_err(), "a light that cannot be switched");

        // Owner-only: the family's list names standard actions only.
        assert!(policy::may_run(&index, Role::Owner, "floors.ok", "gadget"));
        assert!(!policy::may_run(&index, Role::Family, "floors.ok", "gadget"));
        let json = ok.to_json(&ok.admitted(&index));
        assert_eq!((json["target_kind"].clone(), json["requires"].clone()), (json!("capable"), json!("custom")));
    }

    #[test]
    fn a_custom_action_plans_one_operation_with_its_checked_arguments() {
        register();
        let def = find("floors.level").unwrap();
        let index = DeviceIndex::new([device("gadget", "floors", DeviceClass::Other, names("floors.level"))]);
        assert!(def.validate_params(&json!({ "level": 11 })).is_err());
        assert!(def.validate_params(&json!({ "level": 3, "loud": true })).is_err());
        let params = def.validate_params(&json!({ "level": 3, "quiet": true, "mode": "fast" })).unwrap();
        let steps = plan(def, "gadget", &params, &Context { index: &index, lights: &[], scene: None }).unwrap();
        let args = [
            ("level".to_string(), Scalar::Int(3)),
            ("mode".to_string(), Scalar::Text("fast".to_string())),
            ("quiet".to_string(), Scalar::Bool(true)),
        ];
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].target, "gadget");
        assert_eq!(steps[0].op, Op::Custom { action: "floors.level".to_string(), args: args.into_iter().collect() });
    }
}
