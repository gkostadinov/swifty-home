#!/usr/bin/env bash
# Checks the user documentation against a built `swifty` binary.
#
#   bash docs/doctest.sh [--binary FILE] [--strict] [--keep] [--verbose]
#
# 1. Links. Every relative link in the user docs must point at a file that exists,
#    and an anchor (#section) at a heading that exists in it.
# 2. Commands. Every `swifty ...` line inside a ```sh block is run, in the order of
#    its page, in a scratch hub of its own (one per page): paths under
#    /etc/swifty-home, /var/lib/swifty-home, /var/backups/swifty-home and
#    /etc/glimdock-home point into the scratch directory, `sudo` runs the command as
#    it is, `--config` defaults to the scratch configuration, the port 8790 becomes a
#    free one, and `swifty serve` runs in the background until the page is done.
#    `curl` lines against localhost are run too. Every other line (systemctl, docker,
#    cargo, the install script) needs root, systemd, Docker or a release, and is
#    listed as not run.
# 3. Inline commands. Every `swifty <command>` written inline must be a command the
#    binary knows: `swifty <command> --help` must succeed.
#
# A command that answers "not available in this build yet" is reported as pending,
# not failed, unless --strict is given (CI gives it: every command exists now). A few
# commands cannot succeed in a scratch hub
# (they need a real device, a vendor account, an answer typed at the terminal or a
# real glimdock-home install): for those a clean refusal (an `error:` line, exit 1)
# passes, and a usage error or a crash fails.
#
# Exit status: 0 when nothing failed, 1 otherwise. Needs bash, curl, sed, awk, grep.

set -u

ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=""
STRICT=0
KEEP=0
VERBOSE=0

usage() {
    sed -n '2,27p' "$0" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
    case $1 in
        --binary) BIN=$2; shift 2 ;;
        --strict) STRICT=1; shift ;;
        --keep) KEEP=1; shift ;;
        --verbose) VERBOSE=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "doctest: unknown option $1" >&2; usage >&2; exit 2 ;;
    esac
done

# ---- The pages this script checks: the user documentation, nothing else.
PAGES=()
for page in README.md CHANGELOG.md packaging/README.md \
    docs/install.md docs/first-run.md docs/tls.md docs/configuration.md \
    docs/plugins-user.md docs/security.md docs/architecture.md docs/upgrade.md \
    docs/backup.md docs/roadmap.md docs/demo.md \
    docs/plugins/README.md docs/plugins/how-to.md docs/plugins/contract.md \
    docs/plugins/testing.md docs/plugins/ai-prompt.md docs/plugins/bridge.md \
    docs/api/README.md docs/api/untyped.md; do
    [ -f "$ROOT/$page" ] && PAGES+=("$page")
done
for dir in install first-run tls configuration plugins-user security architecture upgrade backup roadmap; do
    if [ -d "$ROOT/docs/$dir" ]; then
        while IFS= read -r page; do PAGES+=("${page#"$ROOT"/}"); done \
            < <(find "$ROOT/docs/$dir" -name '*.md' | sort)
    fi
done

# Files that are still to be added. A link to one of them is pending until it exists. Nothing
# is expected any more: every page the user docs link to is in the repository.
EXPECTED_ELSEWHERE=""

# ---- The binary.
if [ -z "$BIN" ]; then
    target=${CARGO_TARGET_DIR:-$ROOT/target}
    for candidate in "$target/debug/swifty" "$target/debug/swifty.exe" \
        "$target/release/swifty" "$target/release/swifty.exe"; do
        if [ -x "$candidate" ] && { [ -z "$BIN" ] || [ "$candidate" -nt "$BIN" ]; }; then
            BIN=$candidate
        fi
    done
    if [ -z "$BIN" ]; then
        echo "doctest: building swifty (cargo build -p swifty)"
        (cd "$ROOT" && cargo build -p swifty --locked) || exit 1
        for candidate in "$target/debug/swifty" "$target/debug/swifty.exe"; do
            [ -x "$candidate" ] && BIN=$candidate
        done
    fi
fi
if [ -z "$BIN" ] || [ ! -x "$BIN" ]; then
    echo "doctest: no swifty binary found; give one with --binary FILE" >&2
    exit 1
