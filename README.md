# losker

[![License: GPL-3.0-only](https://img.shields.io/badge/license-GPL--3.0--only-blue)](LICENSE)
[![Platform: Linux · Wayland](https://img.shields.io/badge/platform-Linux%20%C2%B7%20Wayland-lightgrey)](#requirements)

Terminal-cyberpunk **greetd greeter + real session locker** with a built-in
touch OSK, written in Rust + GTK4. One binary, two roles:

| invocation | role | auth |
|---|---|---|
| `losker` | greetd greeter (no keyboard at boot) | greetd IPC handshake |
| `losker --lock` | in-session **screen locker** (ext-session-lock-v1) | PAM against `$USER` |
| `losker --demo-g` | greeter UI without greetd/PAM, safe to run anywhere | fake |
| `losker --demo-l` | lock view in a plain preview window, never locks, PAM never armed; ESC closes, password `demo` plays the granted path | fake |
| `losker --config` | ratatui TUI for the look-and-feel config | — |
| `losker --version` | version | — |

## Why

This was built for machines you can log into **without a physical keyboard**.
Both roles carry the same on-screen keyboard inside their surface, so a
touch-only tablet can boot into a login screen it can actually type on — and
lock/unlock a session later without a keyboard ever being attached.

## Features

- **Real locking, not a black overlay.** `--lock` goes through
  [ext-session-lock-v1](https://wayland.app/protocols/ext-session-lock-v1):
  the compositor enforces that nothing renders above the lock surface, routes
  keyboard *and touch* into it, and kills its own keybinds while locked. On
  X11 or an unsupported compositor it refuses to lock and exits 1.
- **Unlockable by touch alone.** The OSK is drawn entirely in cairo (no GTK
  buttons) *inside the lock surface*: a typewriter-staggered QWERTY deck whose
  shift key latches into caps lock (a small LED on the cap shows the stuck
  state), a symbols layer covering every ASCII special exactly once, and
  press feedback that inverts the keycap. The deck starts stowed; a `▲` tab
  at the bottom center raises it, flipping to `▼` to stow it again. It never
  shows uninvited.
- **The CRT look.** Scanlines and phosphor glow (`strong` / `subtle` / `off`),
  a stock-phosphor-green accent you can recolor with one hex value, and a
  spinning ASCII distro logo — ten distros pre-baked, three resolution tiers.
- **One config TUI.** `losker --config` is a ratatui interface with a live
  spinning preview. Hard rule: no config file → byte-identical UI (locked by
  a unit test).
- **Fails loud, not locked out.** The binary refuses to lock when
  `/etc/pam.d/losker` is missing or empty (the PAM deny-all trap — see
  [Install → Locker](#install)), and its exit codes are a documented contract
  a respawn wrapper can key on.
- **Safe to try anywhere.** Demo modes run the real UI with fake auth and
  never lock anything.

## Try it safely

```bash
losker --demo-g   # greeter shell, fake auth — runs anywhere
losker --demo-l   # lock view preview — ESC closes, password "demo" grants
```

The full test matrix (real lock, conflict, crash takeover) is at the
[bottom](#testing).

## Tested scope

**Tested on:** Arch-family (Arch, CachyOS) with niri + greetd, on a laptop
and a keyboard-less touch tablet. Other distros, compositors and shells are
untested; the wiki's
[Installation](https://github.com/GGEZUS/losker/wiki/Installation#tested-scope-and-what-that-means-for-you)
page lists what moves per distro. Beyond that, you are on your own.

## Requirements

- Linux + Wayland. The greeter role needs [greetd](https://git.sr.ht/~kennylevinsen/greetd);
  the locker needs a compositor that supports
  [ext-session-lock-v1](https://wayland.app/protocols/ext-session-lock-v1)
  (niri, sway, hyprland ≥ some version, …).
- `gtk4` **≥ 4.14** — the bindings use 4.14 APIs (`Display::monitors()`)
- `gtk4-layer-shell` **≥ 1.2** — the session-lock API (`pacman -S gtk4-layer-shell`)
- `linux-pam` — plus the PAM service file below, or `--lock` refuses to start
- `rustc ≥ 1.92`, cargo, `pkg-config`
- optional: fastfetch + the vendored `fetch`, only for live logo generation
  (see [Distro logos](#distro-logos))

## Install

### One command — first install and every update

```bash
curl -sSL https://raw.githubusercontent.com/GGEZUS/losker/main/install.sh | bash
```

From a clone, the same flow is `./install.sh`. It fetches, builds (the test
suite gates each install), deploys the binary and the version-coupled art,
creates `/etc/pam.d/losker` **only** when missing, and never rewrites
machine-tuned configs — the full contract is in the script header. Deployed
commits are stamped into `/var/lib/losker/installed-commit`, so a current
machine is a no-op, `--status` reports which side of upstream you are on,
`--rollback` restores the kept `losker.old`, and other devices update
without a toolchain of their own:

```bash
./install.sh --push user@tablet   # update here, then ship binary + changed art
```

(far end: ssh reachable, sudo allowed without a terminal). What it
deliberately leaves to you — greetd's config, the lock keybind, shell
coexistence — is exactly the by-hand material below and on the wiki's
[Installation](https://github.com/GGEZUS/losker/wiki/Installation) page.

### Build (by hand)

```bash
cargo build --release && cargo test
```

### Greeter (greetd)

```bash
sudo -A cp target/release/losker /usr/local/bin/
```

Plus the machine files: `/etc/greetd/config.toml` and
`/etc/greetd/niri-greeter.kdl` (templates in the repo root:
`greetd-config.toml`, `niri-greeter.kdl`), `/etc/greetd/losker-ascii.txt`
(or a distro logo from `assets/logos/`, see below) and `/etc/pam.d/losker`
(see the next section). The full walk-through, including the greeter-session
traps (`NO_AT_BRIDGE=1 GTK_USE_PORTAL=0`, PrivateTmp diagnostics), is the
wiki's [Installation](https://github.com/GGEZUS/losker/wiki/Installation)
page.

### Locker (any machine)

The lock authenticates `$USER` through PAM service `losker`:

```
# /etc/pam.d/losker
auth include login
```

**This file is load-bearing.** If it is missing or empty, PAM falls back to
its deny-all `other` stack: `pam_start` still succeeds and EVERY password is
refused, a permanent lockout. The binary therefore **refuses to lock**
(exit 1) when the file is missing or empty, instead of locking you out.

Wrap the keybind in a respawn supervisor keyed on exit codes (the exit-code
table is [below](#exit-codes)):

```kdl
Mod+Shift+L hotkey-overlay-title="Lock Screen: losker" {
    spawn-sh "sh -c 'while :; do NO_AT_BRIDGE=1 GTK_USE_PORTAL=0 losker --lock; rc=$?; [ $rc -eq 0 ] || [ $rc -eq 1 ] || [ $rc -eq 2 ] && break; sleep 0.5; done'";
}
```

`NO_AT_BRIDGE=1 GTK_USE_PORTAL=0`: keep GTK away from the a11y/portal buses,
they crash-looped the greeter session, same risk in-session.

### Desktop integration

A machine tries to lock in two more places than the keybind; the tested
answers (full copy-paste blocks on the wiki):

- **Suspend**: a systemd sleep hook runs the same respawn loop detached, one
  second before the machine sleeps
  ([Installation: suspend locking](https://github.com/GGEZUS/losker/wiki/Installation#suspend-locking-systemd-sleep-hook)) —
  `./install.sh --suspend` templates and enables the same two files, shipped
  in `dist/`.
- **noctalia**: if you run it, its own lock needs disabling in two places,
  both in `~/.local/state/noctalia/settings.toml` (the json `lockOnSuspend`
  key is dead). Verified against noctalia 5.0.1; details on the wiki's
  [Noctalia](https://github.com/GGEZUS/losker/wiki/Noctalia) page.

The rule underneath: exactly one thing may own locking. A different shell
with lock features needs the same division.

## Configuration: `losker --config`

A TUI for the look-and-feel config lives in the same binary:

```bash
sudo losker --config        # edits /etc/losker/config.toml
```

Keys: all optional, unknown keys ignored, bad values fall back to defaults
(defaults = today's behavior, so no config file changes nothing):

| key | values | default |
|---|---|---|
| `osk` | `enabled` / `disabled` | `enabled`, disable only when a physical keyboard is ALWAYS connected |
| `logo` | `<distro key>` / `none` / `legacy` | `legacy` (`/etc/greetd/losker-ascii.txt`) |
| `logo_res` | `high` / `medium` / `low` | `high`, which baked capture tier to draw (a missing tier falls back to `high`) |
| `logo_size` | `large` / `medium` / `small` | `large`, how much of the screen the logo fills; independent of `logo_res` (low res at large size is chunky on purpose). The `--config` TUI shows the resulting pixel size for each choice |
| `accent` | `#rrggbb` | stock phosphor green |
| `crt` | `strong` / `subtle` / `off` | `strong` (the classic look) |

The TUI shows a live spinning preview of the selected logo, cycling accent
swatches, and always prints the effective config path (handy under `sudo`,
which strips `LOSKER_CONFIG`/`LOSKER_LOGOS`). Dry-run without touching the
system file:

```bash
LOSKER_CONFIG=/tmp/cfg.toml losker --config
```

Both the greeter and the locker read the config once at startup: edits
apply at the next start, not live.

## Distro logos

Pre-baked spinning logos (generated from [fetch](https://github.com/areofyl/fetch))
ship in `assets/logos/`: arch, cachyos, debian, fedora, ubuntu, linuxmint,
opensuse, manjaro, endeavour, nixos, each in three resolution tiers
(`<key>.txt` = high, `<key>-med.txt` = medium, `<key>-low.txt` = low; tiers
are baked at fetch `--size` 2 / 1 / 0.5, so "high" draws the same logo with
finer characters). Install them world-readable (the greeter runs as user
`greeter`):

```bash
sudo mkdir -p /etc/losker/logos /etc/losker
sudo cp assets/logos/*.txt /etc/losker/logos/
sudo chmod 644 /etc/losker/logos/*
```

The legacy arch art ships as `assets/losker-ascii{,-med,-low}.txt`; deploy
all three to `/etc/greetd/` the same way. A configured tier whose file is
missing falls back to the plain (high) capture.

Any other fastfetch logo key works via live generation (needs the built
vendor/fetch + fastfetch):

```bash
sudo cp tools/gen-ascii-art.py /usr/local/bin/losker-genart
sudo cp tools/vendor/fetch/fetch /usr/local/lib/losker/fetch
losker-genart --logo void --size 2 --out /etc/losker/logos/void.txt        # high, ~15 s
losker-genart --logo void --size 1 --out /etc/losker/logos/void-med.txt   # medium
losker-genart --logo void --size 0.5 --out /etc/losker/logos/void-low.txt # low
```

NOTE: `--size` only scales the canvas in our fork of fetch (upstream caps
the canvas at 36 rows, silently defeating the flag; the fix is one line in
`apply_layout`). The submodule therefore points at
[GGEZUS/fetch](https://github.com/GGEZUS/fetch) (@ `1459018`), so fresh
clones bake all tiers correctly. If upstream merges the fix, switch
`.gitmodules` back and re-pin.

Pick the distro + resolution in the TUI (or set `logo` / `logo_res` by
hand). The full art pipeline (capture, loop-finding, anti-jitter
normalization, regen recipes) is the wiki's
[Logos](https://github.com/GGEZUS/losker/wiki/Logos) page.

## Exit codes

| code | meaning | wrapper |
|---|---|---|
| 0 | authenticated, unlocked | stop |
| 1 | lock refused: protocol unsupported, lock held by another locker, or `/etc/pam.d/losker` missing/empty, nothing was ever mapped | stop |
| 2 | gtk init failed (no display) | stop |
| 3 | lock lost without auth (compositor-side unlock, lock client killed) | respawn |
| 4 | PAM unusable at auth time, failed open (unlocked without auth) | respawn |
| other (101, 134/SIGABRT, …) | crash; a panic inside a glib callback aborts, it does not exit 101 | respawn |

Why respawn on crash: if the lock client dies, the session **stays locked**
(spec behaviour; outputs stay blanked, keybinds stay dead). niri (and sway)
let a *new* locker take over a dead lock, so the respawned instance recovers
the screen. Without the wrapper, recovery is SSH or a VT.

## Notes & constraints

- **Never `kill` the locker expecting an unlock**: SIGKILL keeps the session
  locked (by design). There is deliberately no SIGTERM-unlock, any same-user
  process could then unlock it.
- **No tooltips/menus in lock views**: popups do not display while the
  screen is locked (gtk4-layer-shell limitation). The UI is labels + cairo.
- Diagnostics: both roles trace to `/var/lib/losker/trace.log` when they can
  write it (the greeter always can; the locker needs the append-right ACL
  covered on the wiki). Otherwise the locker falls back to stderr, the
  session journal captures it. greetd's PrivateTmp makes /tmp and the
  greeter's stderr useless.
- The old niri window-rule on title `osk-lock` is **dead**: lock surfaces
  are not regular windows. Remove the rule; it only masks bugs now.

## Testing

```bash
# safe, never locks:
losker --demo-g  # greeter shell
losker --demo-l  # lock view preview: ESC closes, 'demo' grants+exits
losker --lock    # exits 1 when the PAM file is missing (preflight)

# real lock (keyboard available: this also proves binds are dead while locked)
losker --lock    # type password, RET; wrong password → DENIED + 2s cooldown

# conflict: a second locker exits 1 instead of fighting over the lock
# crash takeover: pkill -9 losker while locked, then re-run --lock,
# it takes over the dead lock (niri: "replacing existing dead lock")
```

PAM-touching tests only ever authenticate a nonexistent canary user, never
a real account: if your `/etc/pam.d/losker` pulls in `pam_faillock`, a test
that fails auth as a real user strikes that user's faillock tally, and
enough runs lock the real account. The
[Development](https://github.com/GGEZUS/losker/wiki/Development) page has
the story.

## Documentation

The [wiki](https://github.com/GGEZUS/losker/wiki) is the documentation:

| page | what's on it |
|---|---|
| [Installation](https://github.com/GGEZUS/losker/wiki/Installation) | greetd + locker setup, suspend locking, tested scope per distro |
| [Configuration](https://github.com/GGEZUS/losker/wiki/Configuration) | every key, defaults, the TUI |
| [OSK](https://github.com/GGEZUS/losker/wiki/OSK) | deck design |
| [Logos](https://github.com/GGEZUS/losker/wiki/Logos) | the spinning ASCII art pipeline |
| [Noctalia](https://github.com/GGEZUS/losker/wiki/Noctalia) | desktop-shell coexistence |
| [Updating](https://github.com/GGEZUS/losker/wiki/Updating) | what system updates can and cannot touch |
| [Troubleshooting](https://github.com/GGEZUS/losker/wiki/Troubleshooting) | known traps: PAM fallbacks, faillock lockouts, compositor quirks |
| [Development](https://github.com/GGEZUS/losker/wiki/Development) | architecture, testing, exit codes |

## Contributing

Issues and pull requests are welcome. Contributions land under the same
license as the project (GPL-3.0-only). The wiki's
[Development](https://github.com/GGEZUS/losker/wiki/Development) page maps
the code before you dive in.

## License

GPL-3.0-only — see [LICENSE](LICENSE).
