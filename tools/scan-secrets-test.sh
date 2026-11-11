#!/usr/bin/env bash
# Checks tools/scan-secrets.sh against sample trees: what it must report, what it must let through,
# and how its allow list works. Run it from anywhere: bash tools/scan-secrets-test.sh
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
failures=0

# A fresh tree with the scanner in its tools/ directory, as it sits in a checkout.
fresh() {
  rm -rf "$WORK/tree"
  mkdir -p "$WORK/tree/tools"
  cp "$HERE/scan-secrets.sh" "$WORK/tree/tools/"
}

scan() { bash "$WORK/tree/tools/scan-secrets.sh" "$@" 2>&1; }

expect() { # expect <name> <wanted exit status> <output must contain, or empty> <command...>
  local name=$1 want=$2 contains=$3 out status
  shift 3
  out=$("$@")
  status=$?
  if [ "$status" != "$want" ] || { [ -n "$contains" ] && ! grep -qF -- "$contains" <<<"$out"; }; then
    echo "FAIL: $name (exit $status, wanted $want; output: $out)"
    failures=$((failures + 1))
  fi
}

refuses_to_print() { # refuses_to_print <name> <text that must not be in the output> <command...>
  local name=$1 text=$2 out
  shift 2
  out=$("$@")
  if grep -qF -- "$text" <<<"$out"; then
    echo "FAIL: $name (the output repeats the matched text)"
    failures=$((failures + 1))
  fi
}

fresh
printf '%s\n' 'a harmless line' 'token = a_path' >"$WORK/tree/notes.txt"
expect "a clean tree passes" 0 "secret scan clean" scan "$WORK/tree"

fresh
printf '%s\n' 'name = "x"' 'password = "hunter22x"' >"$WORK/tree/a.conf"
expect "a literal password is reported with its line" 1 "a.conf:2" scan "$WORK/tree"
refuses_to_print "the matched text is never printed" "hunter22x" scan "$WORK/tree"

fresh
printf '%s\n' 'let key = Secret::new("a-literal-value");' >"$WORK/tree/a.rs"
expect "Secret::new with a literal is reported" 1 "a.rs:1" scan "$WORK/tree"

fresh
printf '%s\n' 'let key = Secret::new(value);' 'let other = Secret::new(read_it()?);' >"$WORK/tree/a.rs"
expect "Secret::new with a value that is not a literal passes" 0 "secret scan clean" scan "$WORK/tree"

fresh
printf '%s\n' 'let key = Secret::new(value); let pw = "x"; password = "hunter22x";' >"$WORK/tree/a.rs"
expect "a wrapper does not hide another match on its line" 1 "a.rs:1" scan "$WORK/tree"

fresh
printf '%s\n' 'x' >"$WORK/tree/home.key"
expect "a key file is reported by name" 1 "home.key" scan "$WORK/tree"

fresh
mkdir -p "$WORK/tree/etc/secrets"
printf '%s\n' 'x' >"$WORK/tree/etc/secrets/provider.ini"
expect "an ini file under secrets is reported" 1 "provider.ini" scan "$WORK/tree"

fresh
printf '%s\n' 'x' >"$WORK/tree/provider.ini"
expect "an ini file elsewhere is not a secret" 0 "secret scan clean" scan "$WORK/tree"

fresh
# Joined here, so that this file holds no token-shaped string of its own.
printf '%s_%s\n' ghp 0123456789abcdefghijABCDEFGHIJ012345 >"$WORK/tree/t.txt"
expect "a token shape is reported" 1 "t.txt:1" scan "$WORK/tree"

fresh
printf '%s\n' 'password = "hunter22x"' >"$WORK/tree/a.conf"
scan --list "$WORK/tree" >"$WORK/list.txt"
expect "--list prints a path and a hash" 0 "a.conf " cat "$WORK/list.txt"
cp "$WORK/list.txt" "$WORK/tree/tools/scan-secrets.allow"
expect "a listed line is allowed" 0 "secret scan clean" scan "$WORK/tree"
printf '%s\n' 'password = "hunter33y"' >"$WORK/tree/a.conf"
expect "an edited line is reported again" 1 "a.conf:1" scan "$WORK/tree"

fresh
printf '%s\n' 'password = "hunter22x"' >"$WORK/tree/a.conf"
printf '%s\n' '# a comment' 'a.conf 0000000000000000000000000000000000000000000000000000000000000000' >"$WORK/tree/tools/scan-secrets.allow"
expect "a wrong hash allows nothing" 1 "a.conf:1" scan "$WORK/tree"

if [ "$failures" -ne 0 ]; then
  echo "$failures scan-secrets check(s) failed" >&2
  exit 1
fi
echo "scan-secrets checks pass"
