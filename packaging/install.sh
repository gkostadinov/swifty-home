#!/bin/sh
# Installs Swifty Home on Linux (x86_64, or aarch64 such as a Raspberry Pi 4 or 5).
#
#   curl -fsSL https://github.com/gkostadinov/swifty-home/releases/latest/download/install.sh | sh
#
# What it does, in this order:
#   1. Finds this machine's architecture and downloads the matching release archive.
#   2. Checks the archive's SHA-256 against the release's SHA256SUMS, and checks the
#      signature on SHA256SUMS. Nothing is installed from a file that fails either check.
#   3. Shows what it is about to change and asks before it changes anything.
#   4. Installs /usr/local/bin/swifty, creates the system user `swifty-home` and the
#      directories /etc/swifty-home, /var/lib/swifty-home and /var/backups/swifty-home,
#      and installs and enables swifty-home.service and the nightly backup timer.
#   5. Runs `swifty init`, which asks for the address people will use and writes
#      /etc/swifty-home/swifty.toml. An existing swifty.toml is never overwritten.
#   6. Starts the service and says what to do next.
#
# The hub starts in dry-run mode: it reaches no device until you run `sudo swifty go-live`.
# Run with --dry-run to see every command without running any. Run with --help for options.
#
# POSIX sh on purpose: `curl ... | sh` runs it with dash on Debian and Ubuntu.
#
# The whole script is one { ... } group, closed on its last line. The shell reads a group
# completely before running any of it, so a download cut off half way runs nothing.
{

set -eu
# Pipelines fail when any part fails, in every shell that has the option.
# shellcheck disable=SC3040 # Guarded: only set where the shell supports it.
if (set -o pipefail) 2>/dev/null; then set -o pipefail; fi

# The release workflow fills these in for the copy it attaches to each release. In a
# checkout they stay as they are, and --archive or --base-url says where to install from.
RELEASE_REPO='@REPO@'
RELEASE_VERSION='@VERSION@'
RELEASE_SIGNING_KEY='@SIGNING_KEY@'

SIGN_NAMESPACE=swifty-home-release
SERVICE_USER=swifty-home
BIN=/usr/local/bin/swifty
CONFIG_DIR=/etc/swifty-home
CONFIG=$CONFIG_DIR/swifty.toml
DATA_DIR=/var/lib/swifty-home
BACKUP_DIR=/var/backups/swifty-home
SHARE_DIR=/usr/local/share/doc/swifty-home
UNIT_DIR=/etc/systemd/system
UNITS="swifty-home.service swifty-home-backup.service swifty-home-backup.timer"
PAYLOAD="swifty swifty-home.service swifty-home-backup.service swifty-home-backup.timer swifty.example.toml LICENSE NOTICE THIRD-PARTY-NOTICES.txt"

usage() {
    cat <<'EOF'
Installs Swifty Home, a self-hosted smart home hub.

Usage: install.sh [options]
       curl -fsSL <release>/install.sh | sh -s -- [options]

  --version VERSION   Install this version (default: the release this script came with).
  --archive FILE      Install from a local swifty-home-<version>-<arch>-linux.tar.gz. Its
                      SHA256SUMS must be in the same directory.
  --base-url URL      Download from this https:// directory instead of the GitHub release.
  --origin URL        The address people will use, passed to `swifty init`
                      (default: init asks; with --yes, http://localhost:8790).
  --demo, --no-demo   Start with the demo home of pretend devices, or an empty one.
                      Without either, it asks; with --yes, the home starts empty.
  --yes               Do not ask. Accept the plan and every default.
  --no-start          Install and configure, but do not start the service.
  --no-init           Do not run `swifty init` (for example before `swifty migrate`).
  --dry-run           Download and check, then print every command instead of running it.
  --skip-signature    Trust SHA256SUMS without checking its signature. Not recommended.
  --uninstall         Remove the program and the service. Configuration and data are kept.
  --purge             With --uninstall: also delete the configuration, the data, the
                      backups and the swifty-home user. This cannot be undone.
  -h, --help          Show this text.
EOF
}

say() { printf '%s\n' "$*"; }
warn() { printf 'install.sh: warning: %s\n' "$*" >&2; }
die() {
    printf 'install.sh: %s\n' "$*" >&2
    exit 1
}

# --- Options ----------------------------------------------------------------------------

version=""
archive=""
base_url=""
origin=""
demo=""
assume_yes=no
no_start=no
no_init=no
dry_run=no
skip_signature=no
uninstall=no
purge=no

need_value() {
    [ "$#" -ge 2 ] || die "$1 needs a value. See --help."
}

while [ "$#" -gt 0 ]; do
    case $1 in
        --version)
            need_value "$@"
            version=$2
            shift 2
            ;;
        --version=*)
            version=${1#*=}
            shift
            ;;
        --archive)
            need_value "$@"
            archive=$2
            shift 2
            ;;
        --archive=*)
            archive=${1#*=}
            shift
            ;;
        --base-url)
            need_value "$@"
            base_url=$2
            shift 2
            ;;
        --base-url=*)
            base_url=${1#*=}
            shift
            ;;
        --origin)
            need_value "$@"
            origin=$2
            shift 2
            ;;
        --origin=*)
            origin=${1#*=}
            shift
            ;;
        --demo)
            demo=yes
            shift
            ;;
        --no-demo)
            demo=no
            shift
            ;;
        --yes | -y)
            assume_yes=yes
            shift
            ;;
        --no-start)
            no_start=yes
            shift
            ;;
        --no-init)
            no_init=yes
            shift
            ;;
        --dry-run)
            dry_run=yes
            shift
            ;;
        --skip-signature)
            skip_signature=yes
            shift
            ;;
        --uninstall)
            uninstall=yes
            shift
            ;;
        --purge)
            purge=yes
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            die "unknown option: $1. See --help."
            ;;
    esac
