#!/usr/bin/env bash
# losker installer + updater — one command for the first install, every later
# update, and shipping builds to the keyboard-less devices.
#
#   ./install.sh                    install or update THIS machine
#   ./install.sh --push <ssh-host>  update this machine, then copy the fresh
#                                   binary (+ changed logos) to <ssh-host>;
#                                   no toolchain needed on the far end, but
#                                   its sudo must run without a terminal
#                                   (NOPASSWD), as lock screens deserve
#   ./install.sh --status           installed vs origin/main, changes nothing
#   ./install.sh --rollback         put back the previous binary (losker.old)
#
# No clone yet? The pipe works, and later runs of the same command are the
# updater (it clones to $LOSKER_SRC, default ~/.local/share/losker/src):
#
#   curl -sSL https://raw.githubusercontent.com/GGEZUS/losker/main/install.sh | bash
#
# Contract with the machine — what this script will and will not touch:
#   installs   /usr/local/bin/losker (previous copy kept as losker.old)
#   refreshes  /etc/losker/logos/, /etc/greetd/losker-ascii*.txt and the
#              losker-suspend-lock helper — the files coupled to the binary
#   creates    /etc/pam.d/losker ONLY when missing (the refuse-to-lock trap);
#              an existing one is never rewritten, it may be hand-tuned
#              against faillock (wiki: Installation -> Locker)
#   never      /etc/greetd/config.toml, niri-greeter.kdl, /etc/losker/config.toml,
#              the suspend systemd unit once installed, /var/lib/losker state
#   stamps     the deployed commit into /var/lib/losker/installed-commit —
#              that is how --status reports and how an update becomes a
#              no-op when the machine is already current
#
# Options:
#   --force           rebuild and reinstall even when the stamp matches
#   --test/--no-test  cargo test gate (default on — the suite takes ~2 s)
#   --suspend         set up suspend locking without asking (first install)
#   -y, --yes         take the default answer at every question
#   --dry-run         print the root and ssh actions instead of running them
#
# Requirements and the full map of moving parts: the wiki Installation page.
set -Eeuo pipefail

REPO_URL="${LOSKER_REPOSITORY:-https://github.com/GGEZUS/losker}"
BRANCH="${LOSKER_BRANCH:-main}"
SRC_DIR="${LOSKER_SRC:-$HOME/.local/share/losker/src}"
BIN=/usr/local/bin/losker
BIN_OLD=/usr/local/bin/losker.old
SUSPEND_BIN=/usr/local/bin/losker-suspend-lock
SUSPEND_UNIT=/etc/systemd/system/losker-suspend-lock.service
STAMP_FILE=/var/lib/losker/installed-commit
SSH_OPTS=(-o ConnectTimeout=8)