fi
BIN=$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")

WORK=$(mktemp -d "${TMPDIR:-/tmp}/swifty-doctest.XXXXXX")
PORT=$((18000 + RANDOM % 2000))

FAILED=0
PENDING=0
OK=0
REFUSED=0
LINKS=0
FAILURES=()
PENDINGS=()
NOT_RUN=()

fail() { FAILED=$((FAILED + 1)); FAILURES+=("$1"); }
pending() { PENDING=$((PENDING + 1)); PENDINGS+=("$1"); }

stop_servers() {
    local pidfile=$1
    [ -f "$pidfile" ] || return 0
    while read -r pid; do
        kill "$pid" 2>/dev/null
        wait "$pid" 2>/dev/null
    done < "$pidfile"
    rm -f "$pidfile"
}

cleanup() {
    for pidfile in "$WORK"/*/pids; do
        [ -f "$pidfile" ] && stop_servers "$pidfile"
    done
    if [ "$KEEP" = 1 ]; then
        echo "doctest: scratch files kept in $WORK"
    else
        rm -rf "$WORK"
    fi
}
trap cleanup EXIT

# ---- 1. Links.

# The anchors a Markdown file offers, as GitHub makes them from its headings.
anchors_of() {
    awk '/^[ \t]*```/ { fence = !fence; next } !fence && /^#+ / { print }' "$1" |
        sed 's/^#* *//' | tr 'A-Z' 'a-z' | sed 's/[^a-z0-9 _-]//g; s/ /-/g'
}

# A path relative to the repository, with `.` and `..` worked out.
normalize() {
    echo "$1" | awk -F/ '{
        n = 0
        for (i = 1; i <= NF; i++) {
            if ($i == "" || $i == ".") continue
            if ($i == "..") { if (n > 0) n--; continue }
            part[++n] = $i
        }
        out = ""
        for (i = 1; i <= n; i++) out = out (i > 1 ? "/" : "") part[i]
        print out
    }'
}

check_links() {
    local page=$1 dir link path anchor target
    dir=$(dirname "$ROOT/$page")
    while IFS= read -r link; do
        link=${link#](}
        link=${link%)}
        link=${link%% *}
        case $link in
            http://*|https://*|mailto:*|'') continue ;;
        esac
        LINKS=$((LINKS + 1))
        path=${link%%#*}
        anchor=""
        case $link in *'#'*) anchor=${link#*#} ;; esac
        if [ -z "$path" ]; then
            target="$ROOT/$page"
        else
            target="$dir/$path"
        fi
        if [ ! -e "$target" ]; then
            local relative
            relative=$(normalize "$(dirname "$page")/$path")
            if ! echo " $EXPECTED_ELSEWHERE " | grep -q " $relative "; then
                fail "$page: broken link to $link"
            else
                pending "$page: link to $relative (added by another package)"
            fi
            continue
        fi
        if [ -n "$anchor" ] && [ -f "$target" ]; then
            if ! anchors_of "$target" | grep -qx -- "$anchor"; then
                fail "$page: link to $link names a heading that does not exist"
            fi
        fi
    done < <(awk '/^[ \t]*```/ { fence = !fence; next } !fence' "$ROOT/$page" |
        grep -o '\]([^)]*)')
}

for page in "${PAGES[@]}"; do
    check_links "$page"
done

# ---- 2. Commands in ```sh blocks.

# The lines of every ```sh block of a page, without their indentation.
sh_lines() {
    awk '
        /^[ \t]*```sh[ \t]*$/ { inside = 1; next }
        /^[ \t]*```/ { inside = 0; next }
        inside { sub(/^[ \t]+/, ""); if ($0 != "") print }
    ' "$ROOT/$1"
}

# What a line runs, once `sudo` is set aside.
command_of() {
    local line=$1
    line=${line#sudo }
    line=${line/#.\/target\/release\/swifty /swifty }
    echo "$line"
}

# Lines that cannot succeed in a scratch hub, and why. A clean refusal passes for them.
may_refuse() {
    case $1 in
        "swifty plugin add mock"*--yes*) return 1 ;;
        "swifty plugin add "*|"swifty plugin setup "*)
            echo "it needs answers at the terminal, and a real device for most plugins" ;;
        "swifty plugin remove "*)
            case $1 in *--yes*) echo "the integration may not exist in a scratch hub" ;;
                *) echo "it asks before removing" ;; esac ;;
        "swifty plugin secret set "*) echo "the integration does not exist in a scratch hub" ;;
        "swifty migrate "*) echo "it needs a real glimdock-home install" ;;
        *) return 1 ;;
    esac
    return 0
}