done

if [ "$purge" = yes ] && [ "$uninstall" = no ]; then
    die "--purge only goes with --uninstall."
fi
if [ -n "$archive" ] && [ -n "$base_url" ]; then
    die "use --archive or --base-url, not both."
fi

# --- Helpers ----------------------------------------------------------------------------

has() { command -v "$1" >/dev/null 2>&1; }

# A tool that may live in /usr/sbin or /sbin, which an ordinary user's PATH can lack.
find_sbin() {
    if has "$1"; then
        command -v "$1"
        return 0
    fi
    for dir in /usr/sbin /sbin /usr/bin /bin; do
        if [ -x "$dir/$1" ]; then
            printf '%s\n' "$dir/$1"
            return 0
        fi
    done
    return 1
}

# True when there is a terminal to ask questions on, even with the script on a pipe.
has_tty() { (exec </dev/tty) 2>/dev/null; }

if [ "$(id -u)" -eq 0 ]; then
    SUDO=""
elif has sudo; then
    SUDO=sudo
else
    SUDO=none
fi

# Runs one command that changes the system, as root. With --dry-run it only prints it.
run() {
    if [ "$dry_run" = yes ]; then
        shown=""
        for arg in "$@"; do
            case $arg in
                "" | *[!A-Za-z0-9_./:=@%+,-]*) shown="$shown '$arg'" ;;
                *) shown="$shown $arg" ;;
            esac
        done
        printf '    would run:%s\n' "$shown"
        return 0
    fi
    if [ -z "$SUDO" ]; then
        "$@"
    else
        sudo "$@"
    fi
}

# `test`, as root: /etc/swifty-home is not readable by other users.
root_test() {
    if [ -z "$SUDO" ]; then
        test "$@"
    elif [ "$dry_run" = yes ]; then
        sudo -n test "$@" 2>/dev/null || test "$@"
    else
        sudo test "$@"
    fi
}

# Asks a yes-or-no question on the terminal. $2 is the answer to an empty reply.
ask() {
    printf '%s ' "$1" >/dev/tty
    reply=""
    read -r reply </dev/tty || reply=""
    [ -n "$reply" ] || reply=$2
    case $reply in
        y | Y | yes | Yes | YES) return 0 ;;
        *) return 1 ;;
    esac
}