say()  { printf '%s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

usage() {
    cat <<'EOF'
losker installer + updater

  install.sh                    install or update THIS machine
  install.sh --push <ssh-host>  update here, then copy binary + changed logos
                                to <ssh-host> over ssh (no toolchain needed
                                on the far end)
  install.sh --status           installed vs origin/main, changes nothing
  install.sh --rollback         put back the previous binary (losker.old)

First install without a clone (later runs of the same line are updates;
the clone lands in $LOSKER_SRC, default ~/.local/share/losker/src):

  curl -sSL https://raw.githubusercontent.com/GGEZUS/losker/main/install.sh | bash

Options:
  --force           rebuild and reinstall even when the stamp matches
  --test/--no-test  cargo test gate (default on)
  --suspend         set up suspend locking without asking (first install)
  -y, --yes         take the default answer at every question
  --dry-run         print the root and ssh actions instead of running them
  -h, --help        this text

Touching the machine: installs the binary (previous kept as losker.old) and
the version-coupled art; creates /etc/pam.d/losker only when missing; never
rewrites machine-tuned configs (greetd, /etc/losker/config.toml, suspend
unit); stamps the deployed commit into /var/lib/losker/installed-commit.
EOF
    exit 0
}

# ask <question> <y|n default> — EOF counts as the default; --yes and --dry-run
# take the default without stopping (nothing here is worth a blocked pipe).
ask() {
    local q ans
    q="$1 [$([ "$2" = y ] && echo Y/n || echo y/N)] "
    if [ "$ASSUME_YES" = 1 ] || [ "$DRY_RUN" = 1 ]; then
        say "  $q-- taking $2"
        [ "$2" = y ]
    else
        read -r -p "  $q" ans || ans=
        ans=${ans:-$2}; ans=${ans,,}
        [ "$ans" = y ] || [ "$ans" = yes ]
    fi
}

# every privileged action goes through these two, so --dry-run can preview
# the exact commands and the script body stays honest about its scope
run_root() {
    if [ "$DRY_RUN" = 1 ]; then say "  [dry-run] sudo $*"; else sudo "$@"; fi
}
write_root() {  # write_root <file>, content on stdin
    # never `sudo tee` from a pipe: when sudo prompts without a terminal
    # (askpass), it drains stdin and the file lands EMPTY — stage in a
    # temp file and let install carry it across the privilege line instead
    local tmp
    tmp=$(mktemp)
    cat >"$tmp"
    if [ "$DRY_RUN" = 1 ]; then
        say "  [dry-run] sudo install -m644 $tmp $1, content:"
        sed 's/^/      /' "$tmp"
    else
        sudo install -m644 "$tmp" "$1"
    fi
    rm -f "$tmp"
}

# ver_ge <have> <want> — major.minor compare ("1.99" >= "1.92")
ver_ge() {
    local hm=${1%%.*} rest=${1#*.} wm=${2%%.*}
    local hn=${rest%%.*}; rest=${2#*.}; local wn=${rest%%.*}
    [ "$hm" -gt "$wm" ] && return 0
    [ "$hm" -eq "$wm" ] && [ "$hn" -ge "$wn" ]
}

# --- argument parsing -------------------------------------------------------

# the bootstrap below re-execs the clone's script with these — the parse
# loop shifts them away, so keep the originals
ORIGINAL_ARGS=("$@")

MODE=install PUSH_HOST="" FORCE=0 RUN_TESTS=1 ASSUME_YES=0 DRY_RUN=0 WANT_SUSPEND=0
while [ $# -gt 0 ]; do
    case $1 in
        --push)     [ $# -ge 2 ] || die "--push needs a host (user@host)"; MODE=push; PUSH_HOST=$2; shift 2;;
        --push=*)   MODE=push; PUSH_HOST=${1#*=}; [ -n "$PUSH_HOST" ] || die "--push needs a host"; shift;;
        --status)   MODE=status; shift;;
        --rollback) MODE=rollback; shift;;
        --force)    FORCE=1; shift;;
        --test)     RUN_TESTS=1; shift;;
        --no-test)  RUN_TESTS=0; shift;;
        --suspend)  WANT_SUSPEND=1; shift;;
        -y|--yes)   ASSUME_YES=1; shift;;
        --dry-run)  DRY_RUN=1; shift;;
        -h|--help)  usage;;
        *)          die "unknown option: $1 (see --help)";;
    esac
done

# --- locate the source: a checkout, or bootstrap one ------------------------

# BASH_SOURCE is unset when bash reads the script from a pipe (the curl
# one-liner) — fall back to $0, whose dirname is the caller's cwd
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd) || script_dir=$PWD

is_losker_repo() {
    [ -f "$1/Cargo.toml" ] && grep -q '^name = "losker"' "$1/Cargo.toml" \
        && git -C "$1" rev-parse --is-inside-work-tree >/dev/null 2>&1
}

