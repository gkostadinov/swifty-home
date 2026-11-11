//! `cargo xtask check-public-tree`: reads every file of the tree and fails when one
//! holds something that belongs to a person rather than to the project.
//!
//! ```text
//! cargo xtask check-public-tree                        the generic rules, as CI runs them
//! cargo xtask check-public-tree --denylist FILE        also a private list of words
//! cargo xtask check-public-tree --root DIR             a tree other than this checkout
//! ```
//!
//! The generic rules name nothing private, so they can live in a public repository:
//!
//! 1. no **MAC address** outside the documentation values (`02:00:00:00:00:xx`,
//!    `AA:BB:CC:DD:EE:FF`, `00:00:5E:00:53:xx`, all zeros, all ones);
//! 2. no **IPv4 literal** outside the documentation ranges (`192.0.2.0/24`,
//!    `198.51.100.0/24`, `203.0.113.0/24`), loopback, and a short list of generic values:
//!    `0.0.0.0`, `255.255.255.255`, a netmask, the base of a private range
//!    (`10.0.0.0`, `172.16.0.0`, `192.168.0.0`, `169.254.0.0`, `100.64.0.0`), the cloud
//!    metadata address, two well-known multicast groups, and two public resolvers;
//! 3. no **e-mail address** outside `NOTICE` and `SECURITY.md`, unless its domain is one
//!    the next rule allows (`someone@example.org` is not anyone's address);
//! 4. no **domain** outside `example.org`, `example.com`, `example.net`, the reserved
//!    names (`.test`, `.example`, `.invalid`, `.localhost`), the vendor API hosts that the
//!    plugins' manifests declare, and the public sites that licences, credits and docs
//!    link to (lists below);
//! 5. no **path under a person's home directory**: `/home/<name>` and `C:\Users\<name>`.
//!
//! `--denylist FILE` adds the one thing the generic rules cannot know: the owner's own
//! words. The file holds one literal per line, matched without regard to case, with `#`
//! comments and blank lines ignored. `word:NAME` matches NAME only as a whole word (for a
//! short name that is also part of longer, unrelated words). `allow:TEXT` denies nothing:
//! TEXT is cut out of each line before the entries are matched, so the project's own
//! published address can name its owner while the name anywhere else is still caught. A match is reported by file and line only, never with
//! the word, so a log never repeats the list. The list itself must never be in the tree.
//!
//! The scan covers the files git knows (tracked, and untracked but not ignored); outside a
//! git checkout it walks the directory. It skips binary files. It prints each finding as
//! `path:line: rule: text` and exits 1; a clean tree exits 0; a usage error exits 2.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// One line for `cargo xtask help`.
pub const ABOUT: &str =
    "Checks the tree for private data with the generic rules, and a private word list: [--denylist FILE] [--root DIR]";

/// Example domains: the names of the documentation. A subdomain is as good as the name.
const EXAMPLE_DOMAINS: &[&str] = &["example.org", "example.com", "example.net"];

/// Top-level names that cannot be anyone's (RFC 2606 and RFC 6761).
const RESERVED_TLDS: &[&str] = &["test", "example", "invalid", "localhost"];

/// The vendor API hosts the plugins' manifests declare (`NetUse::CloudHost`), at the level
/// of the name the vendor owns, and the default weather service. A test below reads every
/// plugin's manifest and fails when one declares a host this list does not cover.
const VENDOR_API_HOSTS: &[&str] = &[
    "petkt.com",
    "petktasia.com",
    "petkit.cn",
    "eu-pet.com",
    "toshibahomeaccontrols.com",
    "azure-devices.net",
    "tuyaeu.com",
    "tuyaus.com",
    "tuyain.com",
    "tuyacn.com",
    "open-meteo.com",
];

/// Public sites a licence notice, a credit, a schema namespace or a link in the docs may
/// name: code hosting, registries, standards and licences. None of them is a person's.
const PUBLIC_SITES: &[&str] = &[
    "github.com",
    "githubusercontent.com",
    "ghcr.io",
    "gcr.io",
    "gitlab.com",
    "crates.io",
    "docs.rs",
    "rust-lang.org",
    "apache.org",
    "creativecommons.org",
    "w3.org",
    "xmlsoap.org",
    "unicode.org",
    "semver.org",
    "keepachangelog.com",
    "contributor-covenant.org",
    "huggingface.co",
    "meethue.com",
];

/// Names that end in these are domains wherever they stand: little else in source reads
/// like a word, a dot and `com`.
const STRONG_TLDS: &[&str] = &["com", "org", "net", "edu", "gov", "mil", "biz", "io", "eu"];