# Asks before a change. --yes accepts; with no terminal to ask on, it stops.
confirm() {
    if [ "$assume_yes" = yes ] || [ "$dry_run" = yes ]; then
        return 0
    fi
    if ! has_tty; then
        die "there is no terminal to ask on. Run it again with --yes to accept the plan, or with --dry-run to only look."
    fi
    ask "$1 [Y/n]" y
}

group_exists() {
    if has getent; then
        getent group "$1" >/dev/null 2>&1
    else
        grep -q "^$1:" /etc/group 2>/dev/null
    fi
}

systemd_running() { has systemctl && [ -d /run/systemd/system ]; }

# Never lets a mistake become `rm -rf /`.
remove_tree() {
    case $1 in
        "" | / | /etc | /var | /var/lib | /var/backups | /usr | /usr/local) die "refusing to remove '$1'." ;;
    esac
    if root_test -e "$1"; then
        run rm -rf -- "$1"
    fi
}

# --- Uninstall --------------------------------------------------------------------------

do_uninstall() {
    if [ "$SUDO" = none ] && [ "$dry_run" = no ]; then
        die "this needs root. Run it as root, or install sudo."
    fi
    say "Removing Swifty Home from this machine."
    say "  - stop and disable swifty-home.service and the backup timer"
    say "  - remove $BIN, the unit files and $SHARE_DIR"
    if [ "$purge" = yes ]; then
        say "  - DELETE $CONFIG_DIR, $DATA_DIR and $BACKUP_DIR, and the user $SERVICE_USER"
        say "    This deletes every passkey, device, room, the audit log and the backups. It cannot be undone."
    else
        say "  - keep $CONFIG_DIR, $DATA_DIR, $BACKUP_DIR and the user $SERVICE_USER"
    fi

    if [ "$purge" = yes ] && [ "$assume_yes" = no ] && [ "$dry_run" = no ]; then
        has_tty || die "--purge deletes data for good. Run it again with --yes to confirm, or from a terminal."
        printf 'Type "delete" to delete the configuration, data and backups for good: ' >/dev/tty
        typed=""
        read -r typed </dev/tty || typed=""
        [ "$typed" = delete ] || die "nothing was changed."
    elif ! confirm "Remove Swifty Home?"; then
        die "nothing was changed."
    fi

    if has systemctl; then
        for unit in swifty-home-backup.timer swifty-home.service; do
            if [ -e "$UNIT_DIR/$unit" ]; then
                run systemctl disable --now "$unit" || warn "could not stop $unit"
            fi
        done
    fi
    for unit in $UNITS; do
        if [ -e "$UNIT_DIR/$unit" ]; then
            run rm -f -- "$UNIT_DIR/$unit"
        fi
    done
    if systemd_running; then
        run systemctl daemon-reload
    fi
    if [ -e "$BIN" ]; then
        run rm -f -- "$BIN"
    fi
    remove_tree "$SHARE_DIR"

    if [ "$dry_run" = yes ] && [ "$purge" = no ]; then
        say "Dry run finished. Nothing on this machine was changed."
        return 0
    fi
    if [ "$purge" = yes ]; then
        remove_tree "$CONFIG_DIR"
        remove_tree "$DATA_DIR"
        remove_tree "$BACKUP_DIR"
        if id -u "$SERVICE_USER" >/dev/null 2>&1; then
            if userdel=$(find_sbin userdel); then
                run "$userdel" "$SERVICE_USER"
            elif has deluser; then
                run deluser "$SERVICE_USER"
            else
                warn "no userdel or deluser here: remove the user $SERVICE_USER by hand."
            fi
        fi
        if group_exists "$SERVICE_USER"; then
            if groupdel=$(find_sbin groupdel); then
                run "$groupdel" "$SERVICE_USER"
            elif has delgroup; then
                run delgroup "$SERVICE_USER"
            fi
        fi
        if [ "$dry_run" = yes ]; then
            say "Dry run finished. Nothing on this machine was changed."
        else
            say "Swifty Home and its data are removed."
        fi
    else
        say "Swifty Home is removed. Your configuration and data are still in $CONFIG_DIR and $DATA_DIR."
        say "To delete them as well: install.sh --uninstall --purge"
    fi
}

