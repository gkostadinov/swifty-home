//! The words voice and Swifty know for one home: its rooms, the names of its devices,
//! the class nouns, and what the plugins say the recogniser mishears.
//!
//! A [`Lexicon`] is built for each request from the device index, the rooms and the
//! plugins' words, the way the room index is. The grammar in `voice.rs` names no
//! device, product or house: every proper noun arrives here as data. A device answers
//! to the words of its name and of its target, so renaming it changes what it is
//! called at once, and its old name stops working.
//!
//! Plugins contribute words only. A noun or a mishearing that would change what a
//! grammar word does is dropped, and listed in [`Lexicon::dropped`] with the reason.

use std::collections::{BTreeMap, HashSet};
use std::sync::RwLock;

use serde::Serialize;
use swifty_plugin_api::{CapKind, DeviceClass, Manifest};

use super::{is_grammar_word, is_reserved_phrase, normalize_with, SpokenRoom, BUILT_IN_NOUNS};
use crate::runtime::index::DeviceIndex;

/// What one plugin adds to the words: mishearings and the brand words that name a class.
/// The same shape as `Manifest::vocabulary` and `Manifest::nouns`.
#[derive(Debug, Clone, Copy)]
pub struct PluginWords {
    pub plugin: &'static str,
    /// What the recogniser writes, and what was meant: ("whiz", "wiz").
    pub vocabulary: &'static [(&'static str, &'static str)],
    /// Brand words that name a class: ("sonos", Speaker).
    pub nouns: &'static [(&'static str, DeviceClass)],
}

impl From<&'static Manifest> for PluginWords {
    fn from(manifest: &'static Manifest) -> Self {
        Self { plugin: manifest.id, vocabulary: manifest.vocabulary, nouns: manifest.nouns }
    }
}

/// The plugins' words this process knows, from the manifests of the plugins it was
/// built with. Registered once at startup and never taken back.
static REGISTERED: RwLock<Vec<PluginWords>> = RwLock::new(Vec::new());

/// The plainest lexicon there is with the registered words, and how many plugins'
/// words it was built with. Built again only when more are registered.
static PLAIN: RwLock<Option<(usize, &'static Lexicon)>> = RwLock::new(None);

/// Adds these plugins' words to what the plain lexicon knows, once each, and gives
/// the words of theirs the grammar would not take, for the plugin page.
pub fn register_plugin_words(plugins: &[PluginWords]) -> Vec<DroppedWord> {
    {
        let mut registered = REGISTERED.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        for plugin in plugins {
            if !registered.iter().any(|known| known.plugin == plugin.plugin) {
                registered.push(*plugin);
            }
        }
    }
    let dropped = Lexicon::build(&DeviceIndex::default(), Vec::new(), plugins).dropped;
    for word in &dropped {
        tracing::info!(plugin = %word.plugin, word = %word.word, why = word.why, "a plugin's word is not used");
    }
    dropped
}

/// Every plugin's words registered so far.
pub fn registered_words() -> Vec<PluginWords> {
    REGISTERED.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}

/// The words the first-party plugins declare, as their manifests did when this
/// was written. The hub's own tests build lexicons with it; the running hub takes
/// the words from the compiled-in manifests.
#[cfg(test)]
pub const LEGACY_WORDS: &[PluginWords] = &[
    PluginWords { plugin: "wiz", vocabulary: &[("whiz", "wiz")], nouns: &[("wiz", DeviceClass::Light)] },
    // Dropped: "hue" is the grammar's word for a colour. See `the_hue_noun_is_dropped`.
    PluginWords { plugin: "hue", vocabulary: &[], nouns: &[("hue", DeviceClass::Light)] },
    PluginWords {
        plugin: "sonos",
        vocabulary: &[],
        nouns: &[("sonos", DeviceClass::Speaker), ("arc", DeviceClass::Speaker)],
    },
    PluginWords { plugin: "webos", vocabulary: &[("lgtv", "lg tv")], nouns: &[("lg", DeviceClass::Tv)] },
    PluginWords { plugin: "tuya", vocabulary: &[], nouns: &[("tuya", DeviceClass::Plug)] },
    PluginWords { plugin: "toshiba", vocabulary: &[("air con", "aircon")], nouns: &[("toshiba", DeviceClass::Aircon)] },
    PluginWords {
        plugin: "petkit",
        vocabulary: &[("pet feeder", "petkit feeder")],
        nouns: &[("petkit", DeviceClass::Feeder)],
    },
    PluginWords {
        plugin: "moonraker",
        vocabulary: &[("clipper", "klipper")],
        nouns: &[("klipper", DeviceClass::Printer), ("moonraker", DeviceClass::Printer)],
    },
    PluginWords { plugin: "proxmox", vocabulary: &[], nouns: &[("proxmox", DeviceClass::Host)] },
    PluginWords { plugin: "plex", vocabulary: &[], nouns: &[("plex", DeviceClass::Service)] },
    PluginWords { plugin: "camera", vocabulary: &[], nouns: &[] },
    PluginWords {
        plugin: "glimdock",
        vocabulary: &[("glim dock", "glimdock")],
        nouns: &[("glimdock", DeviceClass::Telemetry)],
    },
];

/// A word a plugin offered that was not taken, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DroppedWord {
    pub plugin: String,
    pub word: String,
    pub why: &'static str,
}

