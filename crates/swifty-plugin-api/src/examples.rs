//! Samples of every variant of the contract's types.
//!
//! The generated reference in `docs/plugins/reference/` shows these values and
//! their JSON (`cargo xtask gen-docs` writes it), and a test here proves that
//! every variant of every type has at least one sample, so a variant added to
//! the contract cannot be left out of the reference.
//!
//! The values are plain and made up: addresses are `192.0.2.x`, domains are
//! `example.org`, and no credential is real.

/// Samples of a type, at least one of each variant.
///
/// ```
/// use swifty_plugin_api::{Examples, Op};
///
/// for op in Op::examples() {
///     let json = serde_json::to_value(&op).unwrap();
///     assert!(json["op"].is_string());
/// }
/// ```
pub trait Examples: Sized {
    /// At least one value of each variant, in the order the type declares them.
    fn examples() -> Vec<Self>;
}

/// Text, for the samples.
pub(crate) fn text(value: &str) -> String {
    value.to_string()
}

#[cfg(test)]
mod tests {
    //! Every variant has a sample. For a type that deserialises, serde's own
    //! list of variant names (what it says about a name it does not know) is
    //! the list to cover, so a new variant fails here until it has a sample.
    //! For a type that only serialises, an exhaustive `match` names each
    //! variant, so a new variant does not compile until it is listed here.

    use std::collections::BTreeSet;

    use serde::de::DeserializeOwned;
    use serde::Serialize;
    use serde_json::{json, Value};

    use super::Examples;
    use crate::*;

    /// The variant names serde knows for `T`, from its answer to one it does not.
    fn variants_of<T: DeserializeOwned>(probe: Value) -> BTreeSet<String> {
        let error = serde_json::from_value::<T>(probe).err().expect("the probe names no variant").to_string();
        let (_, expected) = error.split_once("expected").unwrap_or_else(|| panic!("no list of variants in: {error}"));
        expected.split('`').skip(1).step_by(2).map(str::to_string).collect()
    }

    /// The variant name of each sample, read from its JSON.
    fn tags_of<T: Examples + Serialize>(tag: impl Fn(&Value) -> Option<String>) -> BTreeSet<String> {
        T::examples()
            .iter()
            .map(|sample| {
                let json = serde_json::to_value(sample).unwrap();
                tag(&json).unwrap_or_else(|| panic!("no variant name in {json}"))
            })
            .collect()
    }

    fn field(name: &'static str) -> impl Fn(&Value) -> Option<String> {
        move |json| json.get(name).and_then(Value::as_str).map(str::to_string)
    }

    fn plain(json: &Value) -> Option<String> {
        json.as_str().map(str::to_string)
    }

    fn outer_key(json: &Value) -> Option<String> {
        match json {
            Value::String(name) => Some(name.clone()),
            Value::Object(map) => map.keys().next().cloned(),
            _ => None,
        }
    }

    fn covers<T: Examples + Serialize + DeserializeOwned>(probe: Value, tag: impl Fn(&Value) -> Option<String>) {
        let all = variants_of::<T>(probe);
        assert!(all.len() > 1, "{all:?}");
        let sampled = tags_of::<T>(tag);
        let missing: Vec<&String> = all.difference(&sampled).collect();
        assert!(missing.is_empty(), "{} has no sample of {missing:?}", std::any::type_name::<T>());
    }

    const UNKNOWN: &str = "no_such_variant";

    #[test]
    fn every_variant_that_deserialises_has_a_sample() {
        covers::<Op>(json!({ "op": UNKNOWN }), field("op"));
        covers::<State>(json!({ "kind": UNKNOWN }), field("kind"));
        covers::<Capability>(json!({ "cap": UNKNOWN }), field("cap"));
        covers::<DeviceClass>(json!(UNKNOWN), plain);
        covers::<SetupStep>(json!({ "step": UNKNOWN }), field("step"));
        covers::<SetupInput>(json!({ "input": UNKNOWN }), field("input"));
        covers::<FieldType>(json!({ "type": UNKNOWN }), field("type"));
        covers::<Readiness>(json!({ "status": UNKNOWN }), field("status"));
        covers::<DeviceError>(json!(UNKNOWN), outer_key);
        let statuses: BTreeSet<String> = Health::examples()
            .iter()
            .map(|health| serde_json::to_value(health.status).unwrap())
            .filter_map(|s| plain(&s))
            .collect();
        assert_eq!(statuses, variants_of::<HealthStatus>(json!(UNKNOWN)));
    }