if [ "$uninstall" = yes ]; then
    do_uninstall
    exit 0
fi

# --- Where this machine stands ----------------------------------------------------------

[ "$(uname -s)" = Linux ] || die "Swifty Home installs on Linux. On other systems, use the Docker image."

case $(uname -m) in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    armv7l | armv6l | armhf)
        die "this is a 32-bit ARM system. Swifty Home needs a 64-bit system (aarch64): on a Raspberry Pi 4 or 5, install the 64-bit Raspberry Pi OS."
        ;;
    *) die "there is no Swifty Home build for $(uname -m). Builds exist for x86_64 and aarch64." ;;
esac

if [ "$SUDO" = none ] && [ "$dry_run" = no ]; then
    die "this needs root. Run it as root, for example: curl -fsSL <release>/install.sh | sudo sh"
fi

for tool in tar gzip mktemp awk install; do
    has "$tool" || die "this needs '$tool', which is not installed."
done

sha256_of() {
    if has sha256sum; then
        sha256sum "$1" | awk '{ print $1 }'
    elif has shasum; then
        shasum -a 256 "$1" | awk '{ print $1 }'
    elif has openssl; then
        openssl dgst -sha256 -r "$1" | awk '{ print $1 }'
    else
        die "this needs sha256sum, shasum or openssl to check the download."
    fi
}

fetch() {
    case $1 in
        https://*) ;;
        *) die "refusing to download over anything but https: $1" ;;
    esac
    if has curl; then
        curl -fsSL --proto '=https' --tlsv1.2 --retry 3 -o "$2" "$1" || die "could not download $1"
    elif has wget; then
        wget -q -O "$2" "$1" || die "could not download $1"
    else
        die "this needs curl or wget to download the release."
    fi
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# --- Get the archive and check it -------------------------------------------------------

# A placeholder the release workflow did not fill in still looks like @NAME@.
filled() {
    case $1 in
        @*@) return 1 ;;
        *) return 0 ;;
    esac
}

