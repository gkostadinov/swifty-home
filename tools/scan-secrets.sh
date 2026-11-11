#!/usr/bin/env bash
# Exits non-zero if any file under the given path looks like it holds a credential.
# Prints file names and line numbers only, never the matched text.
#
# What it looks for: well-known token shapes, a credential-like word assigned a literal value
# ("password = ..."), a bare "token" with a long literal value, and files named like key
# material. A call that wraps a value that is not a literal, such as `Secret::new(value)`,
# assigns nothing and is not reported; `Secret::new("literal")` still is.
#
# tools/scan-secrets.allow lists reviewed false positives as "<repo path> <sha256 of the exact line>".
# Editing an allowed line, or adding a new match anywhere, fails the scan again. To regenerate the
# list after a cleanup, run `tools/scan-secrets.sh --list .`, read every line it prints, and add
# the ones that are not credentials; `--list` prints each match as "<repo path> <sha256>" and
# changes nothing. tools/scan-secrets-test.sh checks this script against sample files.
set -u
LIST=
if [ "${1:-}" = "--list" ]; then
  LIST=1
  shift
fi
TARGET=${1:-.}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
ALLOW=$ROOT/tools/scan-secrets.allow

TOKENS='-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|AKIA[0-9A-Z]{16}|xox[baprs]-[A-Za-z0-9-]{10,}|eyJ[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}|[a-z0-9._-]+@(pam|pve)![A-Za-z0-9_-]+=[0-9a-fA-F-]{36}|PVEAPIToken=[A-Za-z0-9._@!=:-]{20,}|Bearer [A-Za-z0-9._-]{20,}'
ASSIGN="(^|[^a-z0-9_-])(pass(word|phrase|wd)?|psk|secret|api[_-]?key|auth[_-]?token|authpass|privpass|community|cipassword|dns_cloudflare_api_token|cf_token)[\"']?[[:space:]]*[:=][[:space:]]*[\"']?[^[:space:]\"'\$<{(]{4,}"
# A bare "token" only counts with a long literal value, so that a path or a variable name does not match.
BARE_TOKEN="(^|[^a-z0-9_-])token[\"']?[[:space:]]*[:=][[:space:]]*[\"']?[A-Za-z0-9_-]{20,}([\"'[:space:]]|\$)"

scan() {
  grep -rInE --exclude-dir=.git --exclude='scan-secrets*.sh' "$@" "$TARGET" 2>/dev/null | cut -d: -f1,2
}

# Whether a line is only a wrapper around a value that is not a literal.
benign() {
  local cleaned
  cleaned=$(printf '%s\n' "$1" | sed -E "s/Secret::new\\(([^\"'])/Wrapped(\\1/g")
  [ "$cleaned" != "$1" ] || return 1
  printf '%s\n' "$cleaned" | grep -qE -e "$TOKENS" && return 1
  printf '%s\n' "$cleaned" | grep -qiE -e "$ASSIGN" && return 1
  printf '%s\n' "$cleaned" | grep -qiE -e "$BARE_TOKEN" && return 1
  return 0
}

hits=$( { scan -e "$TOKENS"; scan -i -e "$ASSIGN"; scan -i -e "$BARE_TOKEN"; } | sort -u )

if [ -n "$hits" ]; then
  kept=
  while IFS= read -r hit; do
    file=${hit%:*}
    line=${hit##*:}
    rel=$(realpath --relative-to="$ROOT" "$file")
    text=$(sed -n "${line}p" "$file" | tr -d '\r')
    benign "$text" && continue
    sum=$(printf '%s\n' "$text" | sha256sum | cut -d' ' -f1)
    if [ -n "$LIST" ]; then
      echo "$rel $sum"
      continue
    fi
    { [ -f "$ALLOW" ] && grep -qxF "$rel $sum" "$ALLOW"; } || kept="$kept$hit"$'\n'
  done <<< "$hits"
  hits=${kept%$'\n'}
fi
[ -n "$LIST" ] && exit 0

# Files named like key material, wherever they are. A service's own file name is not listed:
# a login or token file is `*.login` or `*.token`, and an ini file under a `secrets` directory is a secret.
named=$(find "$TARGET" -path '*/.git' -prune -o -type f \( -name '*.key' -o -name '*.pem' -o -name 'id_rsa*' -o -name 'id_ed25519*' -o -name '*_ed25519' -o -name 'wpa_supplicant*.conf' -o -name '*.token' -o -name '*.login' -o -path '*/secrets/*.ini' \) -print 2>/dev/null)

if [ -n "$hits$named" ]; then
  echo "SECRET SCAN FAILED under $TARGET:" >&2
  [ -n "$hits" ] && echo "$hits" >&2
  [ -n "$named" ] && echo "$named" >&2
  exit 1
fi
echo "secret scan clean: $TARGET"