/// Names that end in these are domains only in a URL or after an `@`: elsewhere they are
/// as often a field (`self.app`, `logger.info`, `config.home`) as a host.
const WEAK_TLDS: &[&str] = &[
    "app",
    "dev",
    "cloud",
    "info",
    "xyz",
    "online",
    "site",
    "tech",
    "co",
    "ai",
    "me",
    "tv",
    "cc",
    "local",
    "lan",
    "home",
    "internal",
    "intranet",
    "corp",
    "localdomain",
    "arpa",
    "de",
    "uk",
    "us",
    "fr",
    "nl",
    "ch",
    "se",
    "no",
    "dk",
    "fi",
    "cz",
    "bg",
    "ru",
    "ua",
    "jp",
    "cn",
    "kr",
    "au",
    "nz",
    "ca",
    "br",
    "es",
    "pt",
    "gr",
    "ro",
    "hu",
    "ie",
    "lu",
    "li",
    "sg",
    "hk",
    "tw",
    "it",
    "at",
    "be",
    "pl",
    "tr",
    "il",
    "za",
    "mx",
    "ar",
    "cl",
    "in",
    "lt",
    "lv",
    "ee",
    "si",
    "hr",
];

/// The files whose e-mail addresses are the project's contacts.
const EMAIL_FILES: &[&str] = &["NOTICE", "SECURITY.md"];

/// Directories that are never part of the tree.
const SKIPPED_DIRS: &[&str] = &[".git", "target", "node_modules"];

/// Which rule a finding broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    Mac,
    Ipv4,
    Email,
    Domain,
    HomePath,
    Denylist,
}

impl Rule {
    fn name(self) -> &'static str {
        match self {
            Rule::Mac => "mac address",
            Rule::Ipv4 => "ipv4 address",
            Rule::Email => "e-mail address",
            Rule::Domain => "domain",
            Rule::HomePath => "home directory path",
            Rule::Denylist => "denylist",
        }
    }
}

/// One thing the scan objects to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The file, relative to the root, with `/` separators.
    pub path: String,
    /// The line, from 1; 0 for a file name.
    pub line: usize,
    pub rule: Rule,
    /// What matched. Empty for the denylist, whose words are never repeated.
    pub text: String,
}

impl Finding {
    fn show(&self) -> String {
        let place = format!("{}:{}", self.path, self.line);
        if self.rule == Rule::Denylist {
            format!("{place}: a denylist entry matches")
        } else {
            format!("{place}: {}: {}", self.rule.name(), self.text)
        }
    }
}

/// What a scan found out.
#[derive(Debug, Default)]
pub struct Report {
    /// Text files read.
    pub files: usize,
    pub findings: Vec<Finding>,
}

// ---------------------------------------------------------------------------------------
// The rules, line by line.

/// Every finding of the generic rules in one line.
fn scan_line(path: &str, line_number: usize, line: &str, findings: &mut Vec<Finding>) {
    let mut push = |rule: Rule, text: &str| {
        findings.push(Finding { path: path.to_string(), line: line_number, rule, text: text.to_string() })
    };
    let email_file = EMAIL_FILES.contains(&path);
    for found in mac_addresses(line) {
        if !mac_allowed(&found) {
            push(Rule::Mac, &found);
        }
    }
    for (text, octets) in ipv4_addresses(line) {
        if !ipv4_allowed(octets) {
            push(Rule::Ipv4, text);
        }
    }
    for host in hosts(line) {
        let is_email = host.context == Context::Email;
        if is_email && email_file {
            continue;
        }
        if host_flagged(&host) {
            push(if is_email { Rule::Email } else { Rule::Domain }, host.text);
        }
    }
    for text in home_paths(line) {
        push(Rule::HomePath, text);
    }
}

/// Every finding in a file's text: the generic rules, then the denylist's words.
pub fn scan_text(path: &str, text: &str, denylist: &[String]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (index, line) in text.lines().enumerate() {
        scan_line(path, index + 1, line, &mut findings);
        if !denylist.is_empty() {
            let lower = line.to_lowercase();
            if denied_line(&lower, denylist) {
                findings.push(Finding {
                    path: path.to_string(),
                    line: index + 1,
                    rule: Rule::Denylist,
                    text: String::new(),
                });
            }
        }
    }
    findings
}

// MAC addresses.

/// `aa:bb:cc:dd:ee:ff` and `aa-bb-cc-dd-ee-ff`, as lower case with colons.
fn mac_addresses(line: &str) -> Vec<String> {
    let b = line.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i + 17 <= b.len() {
        let separator = b[i + 2];
        let shaped = (separator == b':' || separator == b'-')
            && (0..6).all(|group| {
                let at = i + group * 3;
                b[at].is_ascii_hexdigit() && b[at + 1].is_ascii_hexdigit() && (group == 5 || b[at + 2] == separator)
            });
        let before_ok = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b':' || b[i - 1] == separator);
        let after_ok = i + 17 == b.len() || !(b[i + 17].is_ascii_alphanumeric() || b[i + 17] == b':');
        if shaped && before_ok && after_ok {
            found.push(line[i..i + 17].to_ascii_lowercase().replace('-', ":"));
            i += 17;
        } else {
            i += 1;
        }
    }
    found
}

