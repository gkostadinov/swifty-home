//! `cargo xtask dist`: builds one release archive,
//! `swifty-home-<version>-<arch>-linux.tar.gz`, and writes `SHA256SUMS` beside it.
//!
//! ```text
//! cargo xtask dist                                  build for this machine's arch (musl) and pack
//! cargo xtask dist --arch aarch64                   build for aarch64-unknown-linux-musl and pack
//! cargo xtask dist --binary FILE --arch x86_64      pack a binary built elsewhere (the release does this)
//! cargo xtask dist --notices FILE                   use these third-party notices
//! cargo xtask dist --without-notices                no cargo-about here: a local, unpublishable archive
//! cargo xtask dist --out DIR                        where to write (default target/dist)
//! ```
//!
//! The archive holds one directory, `swifty-home-<version>-<arch>-linux/`, with the
//! binary, the three systemd units, the example config, `LICENSE`, `NOTICE` and
//! `THIRD-PARTY-NOTICES.txt`: the files `packaging/install.sh` installs. The tar is
//! written here, so every file has owner root and the right mode on any build host;
//! `gzip -n` compresses it. The modification time is `SOURCE_DATE_EPOCH` when set,
//! so a release build is reproducible.
//!
//! `SHA256SUMS` lists every archive in the output directory, in the format
//! `sha256sum -c` reads. Signing it is the release workflow's job.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// One line for `cargo xtask help`.
pub const ABOUT: &str = "Builds a release archive and SHA256SUMS: [--arch A] [--binary FILE] [--notices FILE | --without-notices] [--out DIR]";

/// The architectures there are release archives for.
const ARCHES: &[&str] = &["x86_64", "aarch64"];

/// What the arguments ask for.
#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    arch: Option<String>,
    binary: Option<PathBuf>,
    notices: Option<PathBuf>,
    without_notices: bool,
    out: Option<PathBuf>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().cloned().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--arch" => options.arch = Some(value("--arch")?),
            "--binary" => options.binary = Some(PathBuf::from(value("--binary")?)),
            "--notices" => options.notices = Some(PathBuf::from(value("--notices")?)),
            "--without-notices" => options.without_notices = true,
            "--out" => options.out = Some(PathBuf::from(value("--out")?)),
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    if options.notices.is_some() && options.without_notices {
        return Err("give --notices or --without-notices, not both".to_string());
    }
    if let Some(arch) = &options.arch {
        if !ARCHES.contains(&arch.as_str()) {
            return Err(format!("no release archive for `{arch}`; the architectures are {}", ARCHES.join(", ")));
        }
    }
    Ok(options)
}

/// Runs the command.
pub fn run(args: &[String]) -> ExitCode {
    match parse(args).and_then(|options| dist(&options)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("cargo xtask dist: {message}");
            ExitCode::FAILURE
        }
    }
}

fn workspace_root() -> PathBuf {
    // xtask/ sits in the workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask has a parent directory").to_path_buf()
}