if ! is_losker_repo "$script_dir"; then
    # Piped straight from the web (or run outside a checkout): clone once,
    # then hand the same arguments to the clone's copy of this script. Every
    # later run of the curl one-liner lands here, finds its clone, and is
    # just an update.
    step "No losker checkout here — bootstrapping one at $SRC_DIR"
    have git || die "git is required to fetch the source"
    if [ -e "$SRC_DIR" ] && ! is_losker_repo "$SRC_DIR"; then
        die "$SRC_DIR exists but is not a losker checkout — move it aside or set LOSKER_SRC"
    fi
    if [ ! -e "$SRC_DIR" ]; then
        git clone --branch "$BRANCH" "$REPO_URL" "$SRC_DIR"
        # the fetch submodule is only for the live logo pipeline; a failed
        # init must never block a build
        git -C "$SRC_DIR" submodule update --init --recursive \
            || warn "submodule init failed (custom-logo generation only) — continuing"
    fi
    exec bash "$SRC_DIR/install.sh" "${ORIGINAL_ARGS[@]}"
fi

REPO=$(git -C "$script_dir" rev-parse --show-toplevel)
cd "$REPO"

# --- repo state -------------------------------------------------------------

# repo_id — the commit the working tree would build, marked -dirty when local
# edits exist (a dirty tree always rebuilds: that is the dev-iteration case)
repo_id() {
    local sha
    sha=$(git rev-parse HEAD)
    if [ -n "$(git status --porcelain --untracked-files=no --ignore-submodules=dirty)" ]; then
        echo "$sha-dirty"
    else
        echo "$sha"
    fi
}
short_id() {  # "71c1d81" or "71c1d81-dirty" — for humans, in messages
    local s=${REPO_ID:0:12}
    [[ $REPO_ID == *dirty ]] && s="$s-dirty"
    echo "$s"
}

read_stamp() {
    cat "$STAMP_FILE" 2>/dev/null || sudo -n cat "$STAMP_FILE" 2>/dev/null || true
}

update_source() {
    step "Updating source ($REPO)"
    if ! git fetch origin "$BRANCH" 2>/dev/null; then
        warn "git fetch failed (offline?) — continuing from local HEAD"
        return 0
    fi
    git rev-parse -q --verify "origin/$BRANCH" >/dev/null \
        || die "origin/$BRANCH does not exist (wrong LOSKER_BRANCH?)"
    local remote old
    remote=$(git rev-parse "origin/$BRANCH")
    old=$(git rev-parse --short HEAD)
    if [ -n "$(git status --porcelain --untracked-files=no --ignore-submodules=dirty)" ]; then
        warn "working tree has local edits — not pulling; building the tree as it stands"
    elif [ "$(git rev-parse HEAD)" = "$remote" ]; then
        say "  already at origin/$BRANCH ($old)"
    elif git merge-base --is-ancestor HEAD "$remote"; then
        git merge --ff-only "$remote" >/dev/null
        say "  updated to $(git rev-parse --short HEAD) (was $old)"
        git submodule update --init --recursive || true
    elif git merge-base --is-ancestor "$remote" HEAD; then
        say "  local commits ahead of origin/$BRANCH (not pushed) — building them"
    else
        die "local history diverged from origin/$BRANCH — resolve that by hand first"
    fi
}

# --- dependencies -----------------------------------------------------------

deps_missing=()
check_deps_once() {
    deps_missing=()
    local pc v
    have cargo || deps_missing+=("cargo")
    if have rustc; then
        v=$(rustc --version | awk '{print $2}')
        ver_ge "$v" "1.92" || deps_missing+=("rustc >= 1.92 (found $v)")
    else
        deps_missing+=("rustc")
    fi
    pc=$(command -v pkg-config || command -v pkgconf || true)
    if [ -n "$pc" ]; then
        "$pc" --atleast-version=4.14 gtk4 || deps_missing+=("gtk4 >= 4.14 (pacman: gtk4)")
        # the C library's pkg-config name carries the -0 suffix; probing
        # "gtk4-layer-shell" false-negatives on every Arch box
        "$pc" --atleast-version=1.2 gtk4-layer-shell-0 \
            || deps_missing+=("gtk4-layer-shell >= 1.2 (pkg-config name gtk4-layer-shell-0)")
    else
        deps_missing+=("pkgconf")
    fi
    # PAM is runtime, not build-time: --lock is what suffers, warn only.
    # Arch ships libpam.so.0 (package "pam"), Debian libpam.so.2 — glob both.
    ls /usr/lib/libpam.so.* /usr/lib64/libpam.so.* >/dev/null 2>&1 \
        || warn "no libpam found — --lock will refuse to lock until PAM is installed"
}

