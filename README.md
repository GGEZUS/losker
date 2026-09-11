# losker

Terminal-cyberpunk **greetd greeter + real session locker** with a built-in
touch OSK, written in Rust + GTK4. One binary, two roles:

| invocation | role | auth |
|---|---|---|
| `losker` | greetd greeter (no keyboard at boot) | greetd IPC handshake |
| `losker --lock` | in-session **screen locker** (ext-session-lock-v1) | PAM against `$USER` |
| `losker --demo-g` | greeter UI without greetd/PAM — safe to run anywhere | fake |
| `losker --demo-l` | lock view in a plain preview window — never locks, PAM never armed; ESC closes, password `demo` plays the granted path | fake |
| `losker --version` | version | — |

`--lock` is a **real** locker: the compositor enforces that nothing renders
above the lock surface, routes keyboard *and touch* into it, and kills its
own keybinds while locked. It draws its own OSK inside the lock surface, so
unlocking needs no physical keyboard. Requires a compositor that supports
[ext-session-lock-v1](https://wayland.app/protocols/ext-session-lock-v1)
(niri, sway, hyprland ≥ some version, …); on X11 or an unsupported
compositor it refuses to lock and exits 1.

## Build

```bash
cargo build --release && cargo test
```

Dependencies: `gtk4` (≥ 4.12), `gtk4-layer-shell` **≥ 1.2** (session-lock
API; `pacman -S gtk4-layer-shell`), `linux-pam`, `rustc ≥ 1.92`,
`pkg-config`.

## Deploy (greeter, on the tablet)

```bash
sudo -A cp target/release/losker /usr/local/bin/
```

Plus the machine files: `/etc/greetd/config.toml`, `/etc/greetd/niri-greeter.kdl`,
`/etc/greetd/losker-ascii.txt`, `/etc/pam.d/losker` — see the vault notes.

## Deploy (locker, any machine)

The lock authenticates `$USER` through PAM service `losker`:

```
# /etc/pam.d/losker
auth include login
```

**This file is load-bearing.** If it is missing or empty, PAM falls back to
its deny-all `other` stack: `pam_start` still succeeds and EVERY password is
refused — a permanent lockout. The binary therefore **refuses to lock**
(exit 1) when the file is missing or empty, instead of locking you out.

## Configuration — `losker --config`

A TUI for the look-and-feel config lives in the same binary:

```bash
sudo losker --config        # edits /etc/losker/config.toml
```

Keys — all optional, unknown keys ignored, bad values fall back to defaults
(defaults = today's behavior, so no config file changes nothing):

| key | values | default |
|---|---|---|
| `osk` | `enabled` / `disabled` | `enabled` — disable only when a physical keyboard is ALWAYS connected |
| `logo` | `<distro key>` / `none` / `legacy` | `legacy` (`/etc/greetd/losker-ascii.txt`) |
| `logo_res` | `high` / `medium` / `low` | `high` — which baked capture tier to draw (a missing tier falls back to `high`) |
| `logo_size` | `large` / `medium` / `small` | `large` — how much of the screen the logo fills; independent of `logo_res` (low res at large size is chunky on purpose). The `--config` TUI shows the resulting pixel size for each choice |
| `accent` | `#rrggbb` | stock phosphor green |
| `crt` | `strong` / `subtle` / `off` | `strong` (= the classic look) |

With `osk` enabled the deck **starts stowed** on both the greeter and the
locker — a small `▲` tab at the bottom center of the screen raises it
(flipping to `▼` to stow it again). It never shows uninvited.

The TUI shows a live spinning preview of the selected logo, cycling accent
swatches, and always prints the effective config path (handy under `sudo`,
which strips `LOSKER_CONFIG`/`LOSKER_LOGOS`). Dry-run without
touching the system file:

```bash
LOSKER_CONFIG=/tmp/cfg.toml losker --config
```

Both the greeter and the locker read the config once at startup — edits
apply at the next start, not live.

## Distro logos

Pre-baked spinning logos (generated from areofyl/fetch) ship in
`assets/logos/`: arch, cachyos, debian, fedora, ubuntu, linuxmint, opensuse,
manjaro, endeavour, nixos — each in three resolution tiers (`<key>.txt` =
high, `<key>-med.txt` = medium, `<key>-low.txt` = low; tiers are baked at
fetch `--size` 2 / 1 / 0.5, so "high" draws the same logo with half-size,
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
[vendor/fetch](https://github.com/areofyl/fetch) + fastfetch):

```bash
sudo cp tools/gen-ascii-art.py /usr/local/bin/losker-genart
sudo cp tools/vendor/fetch/fetch /usr/local/lib/losker/fetch
losker-genart --logo void --size 2 --out /etc/losker/logos/void.txt        # high, ~15 s
losker-genart --logo void --size 1 --out /etc/losker/logos/void-med.txt   # medium
losker-genart --logo void --size 0.5 --out /etc/losker/logos/void-low.txt # low
```

NOTE: `--size` only scales the canvas in our vendored fetch (one-line patch
to `tools/vendor/fetch/fetch.c` — upstream caps the canvas at 36 rows,
defeating the flag). The submodule is dirty with that patch on purpose.

Pick the distro + resolution in the TUI (or set `logo` / `logo_res` by
hand). Regenerate the arch default with `python3 tools/gen-ascii-art.py
--size 2` (see the vault's Spinning Logo page for the full pipeline).

Keybind — wrap it in a respawn supervisor keyed on exit codes (see below):

```kdl
Mod+Shift+L hotkey-overlay-title="Lock Screen: losker" {
    spawn-sh "sh -c 'while :; do NO_AT_BRIDGE=1 GTK_USE_PORTAL=0 losker --lock; rc=$?; [ $rc -eq 0 ] || [ $rc -eq 1 ] || [ $rc -eq 2 ] && break; sleep 0.5; done'";
}
```

`NO_AT_BRIDGE=1 GTK_USE_PORTAL=0`: keep GTK away from the a11y/portal buses —
they crash-looped the greeter session, same risk in-session.

## Exit codes

| code | meaning | wrapper |
|---|---|---|
| 0 | authenticated, unlocked | stop |
| 1 | lock refused: protocol unsupported, lock held by another locker, or `/etc/pam.d/losker` missing/empty — nothing was ever mapped | stop |
| 2 | gtk init failed (no display) | stop |
| 3 | lock lost without auth (compositor-side unlock, lock client killed) | respawn |
| 4 | PAM unusable at auth time — failed open (unlocked without auth) | respawn |
| other (101, 134/SIGABRT, …) | crash; a panic inside a glib callback aborts, it does not exit 101 | respawn |

Why respawn on crash: if the lock client dies, the session **stays locked**
(spec behaviour; outputs stay blanked, keybinds stay dead). niri (and sway)
let a *new* locker take over a dead lock, so the respawned instance recovers
the screen. Without the wrapper, recovery is SSH or a VT.

## Notes & constraints

- **Never `kill` the locker expecting an unlock** — SIGKILL keeps the
  session locked (by design). There is deliberately no SIGTERM-unlock:
  any same-user process could then unlock it.
- **No tooltips/menus in lock views**: popups do not display while the
  screen is locked (gtk4-layer-shell limitation). The UI is labels + cairo.
- Diagnostics: greeter traces to `/var/lib/losker/trace.log`
  (greetd's PrivateTmp makes /tmp and stderr useless there). The lock runs
  as your user and cannot write that dir, so its trace goes to stderr —
  the session journal captures it.
- The old niri window-rule on title `osk-lock` is **dead** — lock surfaces
  are not regular windows. Remove the rule; it only masks bugs now.

## Testing the locker

```bash
# safe, never locks:
losker --demo-g  # greeter shell
losker --demo-l  # lock view preview: ESC closes, 'demo' grants+exits
losker --lock   # exits 1 when the PAM file is missing (preflight)

# real lock (keyboard available: this also proves binds are dead while locked)
losker --lock   # type password, RET; wrong password → DENIED + 2s cooldown

# conflict: a second locker exits 1 instead of fighting over the lock
# crash takeover: pkill -9 losker while locked, then re-run --lock —
# it takes over the dead lock (niri: "replacing existing dead lock")
```