fn dist(options: &Options) -> Result<(), String> {
    let root = workspace_root();
    let manifest = read_text(&root.join("Cargo.toml"))?;
    let version = workspace_version(&manifest).ok_or("no version under [workspace.package] in Cargo.toml")?;
    let arch = match &options.arch {
        Some(arch) => arch.clone(),
        None if ARCHES.contains(&std::env::consts::ARCH) => std::env::consts::ARCH.to_string(),
        None => return Err(format!("this machine is {}; say which archive with --arch", std::env::consts::ARCH)),
    };

    let binary = match &options.binary {
        Some(path) => path.clone(),
        None => build_binary(&root, &arch)?,
    };
    let notices = if let Some(path) = &options.notices {
        read_text(path)?
    } else if options.without_notices {
        eprintln!(
            "cargo xtask dist: warning: no third-party notices. This archive is for local use, not for a release."
        );
        "Third-party notices were not generated for this local build. A release archive\n\
         carries them; see packaging/notices/about.toml.\n"
            .to_string()
    } else {
        generate_notices(&root)?
    };

    let top = format!("swifty-home-{version}-{arch}-linux");
    let read = |path: &Path| fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()));
    let packaging = root.join("packaging");
    let files: Vec<(&str, Vec<u8>, u32)> = vec![
        ("swifty", read(&binary)?, 0o755),
        ("swifty-home.service", read(&packaging.join("systemd/swifty-home.service"))?, 0o644),
        ("swifty-home-backup.service", read(&packaging.join("systemd/swifty-home-backup.service"))?, 0o644),
        ("swifty-home-backup.timer", read(&packaging.join("systemd/swifty-home-backup.timer"))?, 0o644),
        ("swifty.example.toml", read(&packaging.join("swifty.example.toml"))?, 0o644),
        ("LICENSE", read(&root.join("LICENSE"))?, 0o644),
        ("NOTICE", read(&root.join("NOTICE"))?, 0o644),
        ("THIRD-PARTY-NOTICES.txt", notices.into_bytes(), 0o644),
    ];
    if !files[0].1.starts_with(b"\x7fELF") {
        eprintln!("cargo xtask dist: warning: {} is not a Linux (ELF) binary.", binary.display());
    }

    let mtime = source_date_epoch()?;
    let mut tar = Tar::default();
    tar.directory(&format!("{top}/"), mtime)?;
    for (name, data, mode) in &files {
        tar.file(&format!("{top}/{name}"), data, *mode, mtime)?;
    }

    let out = options.out.clone().unwrap_or_else(|| root.join("target").join("dist"));
    fs::create_dir_all(&out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    let tar_path = out.join(format!("{top}.tar"));
    let gz_path = out.join(format!("{top}.tar.gz"));
    fs::write(&tar_path, tar.finish()).map_err(|e| format!("cannot write {}: {e}", tar_path.display()))?;
    let status = Command::new("gzip")
        .args(["-n", "-9", "-f"])
        .arg(&tar_path)
        .status()
        .map_err(|e| format!("cannot run gzip: {e}"))?;
    if !status.success() || !gz_path.is_file() {
        return Err(format!("gzip failed on {}", tar_path.display()));
    }

    let sums = sums_of(&out)?;
    fs::write(out.join("SHA256SUMS"), &sums).map_err(|e| format!("cannot write SHA256SUMS: {e}"))?;
    println!("{}", gz_path.display());
    print!("{sums}");
    Ok(())
}

fn read_text(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// The `version` under `[workspace.package]` in the root manifest.
fn workspace_version(manifest: &str) -> Option<String> {
    let mut in_section = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == "[workspace.package]";
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        if key.trim() == "version" {
            let value = value.trim().strip_prefix('"')?.split('"').next()?;
            return Some(value.to_string());
        }
    }
    None
}

fn cargo() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

fn build_binary(root: &Path, arch: &str) -> Result<PathBuf, String> {
    let triple = format!("{arch}-unknown-linux-musl");
    eprintln!("cargo xtask dist: building swifty for {triple}");
    let status = cargo()
        .current_dir(root)
        .args(["build", "--release", "--locked", "--package", "swifty", "--bin", "swifty", "--target", &triple])
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !status.success() {
        return Err(format!(
            "cargo build for {triple} failed. It needs `rustup target add {triple}` and a C compiler for that \
             target (musl-tools on Debian). Or build elsewhere and pass --binary."
        ));
    }
    let target_dir = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| root.join("target"));
    let target_dir = if target_dir.is_absolute() { target_dir } else { root.join(target_dir) };
    Ok(target_dir.join(&triple).join("release").join("swifty"))
}

fn generate_notices(root: &Path) -> Result<String, String> {
    let output = cargo()
        .current_dir(root)
        .args(["about", "generate", "--locked", "-c", "packaging/notices/about.toml"])
        .args(["-m", "crates/swifty/Cargo.toml", "packaging/notices/about.hbs"])
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !output.status.success() {
        return Err("`cargo about generate` failed. Install it with `cargo install --locked cargo-about`, \
                    or pass --without-notices for an archive you only use locally."
            .to_string());
    }
    String::from_utf8(output.stdout).map_err(|_| "cargo about wrote something that is not UTF-8".to_string())
}

fn source_date_epoch() -> Result<u64, String> {
    match std::env::var("SOURCE_DATE_EPOCH") {
        Ok(value) => value.trim().parse().map_err(|_| format!("SOURCE_DATE_EPOCH is not a number: {value}")),
        Err(_) => Ok(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)),
    }
}