if [ -n "$archive" ]; then
    [ -f "$archive" ] || die "no file at $archive"
    name=$(basename "$archive")
    case $name in
        swifty-home-*-linux.tar.gz) ;;
        *) die "expected a file named swifty-home-<version>-<arch>-linux.tar.gz, got $name" ;;
    esac
    rest=${name#swifty-home-}
    rest=${rest%-linux.tar.gz}
    archive_arch=${rest##*-}
    archive_version=${rest%-*}
    [ "$archive_arch" = "$arch" ] || die "$name is for $archive_arch, and this machine is $arch."
    if [ -n "$version" ] && [ "${version#v}" != "$archive_version" ]; then
        die "$name is version $archive_version, not ${version#v}."
    fi
    version=$archive_version
    source_dir=$(dirname "$archive")
    [ -f "$source_dir/SHA256SUMS" ] || die "no SHA256SUMS next to $name. \`cargo xtask dist\` writes one beside each archive."
    cp "$archive" "$tmp/$name"
    cp "$source_dir/SHA256SUMS" "$tmp/SHA256SUMS"
    if [ -f "$source_dir/SHA256SUMS.sig" ]; then
        cp "$source_dir/SHA256SUMS.sig" "$tmp/SHA256SUMS.sig"
    fi
    from="$archive"
else
    if [ -z "$version" ]; then
        filled "$RELEASE_VERSION" || die "this copy of install.sh is not from a release: say which version with --version, or use --archive."
        version=$RELEASE_VERSION
    fi
    version=${version#v}
    case $version in
        "" | *[!0-9A-Za-z.+-]*) die "not a version: $version" ;;
    esac
    if [ -z "$base_url" ]; then
        filled "$RELEASE_REPO" || die "this copy of install.sh is not from a release: use --base-url or --archive."
        base_url="https://github.com/$RELEASE_REPO/releases/download/v$version"
    fi
    base_url=${base_url%/}
    name="swifty-home-$version-$arch-linux.tar.gz"
    say "Downloading Swifty Home $version for $arch from $base_url"
    fetch "$base_url/$name" "$tmp/$name"
    fetch "$base_url/SHA256SUMS" "$tmp/SHA256SUMS"
    if [ "$skip_signature" = no ]; then
        fetch "$base_url/SHA256SUMS.sig" "$tmp/SHA256SUMS.sig"
    fi
    from="$base_url/$name"
fi

# The signature on SHA256SUMS: required for a download, checked for a local archive when
# a signature sits next to it.
if [ "$skip_signature" = yes ]; then
    warn "not checking the signature on SHA256SUMS (--skip-signature). Only the hash is checked."
elif [ -f "$tmp/SHA256SUMS.sig" ]; then
    filled "$RELEASE_SIGNING_KEY" || die "this copy of install.sh has no release key to check the signature with. Use the install.sh from the release, or --skip-signature if you trust the source."
    has ssh-keygen || die "checking the release signature needs ssh-keygen (package openssh-client). Install it and run this again."
    printf '%s namespaces="%s" %s\n' "$SIGN_NAMESPACE" "$SIGN_NAMESPACE" "$RELEASE_SIGNING_KEY" >"$tmp/allowed_signers"
    if ! ssh-keygen -Y verify -f "$tmp/allowed_signers" -I "$SIGN_NAMESPACE" -n "$SIGN_NAMESPACE" \
        -s "$tmp/SHA256SUMS.sig" <"$tmp/SHA256SUMS" >/dev/null 2>&1; then
        die "the signature on SHA256SUMS does not verify. Nothing was installed."
    fi
    say "  The signature on SHA256SUMS is good."
else
    say "  No signature next to the archive: checking its hash only (fine for an archive you built yourself)."
fi

expected=$(awk -v f="$name" '$2 == f || $2 == "*" f { print $1; exit }' "$tmp/SHA256SUMS")
case $expected in
    "" | *[!0-9a-f]*) die "SHA256SUMS has no entry for $name." ;;
esac
[ "${#expected}" -eq 64 ] || die "SHA256SUMS has a malformed entry for $name."
actual=$(sha256_of "$tmp/$name")
[ "$actual" = "$expected" ] || die "$name does not match SHA256SUMS (expected $expected, got $actual). Nothing was installed."
say "  The archive matches SHA256SUMS."

mkdir "$tmp/x"
tar -xzf "$tmp/$name" -C "$tmp/x"
stage="$tmp/x/swifty-home-$version-$arch-linux"
for file in $PAYLOAD; do
    [ -f "$stage/$file" ] || die "the archive has no $file."
done
"$stage/swifty" version >"$tmp/new-version" 2>&1 || die "the swifty in the archive does not run on this machine."
new_version=$(awk 'NR == 1' "$tmp/new-version")

# --- The plan ---------------------------------------------------------------------------

old_version=""
if [ -x "$BIN" ]; then
    if "$BIN" version >"$tmp/old-version" 2>&1; then
        old_version=$(awk 'NR == 1' "$tmp/old-version")
    fi
    [ -n "$old_version" ] || old_version="an older version"
fi
have_user=no
if id -u "$SERVICE_USER" >/dev/null 2>&1; then have_user=yes; fi
have_config=no
if root_test -e "$CONFIG"; then have_config=yes; fi
was_running=no
if systemd_running && systemctl is-active --quiet swifty-home.service; then was_running=yes; fi

say ""
say "Swifty Home: ${new_version:-version $version} for $arch, from $from"
say ""
say "This will:"
if [ -n "$old_version" ]; then
    say "  - replace $BIN ($old_version)"
else
    say "  - install $BIN"
fi
if [ "$have_user" = no ]; then say "  - create the system user and group $SERVICE_USER"; fi
say "  - create $CONFIG_DIR, $DATA_DIR and $BACKUP_DIR where they are missing"
if has systemctl; then
    say "  - install and enable swifty-home.service and a nightly backup (swifty-home-backup.timer)"
else
    say "  - copy the systemd units to $SHARE_DIR/systemd (no systemd here, so nothing is enabled)"
fi
if [ "$have_config" = yes ]; then
    say "  - keep your configuration in $CONFIG (it is never overwritten)"
elif [ "$no_init" = yes ]; then
    say "  - not run swifty init (--no-init)"
else
    say "  - run swifty init: it asks for the address people will use and writes $CONFIG"
fi
if [ "$no_start" = yes ]; then
    say "  - not start the service (--no-start)"
elif systemd_running; then
    say "  - start the service in dry-run mode: it reaches no device until you run 'sudo swifty go-live'"
fi
say ""

if [ "$dry_run" = yes ]; then
    say "Dry run: these are the commands it would run."
elif ! confirm "Go ahead?"; then
    die "nothing was changed."
fi

if [ "$have_config" = no ] && [ "$no_init" = no ] && [ -z "$demo" ]; then
    if [ "$assume_yes" = yes ] || [ "$dry_run" = yes ] || ! has_tty; then
        demo=no
    elif ask "Start with the demo home, a set of pretend devices to try things on? [Y/n]" y; then
        demo=yes
    else
        demo=no
    fi
fi

# --- Install ----------------------------------------------------------------------------

say "Installing."

if [ "$have_user" = no ]; then
    nologin=$(find_sbin nologin) || nologin=/bin/false
    if useradd=$(find_sbin useradd); then
        if group_exists "$SERVICE_USER"; then
            run "$useradd" --system --gid "$SERVICE_USER" --home-dir "$DATA_DIR" --no-create-home \
                --shell "$nologin" --comment "Swifty Home" "$SERVICE_USER"
        else
            run "$useradd" --system --user-group --home-dir "$DATA_DIR" --no-create-home \
                --shell "$nologin" --comment "Swifty Home" "$SERVICE_USER"
        fi
    elif has adduser && has addgroup; then
        # BusyBox, as on Alpine.
        if ! group_exists "$SERVICE_USER"; then
            run addgroup -S "$SERVICE_USER"
        fi
        run adduser -S -D -H -h "$DATA_DIR" -s "$nologin" -G "$SERVICE_USER" -g "Swifty Home" "$SERVICE_USER"
    else
        die "there is no useradd or adduser here to create the user $SERVICE_USER."
    fi
fi

# Owner, group and mode as `swifty migrate` creates them too. An existing directory is kept as it is.
make_dir() {
    if root_test -d "$1"; then
        say "  keeping $1"
    else
        run install -d -m "$2" -o "$3" -g "$4" "$1"
    fi
}
make_dir "$CONFIG_DIR" 0750 0 "$SERVICE_USER"
make_dir "$DATA_DIR" 0700 "$SERVICE_USER" "$SERVICE_USER"
make_dir "$BACKUP_DIR" 0700 "$SERVICE_USER" "$SERVICE_USER"

# The new binary goes in beside the old one and is then renamed over it, so a running
# hub is never left with a half-written file.
run install -d -m 0755 "$(dirname "$BIN")"
run install -m 0755 -o 0 -g 0 "$stage/swifty" "$BIN.new"
run mv -f "$BIN.new" "$BIN"

run install -d -m 0755 "$SHARE_DIR" "$SHARE_DIR/systemd"
for file in LICENSE NOTICE THIRD-PARTY-NOTICES.txt swifty.example.toml; do
    run install -m 0644 -o 0 -g 0 "$stage/$file" "$SHARE_DIR/$file"
done
for unit in $UNITS; do
    run install -m 0644 -o 0 -g 0 "$stage/$unit" "$SHARE_DIR/systemd/$unit"
done

if has systemctl; then
    run install -d -m 0755 "$UNIT_DIR"
    for unit in $UNITS; do
        run install -m 0644 -o 0 -g 0 "$stage/$unit" "$UNIT_DIR/$unit"
    done
    if systemd_running; then
        run systemctl daemon-reload
        run systemctl enable swifty-home.service swifty-home-backup.timer
    else
        say "  systemd is installed but not running here: the units are in $UNIT_DIR, not enabled."
    fi
fi

# --- First run --------------------------------------------------------------------------

config_ready=$have_config
if [ "$have_config" = yes ]; then
    say "  keeping $CONFIG"
elif [ "$no_init" = yes ]; then
    say "  not running swifty init (--no-init)"
else
    set -- "$BIN" init --data-dir "$DATA_DIR"
    if [ -n "$origin" ]; then set -- "$@" --origin "$origin"; fi
    if [ "$demo" = yes ]; then set -- "$@" --demo; fi
    # Run as root, swifty switches to the owner of the data directory (swifty-home) before
    # it writes anything, so the config directory is group-writable while init runs.
    run chmod 0770 "$CONFIG_DIR"
    init_ok=yes
    if [ "$assume_yes" = yes ] || [ "$dry_run" = yes ]; then
        set -- "$@" --yes
        run "$@" </dev/null || init_ok=no
    else
        run "$@" </dev/tty || init_ok=no
    fi
    run chmod 0750 "$CONFIG_DIR"
    [ "$init_ok" = yes ] || die "swifty init stopped. Nothing was started. Fix what it said and run: sudo swifty init"
    # The service must be able to read both files. The audit key is readable by the
    # service alone (the hub refuses a key file that its group or others can read).
    run chgrp "$SERVICE_USER" "$CONFIG"
    run chmod 0640 "$CONFIG"
    if [ "$dry_run" = yes ] || root_test -e "$CONFIG_DIR/audit.key"; then
        run chown "$SERVICE_USER:$SERVICE_USER" "$CONFIG_DIR/audit.key"
        run chmod 0600 "$CONFIG_DIR/audit.key"
    fi
    run chown -R "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR"
    run "$BIN" config check --file "$CONFIG" || die "swifty init wrote a configuration that does not check. Run: sudo swifty config check"
    config_ready=yes
fi

# --- Start ------------------------------------------------------------------------------

started=no
if [ "$no_start" = yes ]; then
    if [ "$was_running" = yes ]; then
        say "  not restarting the running service (--no-start): it uses the old binary until you run 'sudo systemctl restart swifty-home'"
    fi
elif [ "$config_ready" = no ]; then
    say "  not starting the service: there is no $CONFIG yet"
elif systemd_running; then
    run systemctl restart swifty-home.service
    run systemctl start swifty-home-backup.timer
    if [ "$dry_run" = no ]; then
        tries=0
        until systemctl is-active --quiet swifty-home.service; do
            tries=$((tries + 1))
            if [ "$tries" -ge 10 ]; then
                die "swifty-home.service did not start. See: sudo journalctl -u swifty-home -n 50"
            fi
            sleep 1
        done
        started=yes
    fi
fi

# --- What next --------------------------------------------------------------------------

say ""
if [ "$dry_run" = yes ]; then
    say "Dry run finished. Nothing on this machine was changed."
    exit 0
fi
if [ "$started" = yes ]; then
    say "Swifty Home is installed and running, in dry-run mode."
else
    say "Swifty Home is installed."
fi
say ""
say "Next:"
if [ "$config_ready" = no ]; then
    say "  - Configure it:          sudo swifty init   (or: sudo swifty migrate --from <old config>)"
    say "                           init asks for the address people will use and prints the link to open."
fi
if [ "$started" = no ]; then
    if systemd_running; then
        say "  - Start it:              sudo systemctl start swifty-home"
    else
        say "  - Start it:              swifty serve --config $CONFIG   (as the user $SERVICE_USER)"
    fi
fi
if [ "$config_ready" = no ]; then
    :
elif [ "$have_config" = no ]; then
    say "  - Open the page:         the link 'swifty init' printed above. It is http://localhost:8790 on this"
    say "                           machine unless you gave another address. Passkeys need https://, or"
    say "                           http://localhost. For a phone, put Caddy or nginx in front (see docs/tls)."
    say "  - Enroll your passkey:   the link carries a one-time token. Create the owner passkey, then a"
    say "                           recovery passkey. A new token: sudo swifty enroll-token"
else
    say "  - Open the page at the address in $CONFIG and sign in with your passkey."
fi
say "  - Add your devices:      Integrations > Add in the app, or: sudo swifty plugin add <plugin>"
say "                           (sudo swifty plugin list --available shows them all)"
say "  - Reach real devices:    sudo swifty go-live, then sudo systemctl restart swifty-home"
say "  - Check everything:      sudo swifty doctor"
say ""
say "Uninstall: install.sh --uninstall (keeps your data)."

}
