#!/usr/bin/env bash
# Build a .deb from the committed checkout: bash packaging/debian/build-deb.sh [out-dir]
# Needs cargo (Rust 1.88 or newer), git, dpkg-deb, a C compiler (build-essential) and python3 (tests).
# Set DEB_BUILD_OPTIONS=nocheck to skip the test run.
set -euo pipefail

repo=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
out=$(realpath "${1:-.}")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Clone so uncommitted edits never end up in the package (same rule as the PKGBUILD).
git clone -q "file://$repo" "$work/src"
cd "$work/src"

base=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
ver="$base+r$(git rev-list --count HEAD).$(git rev-parse --short HEAD)"
arch=$(dpkg --print-architecture)

export CARGO_TARGET_DIR="$work/src/target"
cargo build --locked --release
case " ${DEB_BUILD_OPTIONS:-} " in
  *" nocheck "*) ;;
  *) cargo test --locked --release ;;
esac

pkg="$work/pkg"
for bin in hermes-harness hermes-native hermes-skill-bridge \
  hermes-gateway-contract-probe hermes-gateway-lookup-probe \
  hermes-gateway-socket-qualification hermes-live-host-probe \
  hermes-live-operator hermes-skill-install hermes-supervised-demo \
  hermes-supervised-status hermes-probe; do
  install -Dm755 "target/release/$bin" "$pkg/usr/bin/$bin"
done
install -Dm644 packaging/systemd/hermes-harness-health.service \
  packaging/systemd/hermes-harness-health.timer \
  -t "$pkg/usr/lib/systemd/user/"
install -d "$pkg/usr/share/hermes-harness"
cp -r hermes_skills "$pkg/usr/share/hermes-harness/skills"
install -Dm644 README.md DIVERGENCES.md -t "$pkg/usr/share/doc/hermes-harness/"
install -Dm644 LICENSE "$pkg/usr/share/doc/hermes-harness/copyright"

install -d "$pkg/DEBIAN"
cat > "$pkg/DEBIAN/control" <<EOF
Package: hermes-harness
Version: $ver
Architecture: $arch
Maintainer: Samuel Schwertfeger <SamuelSchwertfeger@users.noreply.github.com>
Installed-Size: $(du -sk "$pkg/usr" | cut -f1)
Depends: libc6, libgcc-s1, git, util-linux, ripgrep
Recommends: bubblewrap, python3
Section: utils
Priority: optional
Homepage: https://github.com/SamuelSchwertfeger/hermes-harness-rs
Description: Rust port of the Hermes harness
 Skill bridge, probes and an offline health check for ticket-driven
 multi-agent work with Hermes. bubblewrap is needed for functional-verify
 and sandbox-tool; python3 for functional-verify test runs and the
 repair-dispatch syntax check.
EOF

deb="$out/hermes-harness_${ver}_${arch}.deb"
dpkg-deb --build --root-owner-group "$pkg" "$deb"
echo "$deb"