/// `SHA256SUMS` for every release archive in `dir`, sorted by name.
fn sums_of(dir: &Path) -> Result<String, String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map_err(|e| format!("cannot list {}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.starts_with("swifty-home-") && name.ends_with("-linux.tar.gz"))
        .collect();
    names.sort();
    let mut sums = String::new();
    for name in names {
        let data = fs::read(dir.join(&name)).map_err(|e| format!("cannot read {name}: {e}"))?;
        let _ = writeln!(sums, "{}  {name}", hex(&sha256(&data)));
    }
    Ok(sums)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// A ustar archive written in memory: every entry owned by root (0:0), with the
/// mode it is given.
#[derive(Default)]
struct Tar {
    bytes: Vec<u8>,
}

impl Tar {
    const BLOCK: usize = 512;
    /// GNU tar's default record: 20 blocks.
    const RECORD: usize = 20 * Self::BLOCK;

    fn directory(&mut self, name: &str, mtime: u64) -> Result<(), String> {
        let header = header(name, 0o755, 0, mtime, b'5')?;
        self.bytes.extend_from_slice(&header);
        Ok(())
    }

    fn file(&mut self, name: &str, data: &[u8], mode: u32, mtime: u64) -> Result<(), String> {
        let header = header(name, mode, data.len() as u64, mtime, b'0')?;
        self.bytes.extend_from_slice(&header);
        self.bytes.extend_from_slice(data);
        self.pad_to(Self::BLOCK);
        Ok(())
    }

    /// The archive, closed with two zero blocks and padded to a whole record.
    fn finish(mut self) -> Vec<u8> {
        self.bytes.extend_from_slice(&[0; 2 * Self::BLOCK]);
        self.pad_to(Self::RECORD);
        self.bytes
    }

    fn pad_to(&mut self, size: usize) {
        let rest = self.bytes.len() % size;
        if rest != 0 {
            self.bytes.resize(self.bytes.len() + size - rest, 0);
        }
    }
}

/// One ustar header block.
fn header(name: &str, mode: u32, size: u64, mtime: u64, kind: u8) -> Result<[u8; 512], String> {
    if name.len() > 100 || !name.is_ascii() {
        return Err(format!("`{name}` does not fit a tar header (100 ASCII characters at most)"));
    }
    let mut block = [0u8; 512];
    let mut put = |at: usize, value: &[u8]| block[at..at + value.len()].copy_from_slice(value);
    put(0, name.as_bytes());
    put(100, format!("{mode:07o}\0").as_bytes());
    put(108, b"0000000\0");
    put(116, b"0000000\0");
    put(124, format!("{size:011o}\0").as_bytes());
    put(136, format!("{mtime:011o}\0").as_bytes());
    put(148, b"        ");
    put(156, &[kind]);
    put(257, b"ustar\0");
    put(263, b"00");
    put(265, b"root");
    put(297, b"root");
    put(329, b"0000000\0");
    put(337, b"0000000\0");
    let sum: u32 = block.iter().map(|&b| u32::from(b)).sum();
    block[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    Ok(block)
}

/// SHA-256 (FIPS 180-4). xtask uses the standard library only.
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
        0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
        0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
        0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] =
        [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];

    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((data.len() as u64).wrapping_mul(8)).to_be_bytes());

    for chunk in message.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (state, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *state = state.wrapping_add(value);
        }
    }

    let mut digest = [0u8; 32];
    for (bytes, word) in digest.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        *bytes = word.to_be_bytes();
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn sha256_matches_the_standard_vectors() {
        assert_eq!(hex(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            hex(&sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            hex(&sha256(&vec![b'a'; 1_000_000])),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    /// Reads back what `Tar` wrote: (name, mode, uid, gid, kind, data) for each entry,
    /// checking every header's checksum on the way.
    fn read_tar(bytes: &[u8]) -> Vec<(String, u32, u32, u32, u8, Vec<u8>)> {
        let octal = |field: &[u8]| {
            let text: String = field.iter().take_while(|&&b| b != 0 && b != b' ').map(|&b| b as char).collect();
            u64::from_str_radix(&text, 8).unwrap()
        };
        let mut entries = Vec::new();
        let mut at = 0;
        while at + 512 <= bytes.len() {
            let block = &bytes[at..at + 512];
            if block.iter().all(|&b| b == 0) {
                break;
            }
            let mut copy = block.to_vec();
            copy[148..156].copy_from_slice(b"        ");
            let sum: u64 = copy.iter().map(|&b| u64::from(b)).sum();
            assert_eq!(octal(&block[148..156]), sum, "header checksum");
            assert_eq!(&block[257..263], b"ustar\0");
            let name: String = block[..100].iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();
            let size = octal(&block[124..136]) as usize;
            let data = bytes[at + 512..at + 512 + size].to_vec();
            entries.push((
                name,
                octal(&block[100..108]) as u32,
                octal(&block[108..116]) as u32,
                octal(&block[116..124]) as u32,
                block[156],
                data,
            ));
            at += 512 + size.div_ceil(512) * 512;
        }
        entries
    }

    #[test]
    fn the_tar_has_root_owners_modes_and_whole_records() {
        let mut tar = Tar::default();
        tar.directory("swifty-home-0.1.0-x86_64-linux/", 1_700_000_000).unwrap();
        tar.file("swifty-home-0.1.0-x86_64-linux/swifty", &[1u8; 700], 0o755, 1_700_000_000).unwrap();
        tar.file("swifty-home-0.1.0-x86_64-linux/NOTICE", b"notice\n", 0o644, 1_700_000_000).unwrap();
        let bytes = tar.finish();
        assert_eq!(bytes.len() % Tar::RECORD, 0);

        let entries = read_tar(&bytes);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].0, "swifty-home-0.1.0-x86_64-linux/");
        assert_eq!((entries[0].1, entries[0].4), (0o755, b'5'));
        assert_eq!(entries[1].0, "swifty-home-0.1.0-x86_64-linux/swifty");
        assert_eq!((entries[1].1, entries[1].2, entries[1].3, entries[1].4), (0o755, 0, 0, b'0'));
        assert_eq!(entries[1].5, vec![1u8; 700]);
        assert_eq!((entries[2].1, entries[2].5.as_slice()), (0o644, &b"notice\n"[..]));
    }

    #[test]
    fn a_name_too_long_for_the_header_is_refused() {
        assert!(header(&"a".repeat(101), 0o644, 0, 0, b'0').is_err());
        assert!(header(&"a".repeat(100), 0o644, 0, 0, b'0').is_ok());
    }

    #[test]
    fn the_version_comes_from_workspace_package() {
        let manifest = "[workspace]\nmembers = [\"x\"]\n\n[workspace.package]\nedition = \"2021\"\nversion = \"0.1.0\"\n\n[workspace.dependencies]\nversion = \"9\"\n";
        assert_eq!(workspace_version(manifest).as_deref(), Some("0.1.0"));
        assert_eq!(workspace_version("[package]\nversion = \"1.0.0\"\n"), None);
        let root = read_text(&workspace_root().join("Cargo.toml")).unwrap();
        assert!(workspace_version(&root).is_some());
    }

    #[test]
    fn the_options_are_checked() {
        assert_eq!(parse(&[]).unwrap(), Options::default());
        let options = parse(&args(&["--arch", "aarch64", "--binary", "b", "--out", "o"])).unwrap();
        assert_eq!(options.arch.as_deref(), Some("aarch64"));
        assert_eq!(options.binary, Some(PathBuf::from("b")));
        assert!(parse(&args(&["--arch", "armv7"])).unwrap_err().contains("no release archive"));
        assert!(parse(&args(&["--notices", "n", "--without-notices"])).is_err());
        assert!(parse(&args(&["--binary"])).unwrap_err().contains("needs a value"));
        assert!(parse(&args(&["--sign"])).unwrap_err().contains("unknown argument"));
    }

    #[test]
    fn sums_list_only_release_archives_in_order() {
        let dir = std::env::temp_dir().join(format!("xtask-dist-sums-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("swifty-home-0.1.0-x86_64-linux.tar.gz"), b"abc").unwrap();
        fs::write(dir.join("swifty-home-0.1.0-aarch64-linux.tar.gz"), b"").unwrap();
        fs::write(dir.join("swifty-home-0.1.0-x86_64-linux.tar"), b"left over").unwrap();
        fs::write(dir.join("SHA256SUMS"), b"old").unwrap();
        let sums = sums_of(&dir).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            sums,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  swifty-home-0.1.0-aarch64-linux.tar.gz\n\
             ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  swifty-home-0.1.0-x86_64-linux.tar.gz\n"
        );
    }
}