fn mac_allowed(mac: &str) -> bool {
    mac.starts_with("02:00:00:00:00:")
        || mac.starts_with("00:00:5e:00:53:")
        || matches!(mac, "aa:bb:cc:dd:ee:ff" | "00:00:00:00:00:00" | "ff:ff:ff:ff:ff:ff")
}

// IPv4 addresses.

/// Each `a.b.c.d` of four numbers up to 255, as written and as numbers. A number that
/// is part of a longer dotted run (a version, a path of an object) is not an address.
fn ipv4_addresses(line: &str) -> Vec<(&str, [u8; 4])> {
    let b = line.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let starts_here = b[i].is_ascii_digit()
            && (i == 0
                || !(b[i - 1].is_ascii_alphanumeric() || (b[i - 1] == b'.' && i >= 2 && b[i - 2].is_ascii_digit())));
        if !starts_here {
            i += 1;
            continue;
        }
        let Some((end, octets)) = ipv4_at(b, i) else {
            // Skip the whole run of digits, so `1234.5.6.7` is not read from its tail.
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            continue;
        };
        let continues = end < b.len()
            && (b[end].is_ascii_digit()
                || b[end].is_ascii_alphabetic()
                || (b[end] == b'.' && end + 1 < b.len() && b[end + 1].is_ascii_digit()));
        if !continues && !after_a_browser_product(b, i) {
            found.push((&line[i..end], octets));
        }
        i = end;
    }
    found
}

/// The four numbers starting at `at`, and where they end.
fn ipv4_at(b: &[u8], at: usize) -> Option<(usize, [u8; 4])> {
    let mut octets = [0u8; 4];
    let mut i = at;
    for (index, octet) in octets.iter_mut().enumerate() {
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() && i - start < 3 {
            i += 1;
        }
        if i == start {
            return None;
        }
        let value: u16 = std::str::from_utf8(&b[start..i]).ok()?.parse().ok()?;
        *octet = u8::try_from(value).ok()?;
        if index < 3 {
            if i >= b.len() || b[i] != b'.' {
                return None;
            }
            i += 1;
        }
    }
    Some((i, octets))
}

/// `Chrome/131.0.0.0`: a browser's version, not an address.
fn after_a_browser_product(b: &[u8], at: usize) -> bool {
    if at == 0 || b[at - 1] != b'/' {
        return false;
    }
    let end = at - 1;
    let mut start = end;
    while start > 0 && b[start - 1].is_ascii_alphabetic() {
        start -= 1;
    }
    let product = String::from_utf8_lossy(&b[start..end]).to_ascii_lowercase();
    matches!(product.as_str(), "chrome" | "safari" | "firefox" | "edge" | "edg" | "opr" | "version" | "applewebkit")
}

fn ipv4_allowed(o: [u8; 4]) -> bool {
    match o {
        [127, ..] | [192, 0, 2, _] | [198, 51, 100, _] | [203, 0, 113, _] => true,
        [0, 0, 0, 0] | [255, 255, 255, 255] => true,
        [10, 0, 0, 0] | [172, 16, 0, 0] | [192, 168, 0, 0] | [169, 254, 0, 0] | [100, 64, 0, 0] => true,
        [169, 254, 169, 254] | [224, 0, 0, 1] | [224, 0, 0, 251] | [239, 255, 255, 250] => true,
        [8, 8, 8, 8] | [1, 1, 1, 1] => true,
        _ => is_netmask(o),
    }
}

/// `255.255.255.0`: ones, then zeros.
fn is_netmask(o: [u8; 4]) -> bool {
    let bits = u32::from_be_bytes(o);
    bits.leading_ones() + bits.trailing_zeros() == 32
}

// Domains and e-mail addresses.

/// What stands in front of a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Context {
    /// `scheme://` or `//`.
    Url,
    /// `local@`: an e-mail address.
    Email,
    /// An `@` with no local part before it.
    At,
    /// Anything else.
    Plain,
}

/// A dotted name in a line.
#[derive(Debug)]
struct Host<'a> {
    text: &'a str,
    context: Context,
}

fn is_host_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-'
}

