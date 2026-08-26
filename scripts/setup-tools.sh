#!/bin/sh
# Install every tool the local push bar needs. Idempotent. POSIX sh.
# GitHub (`CI` set) is a no-op: Actions must stay on `make ci` without Kani/Miri.
# Skip: ARKFS_SKIP_SETUP=1
#
# Called from: make setup, .githooks/post-checkout, post-merge.
# Not called from pre-push (`make prepush` must not reinstall tools).

if [ -n "${CI:-}" ]; then
	echo "CI is set: skipping local tool install (GitHub uses make ci)."
	exit 0
fi
if [ "${ARKFS_SKIP_SETUP:-}" = 1 ]; then
	echo "ARKFS_SKIP_SETUP=1: skipping tool install."
	exit 0
fi
if [ -n "${ARKFS_SETUP_RUNNING:-}" ]; then
	exit 0
fi
export ARKFS_SETUP_RUNNING=1

# Cargo installs land here; git hooks and make may have a slim PATH.
export PATH="${HOME}/.cargo/bin:${PATH}"

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd) || exit 1
cd "$root" || exit 1

if [ -d "$root/.git" ] || [ -f "$root/.git" ]; then
	git -C "$root" config core.hooksPath .githooks
fi

have() { command -v "$1" >/dev/null 2>&1; }

miri_ok() {
	rustup +nightly component list --installed 2>/dev/null | grep -q '^miri'
}

kani_ok() {
	cargo kani --version >/dev/null 2>&1
}

llvm_cov_ok() {
	have cargo-llvm-cov
}

stable_components_ok() {
	have rustc && have cargo && have rustup || return 1
	list=$(rustup component list --installed 2>/dev/null) || return 1
	echo "$list" | grep -q rustfmt || return 1
	echo "$list" | grep -q clippy || return 1
	echo "$list" | grep -q llvm-tools || return 1
	return 0
}

tools_ok() {
	stable_components_ok && llvm_cov_ok && kani_ok && miri_ok
}

if tools_ok; then
	echo "ArkFS tools already installed."
	exit 0
fi

echo "==> ArkFS setup: installing local proof/test tools"

if ! have rustup; then
	if ! have curl; then
		echo "rustup is missing and curl is not available to install it." >&2
		exit 1
	fi
	echo "==> rustup"
	curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
	# shellcheck disable=SC1091
	[ -f "${HOME}/.cargo/env" ] && . "${HOME}/.cargo/env"
	export PATH="${HOME}/.cargo/bin:${PATH}"
fi

if [ -f "$root/rust-toolchain.toml" ]; then
	echo "==> rust-toolchain.toml (stable + rustfmt/clippy/llvm-tools)"
	# Entering the repo makes rustup download the file's toolchain + components.
	rustup show >/dev/null
fi

echo "==> rustup components on the active toolchain"
rustup component add rustfmt clippy llvm-tools-preview

echo "==> nightly + miri (PathKey)"
rustup toolchain install nightly --component miri
rustup +nightly component add miri

if ! llvm_cov_ok; then
	echo "==> cargo-llvm-cov"
	cargo install cargo-llvm-cov --locked
fi

if ! kani_ok; then
	echo "==> kani-verifier (first run also downloads CBMC; can take a few minutes)"
	cargo install --locked kani-verifier
	cargo kani setup
fi

if ! have fusermount3 && ! have fusermount; then
	echo "==> fuse3 not on PATH (live FUSE tests need /dev/fuse + fusermount3)."
	echo "    Debian/Ubuntu: sudo apt-get install fuse3"
fi

if ! have mix; then
	echo "==> Elixir/Mix not on PATH (Elixir package tests need it)."
	echo "    Debian: sudo apt-get install elixir erlang-dev"
fi

if ! tools_ok; then
	echo "setup-tools: still missing a required tool after install." >&2
	echo "  rustup:     $(command -v rustup || echo MISSING)" >&2
	echo "  cargo:      $(command -v cargo || echo MISSING)" >&2
	echo "  llvm-cov:   $(command -v cargo-llvm-cov || echo MISSING)" >&2
	echo "  kani:       $(cargo kani --version 2>/dev/null || echo MISSING)" >&2
	echo "  miri:       $(miri_ok && echo ok || echo MISSING)" >&2
	exit 1
fi

echo "ArkFS setup complete. Local git push runs make prepush (Kani/Miri/llvm-cov/FUSE)."
exit 0
