# Contributing to Swifty Home

Thank you for helping. This page says how to build and test the hub, what a
change must pass before it is merged, and how commits are written.

## Build

You need Rust. `rust-toolchain.toml` pins the version, and `rustup` installs it
the first time you run `cargo` in the checkout.

```sh
cargo build                      # the hub library and the `swifty` binary
cargo build --release            # what an install runs
cargo run -p swifty -- --help    # the command line
```

The workspace is `crates/*` (the hub, the binary, the plugin contract, the
socket crate and the test kit), `plugins/*` (one crate per integration) and
`xtask/` (build tooling). `services/swifty-speech` is a separate workspace with
its own lock file, and is built on its own.

## Test, format and lint

Every change must pass these, exactly as CI runs them:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny --locked check          # licences and advisories; needs cargo-deny
bash tools/scan-secrets.sh .       # credentials in the tree
cargo xtask check-public-tree      # site data in the tree: see "No site data"
```

`cargo fmt --all` fixes the formatting. `cargo xtask count-tests` runs the tests
and adds up the results; with `--min N` it also fails when fewer than `N` pass.
The number of tests may only go up. A test may be adapted to a change (its
imports, paths, ids and fixture setup), but not weakened. If an assertion has to
change, say which one and why in the pull request.

Tests never reach a real device. The mock devices in
`crates/swifty-plugin-testkit` bind loopback addresses only, and a plugin's own
tests use the same mocks and the conformance suite there.

`cargo xtask help` lists the other developer commands.

## No site data

The repository is public. It holds no data about a real home or network:

- addresses are from the documentation ranges (`192.0.2.x`, `198.51.100.x`,
  `203.0.113.x`) or loopback;
- domains are `example.org`, `example.com` or `.test`;
- names of people, rooms, devices, hosts and places are generic;
- no credential, token, key or certificate, not even an expired one. Test
  fixtures use made-up values that cannot work anywhere.

`tools/scan-secrets.sh` catches the common credential shapes. A reviewed false
positive goes in `tools/scan-secrets.allow` with the hash of its exact line;
`tools/scan-secrets.sh --list .` prints the lines to choose from.

`cargo xtask check-public-tree` reads every file and fails on a MAC address that
is not `02:00:00:00:00:xx` or `AA:BB:CC:DD:EE:FF`, an IPv4 address outside the
documentation ranges, loopback and a few generic values (a netmask, the base of a
private range), an e-mail address outside `NOTICE` and `SECURITY.md` (one at
`example.org` is fine), a domain that is not `example.org`, `example.com`, `.test`,
a vendor API host a plugin manifest declares or a public site such as
`github.com`, and a path under `/home/` or `C:\Users\`. A test that needs a
private address (a parser that must accept one) builds it, for example with
`Ipv4Addr::new(192, 168, 1, 20)`, instead of writing it out. Both checks run in
CI. The maintainer also runs the command with `--denylist FILE` before a
release: the file holds words that only the owner of a house knows, is kept
outside the repository, and is never copied into it.

## Commits

- One logical change per commit, with a plain, specific message: what changed
  and, when it is not obvious, why. For example "Count ignored tests in
  count-tests" rather than "fixes".
- Commits carry no attribution lines of any kind: no `Co-Authored-By`, no
  "Generated with" line, nothing that names a tool or an AI assistant. The author
  of the commit is the person who answers for it.
- Do not rewrite history that has been pushed to a shared branch.

## Writing a plugin

The plugin developer guide is in [`docs/plugins/`](docs/plugins/README.md): what a
plugin is, a step-by-step how-to from `swifty plugin new` to the pull request, the
contract with its reasons, and the test kit. The reference pages in
`docs/plugins/reference/` are generated from the code (`cargo xtask gen-docs`), so a
change to the contract is not finished until they are regenerated. `cargo xtask
check-plugin <id>` runs a plugin's own checks, and `cargo xtask add-plugin` puts a new
one into the build.

## Security

Please report a vulnerability privately, as `SECURITY.md` describes, and not in
a public issue.