/// Every dotted run of letters, digits and hyphens that ends in a name a domain can end in
/// and is not plainly a path, a call or a field of something longer.
fn hosts(line: &str) -> Vec<Host<'_>> {
    let b = line.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !is_host_byte(b[i]) {
            i += 1;
            continue;
        }
        let run_start = i;
        while i < b.len() && is_host_byte(b[i]) {
            i += 1;
        }
        let (mut start, mut end) = (run_start, i);
        while start < end && matches!(b[start], b'.' | b'-') {
            start += 1;
        }
        while end > start && matches!(b[end - 1], b'.' | b'-') {
            end -= 1;
        }
        // Two dots in a row end a name: what follows is a new one.
        if let Some(gap) = line[start..end].rfind("..") {
            start += gap + 2;
        }
        if start >= end || !line[start..end].contains('.') {
            continue;
        }
        let text = &line[start..end];
        let next = b.get(i).copied();
        let after_next = b.get(i + 1).copied();
        if next == Some(b'(') || next == Some(b'_') || (next == Some(b':') && after_next == Some(b':')) {
            continue;
        }
        let context = match start.checked_sub(1).map(|at| b[at]) {
            Some(b'@') if start >= 2 && (b[start - 2].is_ascii_alphanumeric() || b"._%+-".contains(&b[start - 2])) => {
                Context::Email
            }
            Some(b'@') => Context::At,
            Some(b'/') if start >= 2 && b[start - 2] == b'/' => Context::Url,
            Some(b'/') | Some(b'\\') => continue,
            _ => Context::Plain,
        };
        found.push(Host { text, context });
    }
    found
}

/// Whether the rules object to a name.
fn host_flagged(host: &Host<'_>) -> bool {
    let name = host.text.to_ascii_lowercase();
    let labels: Vec<&str> = name.split('.').collect();
    let Some(tld) = labels.last().copied() else { return false };
    if labels.len() < 2 || labels.iter().all(|label| label.bytes().all(|byte| byte.is_ascii_digit())) {
        return false;
    }
    if domain_allowed(&name) {
        return false;
    }
    if STRONG_TLDS.contains(&tld) {
        return true;
    }
    WEAK_TLDS.contains(&tld) && matches!(host.context, Context::Url | Context::Email | Context::At)
}

/// Whether a lower-case name is `example.org`, a name under it, a reserved name, a vendor
/// API host or a public site.
fn domain_allowed(name: &str) -> bool {
    let tld = name.rsplit('.').next().unwrap_or(name);
    if RESERVED_TLDS.contains(&tld) {
        return true;
    }
    EXAMPLE_DOMAINS
        .iter()
        .chain(VENDOR_API_HOSTS)
        .chain(PUBLIC_SITES)
        .any(|allowed| name == *allowed || name.strip_suffix(allowed).is_some_and(|rest| rest.ends_with('.')))
}

// Paths under a home directory.

/// A path that begins at a person's home directory and names the person: `/home/`, or a
/// drive's `Users` folder (its separators backslashes, doubled backslashes, or slashes),
/// then a name. The bare directory does not count.
fn home_paths(line: &str) -> Vec<&str> {
    let b = line.as_bytes();
    let name_byte = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let mut found = Vec::new();
    for at in 0..b.len() {
        let before_ok = at == 0 || !(b[at - 1].is_ascii_alphanumeric() || b"_-./\\~".contains(&b[at - 1]));
        if !before_ok {
            continue;
        }
        let name_at = if b[at..].starts_with(b"/home/") {
            Some(at + "/home/".len())
        } else if b[at].is_ascii_alphabetic() && b.get(at + 1) == Some(&b':') {
            windows_users_dir(b, at + 2)
        } else {
            None
        };
        if let Some(name_at) = name_at.filter(|&name_at| b.get(name_at).is_some_and(|&byte| name_byte(byte))) {
            let mut end = name_at;
            while end < b.len() && (name_byte(b[end]) || b"-.".contains(&b[end])) {
                end += 1;
            }
            found.push(&line[at..end]);
        }
    }
    found
}

/// Where the name starts in a Users folder (single or doubled backslashes, or slashes) when `b[at..]`
/// is one of them.
fn windows_users_dir(b: &[u8], at: usize) -> Option<usize> {
    let backslashes = |from: usize| b[from.min(b.len())..].iter().take_while(|&&byte| byte == b'\\').count();
    let mut i = at;
    match (backslashes(i), b.get(i)) {
        (0, Some(b'/')) => i += 1,
        (0, _) => return None,
        (count, _) => i += count.min(2),
    }
    if b.len() < i + 5 || !b[i..i + 5].eq_ignore_ascii_case(b"users") {
        return None;
    }
    i += 5;
    match (backslashes(i), b.get(i)) {
        (0, Some(b'/')) => Some(i + 1),
        (0, _) => None,
        (count, _) => Some(i + count.min(2)),
    }
}

// ---------------------------------------------------------------------------------------
// The tree.