/// One device as voice knows it.
#[derive(Debug, Clone, PartialEq)]
pub struct SpokenDevice {
    pub target: String,
    /// The name people gave it, for labels.
    pub name: String,
    pub class: DeviceClass,
    capabilities: Vec<CapKind>,
    /// Its name and its target, each as spoken words.
    forms: Vec<Vec<String>>,
    /// The words that are its own: no grammar word, no class noun, no bare number.
    own: Vec<String>,
}

impl SpokenDevice {
    pub fn has(&self, cap: CapKind) -> bool {
        self.capabilities.contains(&cap)
    }

    /// Whether it is called `<noun> <n>`, or `<noun><n>` as one word, where the noun is
    /// one of `nouns`: "bulb 2" for a light named "Bulb 2" or with the target `bulb2`.
    pub fn numbered(&self, nouns: &[&str], n: i64) -> bool {
        let is_n = |token: &str| token.parse::<i64>().ok() == Some(n);
        self.forms.iter().any(|form| {
            form.windows(2).any(|pair| nouns.contains(&pair[0].as_str()) && is_n(&pair[1]))
                || form.iter().any(|token| {
                    nouns.iter().any(|noun| token.strip_prefix(noun).is_some_and(|rest| !rest.is_empty() && is_n(rest)))
                })
        })
    }

    /// Its own words: what tells it apart when spoken.
    pub fn own_words(&self) -> &[String] {
        &self.own
    }
}

/// Everything voice and Swifty know by name, for one request.
#[derive(Debug, Clone, Default)]
pub struct Lexicon {
    pub rooms: Vec<SpokenRoom>,
    /// Device names and targets as spoken words, each to its target and class.
    pub names: Vec<(Vec<String>, String, DeviceClass)>,
    /// Class nouns: the grammar's own ("light", "tv", "printer") and the plugins' ("sonos", "klipper").
    pub nouns: BTreeMap<String, DeviceClass>,
    /// Mishearings from the plugins' vocabulary, as words.
    pub aliases: Vec<(Vec<String>, Vec<String>)>,
    /// What the plugins offered and was not taken.
    pub dropped: Vec<DroppedWord>,
    devices: Vec<SpokenDevice>,
    /// Words that run-together speech is joined back into: nouns, device and room words.
    joinable: HashSet<String>,
}