    #[test]
    fn every_variant_that_only_serialises_has_a_sample() {
        fn names<T>(samples: Vec<T>, name: fn(&T) -> &'static str, all: &[&str]) {
            let sampled: BTreeSet<&str> = samples.iter().map(name).collect();
            let all: BTreeSet<&str> = all.iter().copied().collect();
            assert_eq!(sampled, all, "{}", std::any::type_name::<T>());
        }
        names(
            Reach::examples(),
            |reach| match reach {
                Reach::Local => "local",
                Reach::Cloud { .. } => "cloud",
            },
            &["local", "cloud"],
        );
        names(
            NetUse::examples(),
            |net| match net {
                NetUse::Http { .. } => "http",
                NetUse::Https { .. } => "https",
                NetUse::Ws { .. } => "ws",
                NetUse::Tcp { .. } => "tcp",
                NetUse::Udp { .. } => "udp",
                NetUse::UdpBroadcast { .. } => "udp_broadcast",
                NetUse::WakeOnLan => "wake_on_lan",
                NetUse::CloudHost { .. } => "cloud_host",
            },
            &["http", "https", "ws", "tcp", "udp", "udp_broadcast", "wake_on_lan", "cloud_host"],
        );
        names(
            FieldKind::examples(),
            |kind| match kind {
                FieldKind::Text { .. } => "text",
                FieldKind::Host => "host",
                FieldKind::Url { .. } => "url",
                FieldKind::HostPort => "host_port",
                FieldKind::Port => "port",
                FieldKind::Int { .. } => "int",
                FieldKind::Bool => "bool",
                FieldKind::Choice { .. } => "choice",
                FieldKind::Mac => "mac",
                FieldKind::Rows { .. } => "rows",
            },
            &["text", "host", "url", "host_port", "port", "int", "bool", "choice", "mac", "rows"],
        );
        names(
            Expect::examples(),
            |expect| match expect {
                Expect::Unreadable => "unreadable",
                Expect::FieldEqualsArg { .. } => "field_equals_arg",
                Expect::FieldIs { .. } => "field_is",
                Expect::CounterIncreased { .. } => "counter_increased",
            },
            &["unreadable", "field_equals_arg", "field_is", "counter_increased"],
        );
        names(
            Tier::examples(),
            |tier| match tier {
                Tier::Read => "read",
                Tier::Comfort => "comfort",
                Tier::Consequential => "consequential",
                Tier::Critical => "critical",
                Tier::SafeDirection => "safe_direction",
            },
            &["read", "comfort", "consequential", "critical", "safe_direction"],
        );
        names(
            ParamKind::examples(),
            |kind| match kind {
                ParamKind::Bool => "bool",
                ParamKind::Int { .. } => "int",
                ParamKind::Enum(_) => "enum",
                ParamKind::Label { .. } => "label",
            },
            &["bool", "int", "enum", "label"],
        );
        // The rest only have one shape each, and a sample of it.
        assert!(!PluginError::examples().is_empty());
        assert!(!ConfigProblem::examples().is_empty());
    }

    #[test]
    fn the_samples_read_back_and_hold_no_site_data() {
        fn back<T: Examples + Serialize + DeserializeOwned>() {
            for sample in T::examples() {
                let json = serde_json::to_value(&sample).unwrap();
                let again: T = serde_json::from_value(json.clone()).unwrap_or_else(|e| panic!("{json}: {e}"));
                assert_eq!(serde_json::to_value(&again).unwrap(), json);
            }
        }
        back::<Op>();
        back::<State>();
        back::<Capability>();
        back::<DeviceClass>();
        back::<SetupStep>();
        back::<SetupInput>();
        back::<FieldType>();
        back::<Readiness>();
        back::<Health>();
        back::<DeviceError>();

        fn all_json<T: Examples + Serialize>() -> Vec<String> {
            T::examples().iter().map(|sample| serde_json::to_string(sample).unwrap()).collect()
        }
        let every = [
            all_json::<Op>(),
            all_json::<State>(),
            all_json::<SetupStep>(),
            all_json::<SetupInput>(),
            all_json::<NetUse>(),
            all_json::<FieldKind>(),
            all_json::<DeviceError>(),
        ]
        .concat();
        for json in every {
            // Every address in a URL is a documentation address.
            for (at, _) in json.match_indices("://") {
                let host: String = json[at + 3..].chars().take_while(|c| !matches!(c, '"' | '/' | ':')).collect();
                assert!(host.starts_with("192.0.2.") || host.ends_with("example.org"), "{host} in {json}");
            }
        }
    }

    #[test]
    fn a_finished_setup_sample_keeps_its_secret_out_of_the_json() {
        let done = SetupStep::examples().into_iter().find(|step| matches!(step, SetupStep::Done(_))).unwrap();
        let SetupStep::Done(result) = &done else { unreachable!() };
        assert!(!result.secrets.is_empty(), "the sample shows that secrets are skipped");
        let json = serde_json::to_string(&done).unwrap();
        for (_, value) in &result.secrets {
            assert!(!json.contains(value.expose()));
        }
    }
}