/// The prefix of a denylist line that allows a text instead of denying one.
const ALLOW: &str = "allow:";

/// Whether any denylist entry occurs in `lower` (already lower case), once every
/// `allow:TEXT` line's text is cut out of it: the project's own published address may
/// name its owner, while the name alone is still caught.
pub fn denied_line(lower: &str, denylist: &[String]) -> bool {
    let mut text = lower.to_string();
    for allowed in denylist.iter().filter_map(|entry| entry.strip_prefix(ALLOW)).filter(|a| !a.is_empty()) {
        text = text.replace(allowed, " ");
    }
    denylist.iter().filter(|entry| !entry.starts_with(ALLOW)).any(|entry| denied(&text, entry))
}

/// Whether a denylist entry occurs in `lower` (already lower case). An entry written as
/// `word:NAME` matches NAME only as a whole word, so a short name does not match inside a
/// longer one; any other entry matches anywhere.
pub fn denied(lower: &str, entry: &str) -> bool {
    let Some(word) = entry.strip_prefix("word:") else { return lower.contains(entry) };
    let is_word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    !word.is_empty()
        && lower.match_indices(word).any(|(at, _)| {
            !is_word(lower[..at].chars().next_back()) && !is_word(lower[at + word.len()..].chars().next())
        })
}

/// The words of a denylist, lower case: one per line, `#` comments and blank lines left out.
pub fn parse_denylist(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_lowercase)
        .collect()
}

/// The files to read, relative to `root` with `/` separators: git's list when `root` is a
/// checkout, else everything under it.
fn list_files(root: &Path) -> Result<Vec<String>, String> {
    let git = root.join(".git");
    if git.is_file() || git.join("HEAD").exists() {
        let output = Command::new("git")
            .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
            .current_dir(root)
            .output();
        if let Ok(output) = output {
            if output.status.success() {
                let mut files: Vec<String> = output
                    .stdout
                    .split(|byte| *byte == 0)
                    .filter(|name| !name.is_empty())
                    .map(|name| String::from_utf8_lossy(name).into_owned())
                    .collect();
                files.sort();
                files.dedup();
                return Ok(files);
            }
        }
    }
    let mut files = Vec::new();
    walk(root, "", &mut files).map_err(|error| format!("cannot read {}: {error}", root.display()))?;
    files.sort();
    Ok(files)
}

fn walk(dir: &Path, prefix: &str, files: &mut Vec<String>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        let kind = entry.file_type()?;
        if kind.is_dir() {
            if !SKIPPED_DIRS.contains(&name.as_str()) {
                walk(&entry.path(), &relative, files)?;
            }
        } else if kind.is_file() {
            files.push(relative);
        }
    }
    Ok(())
}

/// Reads the tree under `root` and applies the generic rules and the denylist's words.
pub fn scan_tree(root: &Path, denylist: &[String]) -> Result<Report, String> {
    let mut report = Report::default();
    for relative in list_files(root)? {
        let lower_name = relative.to_lowercase();
        if denied_line(&lower_name, denylist) {
            report.findings.push(Finding {
                path: relative.clone(),
                line: 0,
                rule: Rule::Denylist,
                text: String::new(),
            });
        }
        let Ok(bytes) = fs::read(root.join(&relative)) else { continue };
        if bytes.contains(&0) {
            continue;
        }
        report.files += 1;
        report.findings.extend(scan_text(&relative, &String::from_utf8_lossy(&bytes), denylist));
    }
    report.findings.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    Ok(report)
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    denylist: Option<PathBuf>,
    root: Option<PathBuf>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let slot = match arg.as_str() {
            "--denylist" => &mut options.denylist,
            "--root" => &mut options.root,
            other => return Err(format!("unknown argument `{other}`")),
        };
        let value = args.next().ok_or_else(|| format!("{arg} needs a path"))?;
        *slot = Some(PathBuf::from(value));
    }
    Ok(options)
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
}