/// A device's target as spoken: `vm100` is "vm 100" and `hue-1` is "hue 1".
fn target_words(target: &str) -> Vec<String> {
    let spaced: String = target.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' }).collect();
    let mut out = Vec::new();
    for token in spaced.to_lowercase().split_whitespace() {
        let digits = token.len() - token.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        let (head, tail) = token.split_at(token.len() - digits);
        if head.is_empty() || tail.is_empty() {
            out.push(token.to_string());
        } else {
            out.push(head.to_string());
            out.push(tail.to_string());
        }
    }
    out
}

fn is_number(word: &str) -> bool {
    !word.is_empty() && word.chars().all(|c| c.is_ascii_digit())
}

fn words_of(text: &str) -> Vec<String> {
    let cleaned: String = text.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
    cleaned.split_whitespace().map(str::to_string).collect()
}

impl Lexicon {
    /// The words for a home: every active device in the index, the rooms, and the
    /// plugins' words checked against the grammar.
    pub fn build(index: &DeviceIndex, rooms: Vec<SpokenRoom>, plugins: &[PluginWords]) -> Self {
        let mut lexicon = Lexicon::default();
        for (word, class) in BUILT_IN_NOUNS {
            lexicon.nouns.insert(word.to_string(), *class);
        }
        for plugin in plugins {
            for (word, class) in plugin.nouns {
                lexicon.take_noun(plugin.plugin, word, *class);
            }
        }
        for plugin in plugins {
            for (heard, meant) in plugin.vocabulary {
                lexicon.take_alias(plugin.plugin, heard, meant);
            }
        }

        let nouns = lexicon.nouns.clone();
        for device in index.active() {
            let name = normalize_with(&device.name, &lexicon.aliases, None);
            let forms =
                vec![name.split_whitespace().map(str::to_string).collect::<Vec<_>>(), target_words(&device.target)];
            let mut own: Vec<String> = Vec::new();
            for word in forms.iter().flatten() {
                let distinct =
                    word.len() >= 2 && !is_number(word) && !is_grammar_word(word) && !nouns.contains_key(word);
                if distinct && !own.contains(word) {
                    own.push(word.clone());
                }
            }
            for form in &forms {
                lexicon.names.push((form.clone(), device.target.clone(), device.class));
            }
            lexicon.devices.push(SpokenDevice {
                target: device.target.clone(),
                name: device.name.clone(),
                class: device.class,
                capabilities: device.capabilities.iter().map(|capability| capability.kind()).collect(),
                forms,
                own,
            });
        }

        lexicon.joinable = lexicon.nouns.keys().cloned().collect();
        let own_words: Vec<String> = lexicon.devices.iter().flat_map(|device| device.own.iter().cloned()).collect();
        lexicon.joinable.extend(own_words);
        for room in &rooms {
            let words = std::iter::once(&room.key).chain(room.scenes.iter().map(|(_, key)| key));
            let words: Vec<String> = words.flat_map(|key| key.split_whitespace().map(str::to_string)).collect();
            lexicon.joinable.extend(words.into_iter().filter(|word| !is_number(word) && !is_grammar_word(word)));
        }
        // A room's name was stored in the grammar's normal form. Spoken words also pass
        // through the mishearings and joins here, so the room's words must too.
        lexicon.rooms = rooms
            .into_iter()
            .map(|room| SpokenRoom {
                key: lexicon.normalize(&room.key),
                scenes: room.scenes.into_iter().map(|(name, key)| (name, lexicon.normalize(&key))).collect(),
                ..room
            })
            .collect();
        lexicon
    }

