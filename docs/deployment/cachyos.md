# Daily driver on CachyOS (Arch)

Commands are for fish. The harness has no daemon; the Hermes gateway is a separate upstream service and is not managed here. The only unit is a daily, read-only, offline health check (`hermes-skill-bridge doctor`: prints static capability facts, writes nothing, no network).

## Build and install

```fish
cd packaging/arch
makepkg -f
sudo pacman -U hermes-harness-*.pkg.tar.zst
```

`makepkg` clones the surrounding checkout (`git+file://`), so build from a committed state. Needs `cargo` and `git` (pacman `rust` or `rustup` with a default toolchain). No AUR packages are involved.

Installed files: `/usr/bin/hermes-*`, `/usr/lib/systemd/user/hermes-harness-health.{service,timer}`, `/usr/share/hermes-harness/skills/`, docs under `/usr/share/doc/hermes-harness/`.

## Enable the health timer

```fish
systemctl --user daemon-reload
systemctl --user enable --now hermes-harness-health.timer
loginctl enable-linger $USER
```

`enable-linger` keeps your user manager, and so the timer, running while logged out; without it the timer fires only during login sessions (`Persistent=true` catches up on next login).

The service sets `ProtectSystem`, `ProtectHome` and `PrivateTmp`, which in a user manager need unprivileged user namespaces (on by default on CachyOS). On status 226/NAMESPACE, check `sysctl kernel.unprivileged_userns_clone` (must be 1) and `sysctl user.max_user_namespaces` (above 0), or drop those three lines with `systemctl --user edit hermes-harness-health.service`.

## Logs

```fish
systemctl --user start hermes-harness-health.service   # run once now
journalctl --user -u hermes-harness-health -o cat | jq
```

For interactive use, JSON logs on stderr are off by default. Turn them on for every shell:

```fish
set -Ux HERMES_LOG info     # error|warn|info|debug|trace
set -Ue HERMES_LOG          # turn off again
```

Each line has `ts`, `level`, `target`, `msg`, `bin`. Arguments are never logged.

## Skill manifests

The skill manifests in `hermes_skills/` call `/ABS/PATH/TO/HARNESS/target/release/...` binaries. After a package install, point your installed copies at `/usr/bin/hermes-native` and `/usr/bin/hermes-skill-bridge`. The packaged manifests under `/usr/share/hermes-harness/skills/` are left as shipped.

## Uninstall / undo

```fish
systemctl --user disable --now hermes-harness-health.timer
loginctl disable-linger $USER      # only if nothing else needs linger
sudo pacman -R hermes-harness
set -Ue HERMES_LOG
```
