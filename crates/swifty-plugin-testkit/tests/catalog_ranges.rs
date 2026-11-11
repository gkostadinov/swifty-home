//! The conformance corpus (`conformance::corpus`) copies the ranges and word
//! lists of the hub's catalog, because the test kit may not link the hub. This
//! test reads the catalog's source and fails as soon as the two disagree, so a
//! range changed in one place cannot go unnoticed in the other.
//!
//! It looks for the catalog in `crates/swifty-hub/src/catalog.rs` and in every
//! file under `crates/swifty-hub/src/catalog/`, wherever the hub keeps it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use swifty_plugin_testkit::conformance::corpus;

/// The hub's catalog source: every file that holds part of it, joined.
fn catalog_source() -> String {
    let hub = Path::new(env!("CARGO_MANIFEST_DIR")).join("../swifty-hub/src");
    let mut files: Vec<PathBuf> = Vec::new();
    let single = hub.join("catalog.rs");
    if single.is_file() {
        files.push(single);
    }
    collect(&hub.join("catalog"), &mut files);
    assert!(
        !files.is_empty(),
        "the hub's catalog is neither {}/catalog.rs nor under {}/catalog/; point this test at it",
        hub.display(),
        hub.display()
    );
    files
        .iter()
        .map(|file| without_tests(&fs::read_to_string(file).unwrap()).to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A source file without its test module, which by convention closes the file. The tests
/// define example actions of their own, and their ranges are not the catalog's.
fn without_tests(source: &str) -> &str {
    source.find("#[cfg(test)]").map_or(source, |at| &source[..at])
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The text from `open` (an opening bracket) to its closing one, skipping
/// brackets inside string literals.
fn balanced(text: &str, open: usize) -> &str {
    let bytes = text.as_bytes();
    let (left, right) = (bytes[open], if bytes[open] == b'{' { b'}' } else { b']' });
    let (mut depth, mut in_string, mut at) = (0usize, false, open);
    while at < bytes.len() {
        match bytes[at] {
            b'\\' if in_string => at += 1,
            b'"' => in_string = !in_string,
            byte if !in_string && byte == left => depth += 1,
            byte if !in_string && byte == right => {
                depth -= 1;
                if depth == 0 {
                    return &text[open..=at];
                }
            }
            _ => {}
        }
        at += 1;
    }
    &text[open..]
}

/// Every string literal in `text`.
fn literals(text: &str) -> Vec<String> {
    text.split('"').skip(1).step_by(2).map(str::to_string).collect()
}

/// The strings of `const NAME: &[&str] = &[...]`.
fn word_list(source: &str, name: &str) -> Vec<String> {
    let declaration = format!("const {name}: &[&str] = &[");
    let at = source.find(&declaration).unwrap_or_else(|| panic!("the catalog no longer declares `{name}`"));
    literals(balanced(source, at + declaration.len() - 1))
}

/// The number after `key:` in `text`.
fn number(text: &str, key: &str) -> Option<i64> {
    let rest = &text[text.find(key)? + key.len()..];
    let digits: String = rest.trim_start().chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect();
    digits.parse().ok()
}

/// Each parameter name with every integer range the catalog gives it.
fn int_ranges(source: &str) -> BTreeMap<String, BTreeSet<(i64, i64)>> {
    let mut ranges: BTreeMap<String, BTreeSet<(i64, i64)>> = BTreeMap::new();
    for (at, _) in source.match_indices("ParamDef {") {
        let block = balanced(source, at + "ParamDef ".len());
        let Some(kind) = block.find("ParamKind::Int {") else { continue };
        let Some(name) = block.find("name:").and_then(|name| literals(&block[name..]).into_iter().next()) else {
            continue;
        };
        let kind = &block[kind..];
        if let (Some(min), Some(max)) = (number(kind, "min:"), number(kind, "max:")) {
            ranges.entry(name).or_default().insert((min, max));
        }
    }
    ranges
}

#[test]
fn the_corpus_has_the_catalogs_word_lists() {
    let source = catalog_source();
    for (name, ours) in [
        ("TV_INPUTS", corpus::TV_INPUTS),
        ("TV_KEYS", corpus::TV_KEYS),
        ("AC_MODES", corpus::AC_MODES),
        ("AC_FANS", corpus::AC_FANS),
    ] {
        assert_eq!(word_list(&source, name), ours, "`{name}` in conformance/corpus.rs differs from the catalog's");
    }
}

#[test]
fn the_corpus_has_the_catalogs_ranges() {
    let ranges = int_ranges(&catalog_source());
    for (param, ours) in [
        ("percent", corpus::PERCENT),
        ("hue", corpus::HUE),
        ("level", corpus::VOLUME),
        ("celsius", corpus::CELSIUS),
        ("grams", corpus::GRAMS),
    ] {
        let theirs = ranges.get(param).unwrap_or_else(|| panic!("the catalog has no integer parameter `{param}`"));
        assert_eq!(
            theirs,
            &BTreeSet::from([ours]),
            "the range of `{param}` in conformance/corpus.rs differs from the catalog's"
        );
    }
}

#[test]
fn the_corpus_caps_the_volume_as_the_movie_scene_does() {
    let source = catalog_source();
    let caps: BTreeSet<i64> =
        source.match_indices("VolumeCap {").filter_map(|(at, _)| number(&source[at..], "max:")).collect();
    // Once the cap is scene data rather than code, there is nothing here to compare.
    if !caps.is_empty() {
        assert_eq!(caps, BTreeSet::from([i64::from(corpus::MOVIE_VOLUME_CAP)]), "the movie scene's volume cap moved");
    }
}

#[test]
fn the_reader_finds_what_it_is_shown() {
    let source = r#"
        pub const TV_INPUTS: &[&str] = &["HDMI 1", "Live TV"];
        const P: ParamDef = ParamDef { name: "percent", kind: ParamKind::Int { min: 1, max: 100 }, help: "a {b}" };
        ActionDef { params: &[ParamDef { name: "hue", kind: ParamKind::Int { min: 0, max: 359 }, required: true }] }
        ActionDef { params: &[ParamDef { name: "on", kind: ParamKind::Bool, required: true }] }
        step("speaker", Op::VolumeCap { max: 30 })
    "#;
    assert_eq!(word_list(source, "TV_INPUTS"), ["HDMI 1", "Live TV"]);
    let ranges = int_ranges(source);
    assert_eq!(ranges.len(), 2);
    assert_eq!(ranges["percent"], BTreeSet::from([(1, 100)]));
    assert_eq!(ranges["hue"], BTreeSet::from([(0, 359)]));
}