    /// The grammar and the registered plugins' words, and no devices or rooms.
    pub fn plain() -> &'static Lexicon {
        let words = registered_words();
        if let Some((count, lexicon)) = *PLAIN.read().unwrap_or_else(|poisoned| poisoned.into_inner()) {
            if count == words.len() {
                return lexicon;
            }
        }
        // Leaked once for each time more plugins are registered: a few times a process.
        let lexicon: &'static Lexicon =
            Box::leak(Box::new(Lexicon::build(&DeviceIndex::default(), Vec::new(), &words)));
        *PLAIN.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((words.len(), lexicon));
        lexicon
    }

    fn drop_word(&mut self, plugin: &str, word: &str, why: &'static str) {
        tracing::debug!(plugin, word, why, "a plugin's word was dropped");
        self.dropped.push(DroppedWord { plugin: plugin.to_string(), word: word.to_string(), why });
    }

    fn take_noun(&mut self, plugin: &str, word: &str, class: DeviceClass) {
        let words = words_of(word);
        let [single] = words.as_slice() else {
            return self.drop_word(plugin, word, "a noun is one word");
        };
        if is_reserved_phrase(single) {
            return self.drop_word(plugin, word, "the grammar already uses this word");
        }
        match self.nouns.get(single) {
            Some(known) if *known != class => {
                self.drop_word(plugin, word, "another plugin uses this word for another kind of device")
            }
            Some(_) => {}
            None => {
                self.nouns.insert(single.clone(), class);
            }
        }
    }

    /// A mishearing may only turn words that mean nothing to the grammar into class
    /// nouns, so it can never add, hide or change a verb, a negation or a number.
    fn take_alias(&mut self, plugin: &str, heard: &str, meant: &str) {
        let from = words_of(heard);
        let to = words_of(meant);
        if from.is_empty() || to.is_empty() || from == to {
            return self.drop_word(plugin, heard, "a mishearing needs words on both sides");
        }
        let rewrites_grammar = |word: &String| {
            is_number(word) || (is_grammar_word(word) && !BUILT_IN_NOUNS.iter().any(|(noun, _)| noun == word))
        };
        if from.iter().any(rewrites_grammar) {
            return self.drop_word(plugin, heard, "it would rewrite a word the grammar uses");
        }
        if !to.iter().all(|word| self.nouns.contains_key(word)) {
            return self.drop_word(plugin, heard, "a mishearing can only be heard as nouns");
        }
        self.aliases.push((from, to));
    }

    /// Lower-case, no punctuation, mishearings and number words resolved, and words that
    /// speech split apart ("file store") joined back when together they name something.
    pub fn normalize(&self, raw: &str) -> String {
        normalize_with(raw, &self.aliases, Some(&self.joinable))
    }

    /// Every device, in target order.
    pub fn devices(&self) -> &[SpokenDevice] {
        &self.devices
    }

    pub fn device(&self, target: &str) -> Option<&SpokenDevice> {
        self.devices.iter().find(|device| device.target == target)
    }

    /// Every device of a class, in target order.
    pub fn of_class(&self, class: DeviceClass) -> impl Iterator<Item = &SpokenDevice> {
        self.devices.iter().filter(move |device| device.class == class)
    }

    /// The machine a shortened name stands for: "fil" or "store" for one called
    /// "filestore", when it fits that one machine and nothing else.
    fn shortened(&self, word: &str) -> Option<&SpokenDevice> {
        if word.len() < 3 || is_number(word) || is_grammar_word(word) || self.joinable.contains(word) {
            return None;
        }
        let fits = |own: &String| own.len() > word.len() && (own.starts_with(word) || own.ends_with(word));
        let mut found = self.of_class(DeviceClass::Vm).filter(|device| device.own.iter().any(fits));
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }

    /// The devices this word is one of the own words of, or the one machine it shortens.
    pub fn callees(&self, word: &str) -> Vec<&SpokenDevice> {
        let exact: Vec<&SpokenDevice> =
            self.devices.iter().filter(|device| device.own.iter().any(|w| w == word)).collect();
        if exact.is_empty() {
            self.shortened(word).into_iter().collect()
        } else {
            exact
        }
    }

    /// Whether this word calls this device.
    pub fn calls(&self, word: &str, device: &SpokenDevice) -> bool {
        device.own.iter().any(|w| w == word) || self.shortened(word).is_some_and(|found| found.target == device.target)
    }

    /// The kinds of device a word can name: by being a noun, or a device's own word.
    pub fn classes_of(&self, word: &str) -> Vec<DeviceClass> {
        let mut classes: Vec<DeviceClass> = self.nouns.get(word).copied().into_iter().collect();
        for device in self.callees(word) {
            if !classes.contains(&device.class) {
                classes.push(device.class);
            }
        }
        classes
    }

    /// Whether a word names something here.
    pub fn knows(&self, word: &str) -> bool {
        !self.classes_of(word).is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::index::{legacy_seeded, IndexedDevice};
    use swifty_plugin_api::{Audience, Capability};

    fn device(target: &str, name: &str, class: DeviceClass) -> IndexedDevice {
        IndexedDevice {
            target: target.to_string(),
            instance: "mock".to_string(),
            plugin: "mock",
            local_id: target.to_string(),
            class,
            name: name.to_string(),
            capabilities: vec![Capability::Power],
            audience: Audience::Owner,
            pending: false,
        }
    }

    #[test]
    fn a_device_answers_to_its_name_and_its_target() {
        let index = DeviceIndex::new([
            device("vm100", "File Store · VM 100", DeviceClass::Vm),
            device("bulb2", "Desk lamp", DeviceClass::Light),
        ]);
        let lexicon = Lexicon::build(&index, Vec::new(), &[]);
        let store = lexicon.device("vm100").unwrap();
        assert_eq!(store.own_words(), ["file", "store"]);
        assert!(store.numbered(&["vm"], 100));
        assert!(!store.numbered(&["vm"], 10));
        let lamp = lexicon.device("bulb2").unwrap();
        assert_eq!(lamp.own_words(), ["desk"], "lamp is a class noun, bulb and 2 are no names of their own");
        assert!(lamp.numbered(&["bulb", "light"], 2), "the target says bulb 2");
        assert!(lexicon.names.contains(&(
            vec!["file".into(), "store".into(), "vm".into(), "100".into()],
            "vm100".into(),
            DeviceClass::Vm
        )));
        assert_eq!(lexicon.classes_of("desk"), [DeviceClass::Light]);
        assert_eq!(lexicon.classes_of("lamp"), [DeviceClass::Light]);
        assert!(lexicon.classes_of("kettle").is_empty());
    }

    #[test]
    fn split_words_are_joined_only_into_something_that_is_here() {
        let index = DeviceIndex::new([device("vm101", "Filestore", DeviceClass::Vm)]);
        let lexicon = Lexicon::build(&index, Vec::new(), &[]);
        assert_eq!(lexicon.normalize("stop the file store"), "stop the filestore");
        assert_eq!(lexicon.normalize("stop the tea pot"), "stop the tea pot");
        // A grammar word is never swallowed into a name.
        let index = DeviceIndex::new([device("vm7", "Nothing", DeviceClass::Vm)]);
        let lexicon = Lexicon::build(&index, Vec::new(), &[]);
        assert_eq!(lexicon.normalize("no thing"), "no thing");
    }

    #[test]
    fn a_machine_answers_to_a_shortened_name_only_when_it_is_the_only_one() {
        let index = DeviceIndex::new([
            device("vm1", "filestore", DeviceClass::Vm),
            device("vm2", "gameserver", DeviceClass::Vm),
            device("vm3", "gamebox", DeviceClass::Vm),
        ]);
        let lexicon = Lexicon::build(&index, Vec::new(), &[]);
        assert_eq!(lexicon.callees("store").iter().map(|d| d.target.as_str()).collect::<Vec<_>>(), ["vm1"]);
        assert_eq!(lexicon.callees("fil").len(), 1);
        assert!(lexicon.callees("game").is_empty(), "two machines start with it");
        assert!(lexicon.callees("off").is_empty(), "a grammar word is never a shortened name");
    }

    #[test]
    fn a_plugin_noun_that_is_a_grammar_word_is_dropped() {
        const WORDS: &[PluginWords] = &[PluginWords {
            plugin: "acme",
            vocabulary: &[
                ("zap", "stop"),
                ("halt", "gizmo"),
                ("not the", "gizmo"),
                ("giz mo", "gizmo"),
                ("acme box", "acme tv"),
            ],
            nouns: &[
                ("stop", DeviceClass::Light),
                ("off", DeviceClass::Plug),
                ("tv", DeviceClass::Speaker),
                ("5", DeviceClass::Light),
                ("two words", DeviceClass::Light),
                ("gizmo", DeviceClass::Light),
                ("acme", DeviceClass::Tv),
            ],
        }];
        let lexicon = Lexicon::build(&DeviceIndex::default(), Vec::new(), WORDS);
        let dropped: Vec<&str> = lexicon.dropped.iter().map(|d| d.word.as_str()).collect();
        assert_eq!(dropped, ["stop", "off", "tv", "5", "two words", "zap", "halt", "not the"]);
        assert_eq!(lexicon.nouns.get("gizmo"), Some(&DeviceClass::Light));
        assert_eq!(lexicon.nouns.get("tv"), Some(&DeviceClass::Tv), "the grammar's noun keeps its class");
        assert!(!lexicon.nouns.contains_key("stop"));
        assert_eq!(
            lexicon.aliases,
            [
                (vec!["giz".to_string(), "mo".to_string()], vec!["gizmo".to_string()]),
                (vec!["acme".into(), "box".into()], vec!["acme".into(), "tv".into()])
            ]
        );
        assert_eq!(lexicon.normalize("zap the gizmo"), "zap the gizmo", "no plugin can add a verb");
        assert_eq!(lexicon.normalize("halt the gizmo"), "halt the gizmo", "nor hide one");
        assert!(lexicon.dropped.iter().all(|d| d.plugin == "acme" && !d.why.is_empty()));
    }

    #[test]
    fn two_plugins_cannot_give_one_noun_two_meanings() {
        const WORDS: &[PluginWords] = &[
            PluginWords { plugin: "one", vocabulary: &[], nouns: &[("gizmo", DeviceClass::Light)] },
            PluginWords {
                plugin: "two",
                vocabulary: &[],
                nouns: &[("gizmo", DeviceClass::Plug), ("gizmo", DeviceClass::Light)],
            },
        ];
        let lexicon = Lexicon::build(&DeviceIndex::default(), Vec::new(), WORDS);
        assert_eq!(lexicon.nouns.get("gizmo"), Some(&DeviceClass::Light));
        assert_eq!(lexicon.dropped.len(), 1);
        assert_eq!(lexicon.dropped[0].plugin, "two");
    }

    #[test]
    fn the_hue_noun_is_dropped_because_hue_is_a_colour_word() {
        let lexicon = Lexicon::build(&legacy_seeded(), Vec::new(), LEGACY_WORDS);
        assert_eq!(
            lexicon.dropped,
            [DroppedWord { plugin: "hue".into(), word: "hue".into(), why: "the grammar already uses this word" }]
        );
        for (word, class) in [
            ("wiz", DeviceClass::Light),
            ("sonos", DeviceClass::Speaker),
            ("arc", DeviceClass::Speaker),
            ("lg", DeviceClass::Tv),
            ("klipper", DeviceClass::Printer),
            ("petkit", DeviceClass::Feeder),
            ("proxmox", DeviceClass::Host),
        ] {
            assert_eq!(lexicon.nouns.get(word), Some(&class), "{word}");
        }
        assert_eq!(lexicon.aliases.len(), 6, "every mishearing of today's plugins is kept");
    }

    #[test]
    fn room_names_take_the_same_words_as_speech() {
        let index = DeviceIndex::new([device("vm1", "Filestore", DeviceClass::Vm)]);
        let room = SpokenRoom { id: "r".into(), name: "Whiz corner".into(), key: "whiz corner".into(), scenes: vec![] };
        let lexicon = Lexicon::build(&index, vec![room], LEGACY_WORDS);
        assert_eq!(lexicon.rooms[0].key, "wiz corner");
        assert_eq!(lexicon.normalize("Whiz Corner"), "wiz corner");
    }
}