make_scratch() {
    local env=$1
    mkdir -p "$env/backups" "$env/legacy" "$env/work" "$env/bin"
    # The old glimdock-home format, as `swifty migrate --from` reads it.
    local legacy_example="$ROOT/crates/swifty/tests/fixtures/legacy-config.example.toml"
    [ -f "$legacy_example" ] || legacy_example="$ROOT/packaging/swifty.example.toml"
    cp "$legacy_example" "$env/legacy/config.toml"
    printf 'doctest-speech-token-0123456789\n' > "$env/work/speech-token.txt"
    printf 'doctest-assistant-key-0123456789\n' > "$env/work/assistant-key.txt"
    printf 'doctesthuekey0123456789abcdef\n' > "$env/work/hue-key.txt"
    printf 'site_version = 1\n' > "$env/work/site.toml"

    cat > "$env/bin/sudo" <<'EOF'
#!/usr/bin/env bash
exec "$@"
EOF
    cat > "$env/bin/swifty" <<EOF
#!/usr/bin/env bash
# The documented command, run against the scratch hub.
REAL='$BIN'
ENV='$env'
PORT='$PORT'
EOF
    cat >> "$env/bin/swifty" <<'EOF'
args=("$@")
has_config=0
has_origin=0
sub=""
skip=0
for arg in "$@"; do
    if [ "$skip" = 1 ]; then skip=0; continue; fi
    case $arg in
        --config) has_config=1; skip=1 ;;
        --config=*) has_config=1 ;;
        --origin|--origin=*) has_origin=1 ;;
        -*) ;;
        *) [ -z "$sub" ] && sub=$arg ;;
    esac
done
[ "$has_config" = 0 ] && args=(--config "$ENV/etc/swifty.toml" "${args[@]}")
if [ "$sub" = init ] && [ "$has_origin" = 0 ]; then
    args+=(--origin "http://localhost:$PORT")
fi
if [ "$sub" = serve ]; then
    "$REAL" "${args[@]}" > "$ENV/serve.log" 2>&1 &
    echo $! >> "$ENV/pids"
    for _ in $(seq 1 40); do
        if curl -fsS "http://127.0.0.1:$PORT/healthz" > /dev/null 2>&1; then
            echo "swifty serve is running on 127.0.0.1:$PORT"
            exit 0
        fi
        sleep 0.5
    done
    echo "swifty serve did not answer on 127.0.0.1:$PORT" >&2
    cat "$ENV/serve.log" >&2
    exit 1
fi
exec "$REAL" "${args[@]}"
EOF
    chmod +x "$env/bin/sudo" "$env/bin/swifty"

    # A hub to run commands against, unless the page sets one up itself.
    "$BIN" --config "$env/etc/swifty.toml" init --yes --origin "http://localhost:$PORT" \
        --data-dir "$env/data" > "$env/init.log" 2>&1 ||
        { echo "doctest: the scratch hub could not be set up:" >&2; cat "$env/init.log" >&2; exit 1; }
}