/// Runs the command.
pub fn run(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("cargo xtask check-public-tree: {message}\nUsage: cargo xtask check-public-tree [--denylist FILE] [--root DIR]");
            return ExitCode::from(2);
        }
    };
    let denylist = match &options.denylist {
        None => Vec::new(),
        Some(path) => match fs::read_to_string(path) {
            Ok(text) => {
                let words = parse_denylist(&text);
                if words.is_empty() {
                    eprintln!("cargo xtask check-public-tree: {} holds no entries", path.display());
                    return ExitCode::from(2);
                }
                words
            }
            Err(error) => {
                eprintln!("cargo xtask check-public-tree: cannot read {}: {error}", path.display());
                return ExitCode::from(2);
            }
        },
    };
    let root = options.root.unwrap_or_else(workspace_root);
    let report = match scan_tree(&root, &denylist) {
        Ok(report) => report,
        Err(message) => {
            eprintln!("cargo xtask check-public-tree: {message}");
            return ExitCode::from(2);
        }
    };
    let list = if denylist.is_empty() {
        "the generic rules".to_string()
    } else {
        format!("the generic rules and a denylist of {} entries", denylist.len())
    };
    if report.findings.is_empty() {
        println!("check-public-tree: clean ({} files, {list})", report.files);
        return ExitCode::SUCCESS;
    }
    for finding in &report.findings {
        eprintln!("{}", finding.show());
    }
    let mut files: Vec<&str> = report.findings.iter().map(|finding| finding.path.as_str()).collect();
    files.dedup();
    eprintln!(
        "check-public-tree: {} findings in {} files ({} files read, {list})",
        report.findings.len(),
        files.len(),
        report.files
    );
    ExitCode::from(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pieces joined with a separator: the values below that the rules object to are made
    /// here, so that this file passes the rules it implements.
    fn join(parts: &[&str], separator: &str) -> String {
        parts.join(separator)
    }

    fn private_ip() -> String {
        join(&["10", "20", "30", "40"], ".")
    }

    fn private_mac() -> String {
        join(&["00", "11", "22", "33", "44", "55"], ":")
    }

    fn private_domain() -> String {
        join(&["intranet", "corp-example", "net"], ".")
    }

    fn private_email() -> String {
        format!("someone@{}", join(&["mail-example", "net"], "."))
    }

    fn home_path() -> String {
        join(&["", "home", "alice", ".config"], "/")
    }

    fn windows_home() -> String {
        format!("C:{}{}", "\\Users\\", "alice")
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-public-tree-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn rules_of(text: &str) -> Vec<Rule> {
        scan_text("a.txt", text, &[]).into_iter().map(|finding| finding.rule).collect()
    }

    #[test]
    fn a_clean_tree_passes_and_a_tree_with_one_violation_of_each_kind_fails() {
        let clean = scratch("clean");
        write(&clean, "README.md", "Swifty at https://home.example.org, bulb 192.0.2.7, mac 02:00:00:00:00:01.\n");
        write(&clean, "src/lib.rs", "let host = \"127.0.0.1\"; // a.b(c)\n");
        let report = scan_tree(&clean, &[]).unwrap();
        assert_eq!((report.files, report.findings.len()), (2, 0), "{:?}", report.findings);

        let dirty = scratch("dirty");
        write(&dirty, "ip.txt", &format!("the hub is at {}\n", private_ip()));
        write(&dirty, "mac.txt", &format!("wake {}\n", private_mac()));
        write(&dirty, "mail.txt", &format!("write to {}\n", private_email()));
        write(&dirty, "site.txt", &format!("open https://{}/\n", private_domain()));
        write(&dirty, "path.txt", &format!("data in {}\n", home_path()));
        let report = scan_tree(&dirty, &[]).unwrap();
        let by_file: Vec<(&str, Rule)> = report.findings.iter().map(|f| (f.path.as_str(), f.rule)).collect();
        assert!(by_file.contains(&("ip.txt", Rule::Ipv4)), "{by_file:?}");
        assert!(by_file.contains(&("mac.txt", Rule::Mac)), "{by_file:?}");
        assert!(by_file.contains(&("mail.txt", Rule::Email)), "{by_file:?}");
        assert!(by_file.contains(&("site.txt", Rule::Domain)), "{by_file:?}");
        assert!(by_file.contains(&("path.txt", Rule::HomePath)), "{by_file:?}");
        assert_eq!(report.findings.len(), 5, "one each, and the e-mail's domain is not counted twice: {by_file:?}");
        let _ = fs::remove_dir_all(clean);
        let _ = fs::remove_dir_all(dirty);
    }

    #[test]
    fn the_documentation_and_generic_values_pass() {
        let allowed = [
            "192.0.2.1 198.51.100.254 203.0.113.9 127.0.0.1 127.1.2.3 0.0.0.0 255.255.255.255",
            "255.255.255.0 255.255.0.0 128.0.0.0 10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 169.254.0.0/16",
            "169.254.169.254 224.0.0.251 239.255.255.250 8.8.8.8 1.1.1.1",
            "Chrome/131.0.0.0 version 1.2.3 and 10.0 and 1.2.3.4.5",
            "02:00:00:00:00:01 02-00-00-00-00-ff AA:BB:CC:DD:EE:FF 00:00:5E:00:53:01 00:00:00:00:00:00",
            "https://home.example.org https://api.example.org/v1 tv.example.com a.test x.example",
            "https://github.com/x/y https://crates.io https://api.petkt.com mobileapi.toshibahomeaccontrols.com",
            "someone@example.org git@github.com me@example.com",
            "self.app logger.info config.home ctx.net() std.io::Read crates.io-index file.rs Cargo.toml index.html",
            "see docs/security.md and src/main.rs, ./a/b.org and /usr/local/bin/swifty and /srv/home/hub.db",
            "the /home/ directory and C:\\Users\\ are not paths of anyone's",
        ];
        for line in allowed {
            assert_eq!(rules_of(line), Vec::<Rule>::new(), "{line}");
        }
    }

    #[test]
    fn each_rule_names_what_it_found() {
        let bad = scan_text("a.txt", &format!("x {} y\n", private_ip()), &[]);
        assert_eq!(bad, [Finding { path: "a.txt".into(), line: 1, rule: Rule::Ipv4, text: private_ip() }]);
        let bad = scan_text("a.txt", &format!("\n\nmac={}\n", private_mac().to_uppercase()), &[]);
        assert_eq!((bad[0].line, bad[0].rule, bad[0].text.as_str()), (3, Rule::Mac, private_mac().as_str()));
        assert_eq!(rules_of(&format!("MAC {}", private_mac().replace(':', "-"))), [Rule::Mac]);
        assert_eq!(rules_of(&format!("ip {} again", private_ip())), [Rule::Ipv4]);
        assert_eq!(rules_of(&format!("{}:8080", join(&["10", "0", "0", "1"], "."))), [Rule::Ipv4]);
        assert_eq!(rules_of(&join(&["192", "168", "001", "005"], ".")), [Rule::Ipv4], "leading zeros");
        assert_eq!(rules_of(&format!("host = \"{}\"", private_domain())), [Rule::Domain]);
        assert_eq!(rules_of(&format!("{}.example.org", "ok")), Vec::<Rule>::new());
        assert_eq!(rules_of(&format!("evil-{}", join(&["petkt", "com"], "."))), [Rule::Domain], "not a subdomain");
        assert_eq!(rules_of(&format!("a {} b", home_path())), [Rule::HomePath]);
        assert_eq!(rules_of(&format!("a {} b", windows_home())), [Rule::HomePath]);
        assert_eq!(rules_of(&format!("a {} b", windows_home().replace('\\', "/"))), [Rule::HomePath]);
        assert_eq!(rules_of(&format!("a {} b", windows_home().replace('\\', "\\\\"))), [Rule::HomePath]);
    }

    #[test]
    fn a_weak_ending_counts_only_in_a_url_or_after_an_at_sign() {
        let name = join(&["lights", "dev"], ".");
        assert_eq!(rules_of(&format!("self.{name}")), Vec::<Rule>::new());
        assert_eq!(rules_of(&format!("https://{name}/x")), [Rule::Domain]);
        assert_eq!(rules_of(&format!("user@{name}")), [Rule::Email]);
        let strong = join(&["lights", "org"], ".");
        assert_eq!(rules_of(&format!("self.{strong}")), [Rule::Domain]);
        assert_eq!(rules_of(&format!("self.{strong}()")), Vec::<Rule>::new(), "a call");
    }

    #[test]
    fn an_email_address_is_the_projects_in_notice_and_security_md_only() {
        let email = private_email();
        assert_eq!(scan_text("NOTICE", &email, &[]), []);
        assert_eq!(scan_text("SECURITY.md", &format!("Mail {email}."), &[]), []);
        assert_eq!(scan_text("docs/x.md", &email, &[])[0].rule, Rule::Email);
        assert_eq!(scan_text("docs/NOTICE", &email, &[])[0].rule, Rule::Email);
        // The exemption is for contacts, not for addresses in a URL or a path.
        assert_eq!(scan_text("NOTICE", &format!("https://{}", private_domain()), &[])[0].rule, Rule::Domain);
    }

    #[test]
    fn the_denylist_matches_without_regard_to_case_and_never_repeats_a_word() {
        let words = parse_denylist("# private\n\nZebraFish-Host\n  Quux 7  \n");
        assert_eq!(words, ["zebrafish-host", "quux 7"]);
        let found = scan_text("a.txt", "fine\nthe ZEBRAFISH-host is here\nand quux 7 too\n", &words);
        let lines: Vec<(usize, Rule)> = found.iter().map(|f| (f.line, f.rule)).collect();
        assert_eq!(lines, [(2, Rule::Denylist), (3, Rule::Denylist)]);
        assert!(found.iter().all(|f| f.text.is_empty()), "{found:?}");
        assert_eq!(found[0].show(), "a.txt:2: a denylist entry matches");
        assert!(!found[0].show().to_lowercase().contains("zebrafish"));
        assert_eq!(scan_text("a.txt", "fine\n", &words), []);
    }

    #[test]
    fn a_word_entry_matches_only_the_whole_word() {
        let words = parse_denylist("word:Zeb\n");
        let text = "Zebra Institute\n/srv/zeb/x\nzeb\nZeb's notes\nwith zeb2 and _zeb\nzebzeb\n";
        let lines: Vec<usize> = scan_text("a.txt", text, &words).iter().map(|f| f.line).collect();
        assert_eq!(lines, [2, 3, 4]);
        assert!(denied("d:\\data\\zeb", "word:zeb"));
        assert!(!denied("anything", "word:"));
        // A plain entry still matches inside longer words.
        assert!(denied("zebra", "zeb"));
    }

    #[test]
    fn an_allow_line_lets_one_address_name_a_denied_word_and_nothing_else() {
        let words = parse_denylist("quux\nallow:example.org/Quux/hub\n");
        let text = "get https://example.org/quux/hub/latest\nthe quux team\nexample.org/quux/other\n";
        let lines: Vec<usize> = scan_text("a.txt", text, &words).iter().map(|f| f.line).collect();
        assert_eq!(lines, [2, 3]);
        // An allow line alone denies nothing, and an empty one allows nothing.
        assert_eq!(scan_text("a.txt", "allow: anything\n", &parse_denylist("allow:x\nallow:\n")), []);
    }

    #[test]
    fn the_denylist_reads_file_names_too_and_the_tree_fails_with_it() {
        let root = scratch("denylist");
        write(&root, "docs/Zebrafish-notes.md", "nothing here\n");
        write(&root, "src/lib.rs", "let x = 1;\n");
        write(&root, "docs/a.md", "the ZEBRAFISH again\n");
        assert_eq!(scan_tree(&root, &[]).unwrap().findings.len(), 0);
        let report = scan_tree(&root, &parse_denylist("zebrafish")).unwrap();
        let places: Vec<(&str, usize)> = report.findings.iter().map(|f| (f.path.as_str(), f.line)).collect();
        assert_eq!(places, [("docs/Zebrafish-notes.md", 0), ("docs/a.md", 1)]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn binary_files_and_build_output_are_not_read() {
        let root = scratch("binary");
        write(&root, "logo.png", &format!("\u{0}PNG {}\n", private_ip()));
        write(&root, "target/debug/x.txt", &private_ip());
        write(&root, ".git/config", &private_ip());
        write(&root, "ok.txt", "fine\n");
        let report = scan_tree(&root, &[]).unwrap();
        assert_eq!((report.files, report.findings.len()), (1, 0), "{:?}", report.findings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_arguments() {
        let args = |list: &[&str]| list.iter().map(|word| word.to_string()).collect::<Vec<_>>();
        assert_eq!(parse(&args(&[])).unwrap(), Options::default());
        let both = parse(&args(&["--denylist", "d.txt", "--root", "r"])).unwrap();
        assert_eq!((both.denylist, both.root), (Some(PathBuf::from("d.txt")), Some(PathBuf::from("r"))));
        assert!(parse(&args(&["--denylist"])).unwrap_err().contains("needs a path"));
        assert!(parse(&args(&["--nope"])).unwrap_err().contains("unknown argument"));
    }

    /// Every host a plugin's manifest declares must be one the domain rule allows, so that
    /// a new vendor is added to the list on purpose.
    #[test]
    fn every_host_a_manifest_declares_is_on_the_list() {
        let plugins = workspace_root().join("plugins");
        let mut declared = Vec::new();
        for entry in fs::read_dir(&plugins).unwrap().flatten() {
            let mut files = Vec::new();
            let _ = walk(&entry.path().join("src"), "", &mut files);
            for file in files.iter().filter(|file| file.ends_with(".rs")) {
                let text = fs::read_to_string(entry.path().join("src").join(file)).unwrap_or_default();
                for part in text.split("CloudHost { host: \"").skip(1) {
                    if let Some(host) = part.split('"').next() {
                        declared.push(host.to_string());
                    }
                }
            }
        }
        assert!(declared.len() >= 5, "the plugins declare their hosts with NetUse::CloudHost: {declared:?}");
        for host in declared {
            assert!(domain_allowed(&host), "{host} is declared by a manifest and not on the list");
        }
    }

    /// The tree this checker lives in passes its own generic rules.
    #[test]
    fn this_repository_passes_the_generic_rules() {
        let report = scan_tree(&workspace_root(), &[]).unwrap();
        assert!(report.files > 200, "{} files", report.files);
        let shown: Vec<String> = report.findings.iter().take(20).map(Finding::show).collect();
        assert!(
            report.findings.is_empty(),
            "{} findings, the first of them:\n{}",
            report.findings.len(),
            shown.join("\n")
        );
    }
}