check_deps() {
    step "Checking build dependencies"
    check_deps_once
    if [ ${#deps_missing[@]} -eq 0 ]; then say "  all present"; return 0; fi
    for m in "${deps_missing[@]}"; do warn "missing: $m"; done
    if have pacman && ask "Install the missing packages with pacman now?" n; then
        run_root pacman -S --needed rust pkgconf gtk4 gtk4-layer-shell
        check_deps_once
        if [ ${#deps_missing[@]} -gt 0 ]; then
            for m in "${deps_missing[@]}"; do warn "still missing: $m"; done
            die "install the packages above, then re-run"
        fi
        say "  all present now"
    else
        die "sudo pacman -S --needed rust pkgconf gtk4 gtk4-layer-shell (or your distro's equivalents), then re-run"
    fi
}

# --- build ------------------------------------------------------------------

build() {
    step "Building"
    cargo build --release
    # a binary that cannot even print its version has no business on a lock screen
    ./target/release/losker --version >/dev/null
    if [ "$RUN_TESTS" = 1 ]; then
        cargo test --release   # same profile as the shipped binary, and the
                               # deps are already warm from the build above
    else
        warn "cargo test SKIPPED (--no-test)"
    fi
}

# --- local install ----------------------------------------------------------

install_local() {
    step "Installing to this machine"
    local first=0
    [ -x "$BIN" ] || first=1
    if [ "$DRY_RUN" = 0 ]; then sudo -v; fi   # one prompt, everything after is warm

    if [ "$first" = 0 ]; then
        run_root cp -a "$BIN" "$BIN_OLD"
        say "  previous binary kept as $BIN_OLD"
    fi
    run_root install -m755 target/release/losker "$BIN"
    run_root mkdir -p /etc/losker/logos /var/lib/losker
    run_root install -m644 assets/logos/*.txt /etc/losker/logos/
    # the legacy greeter art lives in /etc/greetd; a machine without that dir
    # has no greeter role, and creating it would imply one
    if [ -d /etc/greetd ]; then
        run_root install -m644 assets/losker-ascii.txt assets/losker-ascii-med.txt \
            assets/losker-ascii-low.txt /etc/greetd/
    fi
    printf '%s\n' "$REPO_ID" | write_root "$STAMP_FILE"
    say "  stamped $(short_id) -> $STAMP_FILE"

    # PAM: create-only, never overwrite — an existing file may be tuned
    # (faillock-free stacks on touch devices, system-auth on Fedora)
    if [ ! -s /etc/pam.d/losker ]; then
        say "  /etc/pam.d/losker missing — without it --lock refuses to lock"
        say "  (the deny-all trap). Writing the Arch/Debian stack:"
        printf '# /etc/pam.d/losker\nauth include login\n' | write_root /etc/pam.d/losker
        say "  Fedora-family: change it to 'auth include system-auth'."
    else
        say "  PAM: /etc/pam.d/losker present (left untouched)"
    fi

    if [ "$first" = 0 ]; then
        # update path: refresh the helper only where it already exists —
        # never introduce suspend locking a machine deliberately declined
        if [ -e "$SUSPEND_BIN" ]; then
            run_root install -m755 dist/losker-suspend-lock "$SUSPEND_BIN"
            say "  suspend helper refreshed"
        fi
        return 0
    fi

    # ---- first install: the one-time, machine-specific pieces --------------
    say "  first install — a few one-time questions:"

    if [ ! -e /etc/greetd/config.toml ] && ask "Set up losker as the greetd greeter (login screen)?" n; then
        run_root mkdir -p /etc/greetd
        run_root cp greetd-config.toml /etc/greetd/config.toml
        [ -e /etc/greetd/niri-greeter.kdl ] \
            || run_root cp niri-greeter.kdl /etc/greetd/niri-greeter.kdl
        run_root install -m644 assets/losker-ascii.txt assets/losker-ascii-med.txt \
            assets/losker-ascii-low.txt /etc/greetd/
        say "  templates installed to /etc/greetd — review them, then when ready:"
        say "    systemctl enable greetd     # switches the boot login flow"
        say "  Keep your previous greeter config: rolling back is one cp back"
        say "  plus 'systemctl restart greetd' (wiki: Updating)."
    fi

    if [ "$WANT_SUSPEND" = 1 ] || ask "Set up suspend locking (lock before sleep)?" n; then
        run_root install -m755 dist/losker-suspend-lock "$SUSPEND_BIN"
        if [ -e "$SUSPEND_UNIT" ]; then
            say "  unit exists — left untouched (it carries your user/uid/socket)"
        else
            local uid wd
            uid=$(id -u)
            wd=${WAYLAND_DISPLAY:-}
            if [ -z "$wd" ] || [ ! -e "/run/user/$uid/$wd" ]; then
                wd=$(find "/run/user/$uid" -maxdepth 1 -name 'wayland-*' -printf '%f\n' 2>/dev/null | head -n1)
                wd=${wd:-wayland-1}
            fi
            sed -e "s|@USER@|$USER|g" -e "s|@UID@|$uid|g" -e "s|@WAYLAND_DISPLAY@|$wd|g" \
                dist/losker-suspend-lock.service.in | write_root "$SUSPEND_UNIT"
            run_root systemctl daemon-reload
            run_root systemctl enable losker-suspend-lock.service
            say "  suspend unit installed: user=$USER uid=$uid socket=$wd;"
            say "  fires on the next suspend. Exactly one thing may own locking —"
            say "  disable your shell's lock-on-suspend first (wiki: Noctalia)."
        fi
    fi

    # both writers of /var/lib/losker (greeter user at boot, this user from
    # the locker) need append rights; ACLs beat groups and 777. Best-effort.
    local greeter_user
    greeter_user=$(awk -F= '/^[[:space:]]*user[[:space:]]*=/{
        gsub(/[[:space:]]|"/,"",$2); print $2; exit}' /etc/greetd/config.toml 2>/dev/null || true)
    if [ -n "$greeter_user" ] && have setfacl; then
        run_root setfacl -R -m "u:$greeter_user:rwX" /var/lib/losker || true
        run_root setfacl -R -m "u:$USER:rwX" /var/lib/losker || true
        run_root setfacl -m "d:u:$greeter_user:rwX" /var/lib/losker || true
        say "  /var/lib/losker ACLs granted to $greeter_user and $USER"
    elif [ -n "$greeter_user" ]; then
        say "  hint: install 'acl' and re-run, or setfacl by hand: /var/lib/losker"
        say "  needs append rights for the greeter user $greeter_user (wiki: Installation)"
    fi

    say ""
    say "  Still by hand (machine-specific, wiki: Installation):"
    say "    - the lock keybind respawn loop (niri snippet)"
    say "    - your desktop shell's own lock-on-suspend, if any — disable it"
    say "  Two-minute sanity check: losker --demo-l, then a real lock once."
}

# --- remote push ------------------------------------------------------------

run_remote() {          # as the ssh login user (scratch dir, probes, --version)
    if [ "$DRY_RUN" = 1 ]; then say "  [dry-run] ssh $PUSH_HOST: $*"; else ssh "${SSH_OPTS[@]}" "$PUSH_HOST" "$*"; fi
}
run_root_remote() {     # privileged on the far end: /usr/local/bin, /etc, stamps
    if [ "$DRY_RUN" = 1 ]; then say "  [dry-run] ssh $PUSH_HOST: sudo $*"; else ssh "${SSH_OPTS[@]}" "$PUSH_HOST" "sudo $*"; fi
}
write_root_remote() {   # write_root_remote <remote-path>, content on stdin
    if [ "$DRY_RUN" = 1 ]; then
        say "  [dry-run] ssh $PUSH_HOST: sudo tee $1"
        cat >/dev/null
    else
        ssh "${SSH_OPTS[@]}" "$PUSH_HOST" "sudo tee $1 >/dev/null"
    fi
}
push_files() {  # push_files <files...> — into a scratch dir on the far end
    run_remote 'rm -rf /tmp/losker-push && mkdir -p /tmp/losker-push'
    if [ "$DRY_RUN" = 1 ]; then
        say "  [dry-run] scp $* -> $PUSH_HOST:/tmp/losker-push/"
    else
        scp -q "${SSH_OPTS[@]}" "$@" "$PUSH_HOST:/tmp/losker-push/"
    fi
}

push_to_host() {
    step "Pushing to $PUSH_HOST"
    local logos=0
    if [ "$DRY_RUN" = 1 ]; then
        say "  [dry-run] skipping reachability/architecture/stamp checks"
        logos=1   # assume art changed; show the full plan
    else
        run_remote true || die "cannot reach $PUSH_HOST over ssh (awake? on the network?)"
        local rarch
        rarch=$(ssh "${SSH_OPTS[@]}" "$PUSH_HOST" 'uname -m') || die "cannot run commands on $PUSH_HOST"
        [ "$rarch" = "$(uname -m)" ] \
            || die "$PUSH_HOST is $rarch, this machine is $(uname -m) — the binary would not run there"
        # logo delta: ship the ~10 MB of art only when a commit changed it
        local old sha
        old=$(ssh "${SSH_OPTS[@]}" "$PUSH_HOST" "cat $STAMP_FILE 2>/dev/null" || true)
        sha=${old%%-*}
        if [[ ! $sha =~ ^[0-9a-f]{7,40}$ ]] || ! git cat-file -e "$sha^{commit}" 2>/dev/null \
            || ! git diff --quiet "$sha" HEAD -- assets 2>/dev/null; then
            logos=1
        fi
    fi

    run_root_remote "[ -x $BIN ] && cp -a $BIN $BIN.old || true"   # far-end rollback copy too
    push_files target/release/losker
    run_root_remote "install -m755 /tmp/losker-push/losker $BIN && rm -rf /tmp/losker-push"
    run_root_remote "mkdir -p /var/lib/losker"
    printf '%s\n' "$REPO_ID" | write_root_remote "$STAMP_FILE"

    if [ "$logos" = 1 ]; then
        say "  art changed since the remote stamp (or unknown) — shipping logos too"
        push_files assets/logos/*.txt assets/losker-ascii.txt \
            assets/losker-ascii-med.txt assets/losker-ascii-low.txt
        run_root_remote "mkdir -p /etc/losker/logos && install -m644 /tmp/losker-push/*.txt /etc/losker/logos/ \
            && { [ -d /etc/greetd ] && install -m644 /tmp/losker-push/losker-ascii*.txt /etc/greetd/ || true; } \
            && rm -rf /tmp/losker-push"
    fi

    if run_remote "[ -x $SUSPEND_BIN ]"; then
        push_files dist/losker-suspend-lock
        run_root_remote "install -m755 /tmp/losker-push/losker-suspend-lock $SUSPEND_BIN"
    fi

    if [ "$DRY_RUN" = 0 ]; then
        say "  remote binary: $(ssh "${SSH_OPTS[@]}" "$PUSH_HOST" "$BIN --version" 2>/dev/null || echo 'would not run — check by hand')"
    fi
    say "  picked up at: next lock (locker) / next boot (greeter); a locker"
    say "  running right now keeps the old build until it unlocks"
}

# --- modes ------------------------------------------------------------------

do_status() {
    step "losker status"
    local id remote fetched=ok stamp verdict dirt="(clean)"
    id=$(repo_id)
    [[ $id == *dirty ]] && dirt="(dirty)"
    if git fetch origin "$BRANCH" 2>/dev/null && git rev-parse -q --verify "origin/$BRANCH" >/dev/null; then
        remote=$(git rev-parse --short "origin/$BRANCH")
    else
        fetched=offline
        remote="$(git rev-parse --short "origin/$BRANCH" 2>/dev/null || echo '?') — last known, fetch failed"
    fi
    stamp=$(read_stamp)

    say "  source:   $REPO @ $(git rev-parse --short HEAD) $dirt"
    say "  upstream: origin/$BRANCH = $remote"
    if [ -x "$BIN" ]; then
        say "  binary:   $BIN — $($BIN --version 2>/dev/null || echo 'no version'), $(date -r "$BIN" '+%F %H:%M')"
    else
        say "  binary:   not installed on this machine"
    fi
    if [[ $stamp == rolled-back-from* ]]; then
        say "  deployed: rolled back from ${stamp#rolled-back-from }"
    elif [ -n "$stamp" ]; then
        [[ $stamp == *dirty ]] && say "  deployed: ${stamp:0:12} (from a dirty tree)" || say "  deployed: ${stamp:0:12}"
    else
        say "  deployed: no stamp — installed before stamping, or fully by hand"
    fi

    if [ ! -x "$BIN" ]; then verdict="not installed here — run ./install.sh"
    elif [ -z "$stamp" ]; then verdict="unstamped install — run ./install.sh once to align and stamp"
    elif [[ $stamp == rolled-back-from* ]]; then verdict="rolled back — ./install.sh reinstalls from source"
    elif [ "$stamp" = "$id" ]; then
        if [ "$fetched" = ok ] && [ "$(git rev-parse HEAD)" != "$(git rev-parse "origin/$BRANCH")" ]; then
            if git merge-base --is-ancestor "origin/$BRANCH" HEAD; then
                verdict="running local commits ahead of origin/$BRANCH (unpushed)"
            else
                verdict="updates on origin/$BRANCH — ./install.sh pulls and rebuilds"
            fi
        else
            verdict="up to date"
        fi
    else verdict="binary older than source — run ./install.sh"; fi
    say "  verdict:  $verdict"
}

do_rollback() {
    step "Rolling back the local binary"
    [ -x "$BIN_OLD" ] || die "no $BIN_OLD to go back to"
    run_root cp -a "$BIN" "$BIN.tmp"
    run_root cp -a "$BIN_OLD" "$BIN"
    run_root cp -a "$BIN.tmp" "$BIN_OLD"
    run_root rm -f "$BIN.tmp"
    # a matching stamp would make the next plain run a no-op while the binary
    # is now older than the tree — mark it so the next ./install.sh rebuilds
    printf 'rolled-back-from %s\n' "$(repo_id)" | write_root "$STAMP_FILE"
    say "  back to the previous build ($("$BIN" --version 2>/dev/null || echo '?'))"
}

# --- main --------------------------------------------------------------------

# status and rollback never need a toolchain or a network
if [ "$MODE" = status ];   then do_status;   exit 0; fi
if [ "$MODE" = rollback ]; then do_rollback; exit 0; fi

check_deps
update_source
REPO_ID=$(repo_id)
stamp=$(read_stamp)

if [ "$FORCE" = 0 ] && [ "$stamp" = "$REPO_ID" ] && [ -x "$BIN" ]; then
    say "Already installed at $(short_id) — nothing to do (--force to rebuild)."
    if [ "$MODE" != push ]; then exit 0; fi
else
    build
    install_local
fi

if [ "$MODE" = push ]; then push_to_host; fi

step "Done"
say "  the two-minute check worth running after any risky update:"
say "    losker --demo-l    # lock view renders, the OSK raises and types"
say "    systemctl suspend  # wake: expect the losker lock screen, unlock once"
