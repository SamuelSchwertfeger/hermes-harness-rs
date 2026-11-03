# Debian 13 (trixie)

Commands are for bash. Same layout as the [Arch package](cachyos.md): the harness has no daemon, and the only unit is a daily, read-only, offline health check (`hermes-skill-bridge doctor`).

CI builds, installs and smoke-tests the package on Debian 13. Other Debian-based releases are not tested.

## Toolchain

The crate needs Rust 1.88 or newer. Debian 13's `rustc` is 1.85, so use rustup:

```sh
sudo apt install build-essential git curl python3     # python3 is used by the tests
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- --profile minimal
. "$HOME/.cargo/env"
rustc --version    # 1.88 or newer
```

## Option 1: build a .deb (recommended)

```sh
bash packaging/debian/build-deb.sh            # writes hermes-harness_<version>_<arch>.deb here
sudo apt install ./hermes-harness_*.deb
```

The script clones the committed checkout (uncommitted edits are left out), builds, runs the tests and packages the result. Set `DEB_BUILD_OPTIONS=nocheck` to skip the tests. The version is `<crate version>+r<commit count>.<short sha>`.

Dependencies are `git` and `util-linux`. apt also pulls the recommended `bubblewrap` (for `functional-verify` and `sandbox-tool`) and `python3` (functional-verify test runs, repair-dispatch syntax check); add `--no-install-recommends` to skip them.

Installed files: `/usr/bin/hermes-*`, `/usr/lib/systemd/user/hermes-harness-health.{service,timer}`, `/usr/share/hermes-harness/skills/`, docs under `/usr/share/doc/hermes-harness/`.

## Option 2: no package

```sh
cargo install --locked --path .               # binaries land in ~/.cargo/bin
```

Nothing goes under `/usr`, and the health timer is not installed. To use the timer anyway, copy both files from `packaging/systemd/` to `~/.config/systemd/user/` and change `ExecStart=` to `%h/.cargo/bin/hermes-skill-bridge doctor`. `ProtectHome=read-only` still allows that path.

## Enable the health timer

```sh
systemctl --user daemon-reload
systemctl --user enable --now hermes-harness-health.timer
loginctl enable-linger "$USER"                # keep the timer firing while logged out
journalctl --user -u hermes-harness-health -o cat | jq
```

The service's sandboxing lines need unprivileged user namespaces. Debian allows them by default (`sysctl kernel.unprivileged_userns_clone` = 1). If the unit fails with status 226/NAMESPACE, check that sysctl, or drop the `PrivateTmp`, `ProtectSystem` and `ProtectHome` lines with `systemctl --user edit hermes-harness-health.service`.

## Logs and skills

`HERMES_LOG=info` turns on JSON logs on stderr (add `export HERMES_LOG=info` to `~/.bashrc` for every shell). Point installed skill manifests at `/usr/bin/hermes-native` and `/usr/bin/hermes-skill-bridge`, or `~/.cargo/bin/...` for option 2.

## Uninstall

```sh
systemctl --user disable --now hermes-harness-health.timer
sudo apt remove hermes-harness                # option 1
cargo uninstall hermes-harness                # option 2
```