rewrite() {
    local env=$1 line=$2
    line=${line//.\/target\/release\/swifty/swifty}
    line=${line//\/etc\/swifty-home/$env/etc}
    line=${line//\/var\/lib\/swifty-home/$env/data}
    line=${line//\/var\/backups\/swifty-home/$env/backups}
    line=${line//\/etc\/glimdock-home/$env/legacy}
    line=${line//localhost:8790/localhost:$PORT}
    line=${line//127.0.0.1:8790/127.0.0.1:$PORT}
    echo "$line"
}

run_page() {
    local page=$1 number=$2 env line command rewritten rc out reason url code
    local lines=()
    while IFS= read -r line; do lines+=("$line"); done < <(sh_lines "$page")
    [ ${#lines[@]} -gt 0 ] || return 0

    env="$WORK/page$number"
    local scratch_made=0
    for line in "${lines[@]}"; do
        command=$(command_of "$line")
        case $command in
            "swifty "*|"curl http://localhost"*|"curl http://127.0.0.1"*) ;;
            *)
                NOT_RUN+=("$line")
                continue ;;
        esac
        if [ "$scratch_made" = 0 ]; then
            make_scratch "$env"
            scratch_made=1
        fi
        rewritten=$(rewrite "$env" "$line")
        out="$env/out"
        if [ "${command%% *}" = curl ]; then
            url=$(echo "$rewritten" | grep -o 'http://[^ ]*' | head -n 1)
            code=$(curl -s -o "$out" -w '%{http_code}' "$url")
            if [ "$code" = 200 ]; then
                OK=$((OK + 1))
                [ "$VERBOSE" = 1 ] && echo "ok       $page: $line"
            else
                fail "$page: $line answered HTTP $code"
            fi
            continue
        fi
        (cd "$env/work" && PATH="$env/bin:$PATH" bash -c "$rewritten" < /dev/null) > "$out" 2>&1
        rc=$?
        if grep -q "not available in this build yet" "$out"; then
            pending "$page: $line"
        elif grep -q "panicked" "$out"; then
            fail "$page: $line crashed: $(grep -m1 panicked "$out")"
        elif [ "$rc" = 0 ]; then
            OK=$((OK + 1))
            [ "$VERBOSE" = 1 ] && echo "ok       $page: $line"
        elif [ "$rc" = 2 ]; then
            fail "$page: $line is not a valid command line: $(grep -m1 -i error "$out")"
        elif reason=$(may_refuse "$command") && grep -q '^error:' "$out"; then
            REFUSED=$((REFUSED + 1))
            [ "$VERBOSE" = 1 ] && echo "refused  $page: $line ($reason): $(grep -m1 '^error:' "$out")"
        else
            fail "$page: $line ended with exit code $rc: $(head -n 3 "$out" | tr '\n' ' ')"
        fi
    done
    stop_servers "$env/pids"
}

number=0
for page in "${PAGES[@]}"; do
    number=$((number + 1))
    run_page "$page" "$number"
done

# ---- 3. Inline `swifty <command>` mentions name real commands.
INLINE=0
while IFS= read -r words; do
    INLINE=$((INLINE + 1))
    # shellcheck disable=SC2086 # The words are meant to be split.
    if ! "$BIN" $words --help > /dev/null 2>&1; then
        fail "inline command \`swifty $words\` is not a command of this binary"
    fi
done < <(
    for page in "${PAGES[@]}"; do
        grep -o '`\(sudo \)\{0,1\}swifty [^`]*`' "$ROOT/$page"
    done | sed 's/^`//; s/`$//; s/^sudo //; s/^swifty //' |
        awk '{ out = ""; for (i = 1; i <= NF; i++) { if ($i !~ /^[a-z][a-z-]*$/) break; out = out (out == "" ? "" : " ") $i } if (out != "") print out }' |
        sort -u
)

# ---- Report.
echo "Swifty Home doc test, with $BIN"
echo "  pages:    ${#PAGES[@]}"
echo "  links:    $LINKS checked"
echo "  inline:   $INLINE distinct commands checked against --help"
echo "  commands: $OK ran, $REFUSED refused as expected in a scratch hub, $PENDING pending, $FAILED failed"
if [ ${#NOT_RUN[@]} -gt 0 ]; then
    echo
    echo "Not run here (they need root, systemd, Docker, a release or the network):"
    printf '%s\n' "${NOT_RUN[@]}" | sort -u | sed 's/^/  /'
fi
if [ "$PENDING" -gt 0 ]; then
    echo
    echo "Pending (not in this build yet):"
    printf '  %s\n' "${PENDINGS[@]}"
fi
if [ "$FAILED" -gt 0 ]; then
    echo
    echo "FAILED:"
    printf '  %s\n' "${FAILURES[@]}"
    exit 1
fi
if [ "$STRICT" = 1 ] && [ "$PENDING" -gt 0 ]; then
    echo
    echo "FAILED: --strict, and $PENDING item(s) are pending."
    exit 1
fi
echo
echo "OK"
