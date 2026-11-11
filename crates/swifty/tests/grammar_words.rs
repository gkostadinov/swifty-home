//! The test kit cannot link the hub, so its list of grammar words
//! (`conformance::GRAMMAR_WORDS`) is a copy of what the hub's voice grammar keeps for
//! itself. A word on the copy that the hub would let a plugin use is a word the suite
//! forbids for no reason, and one it forgets is a word a plugin passes with and the hub
//! then drops at start-up. The hub's own check, `voice::is_reserved_phrase`, is the final
//! word; this keeps the copy from drifting away from it.

use swifty_hub::voice::is_reserved_phrase;
use swifty_plugin_testkit::conformance::{is_grammar_word, GRAMMAR_WORDS};

#[test]
fn every_word_the_test_kit_calls_grammar_is_one_the_hub_reserves() {
    let not_reserved: Vec<&str> = GRAMMAR_WORDS.iter().copied().filter(|word| !is_reserved_phrase(word)).collect();
    assert!(
        not_reserved.is_empty(),
        "the test kit forbids these words as a plugin's vocabulary, but the hub would take them: {not_reserved:?}. \
         Take them off GRAMMAR_WORDS (crates/swifty-plugin-testkit/src/conformance/manifest.rs)"
    );
}

#[test]
fn the_words_a_plugin_may_not_use_are_refused_both_by_the_suite_and_by_the_hub() {
    // Words the grammar acts on, wherever the hub keeps them, and one that is a name and nothing more.
    for word in ["off", "on", "stop", "please", "not", "or", "all", "hue", "ac", "plug", "42"] {
        assert!(is_grammar_word(word), "the suite lets a plugin use {word:?}");
        assert!(is_reserved_phrase(word), "the hub lets a plugin use {word:?}");
    }
    for word in ["acme", "blinds", "lumen"] {
        assert!(!is_grammar_word(word) && !is_reserved_phrase(word), "{word:?} is a name any plugin may use");
    }
}
