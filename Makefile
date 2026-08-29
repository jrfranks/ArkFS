# ArkFS monorepo organizer. Mix/Cargo are per-package tools; Make owns the tree.
# Human map: docs/maintainer.md. GitHub runs `make ci`. Local pre-push runs `make prepush`.
ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))

# Release = arkfs ELF. Sim/test = cargo test (debug) + mix test + `make sim`.
#   make install PREFIX=/usr
#   make install DESTDIR=/tmp/stage PREFIX=/usr
# BINDIR must stay PREFIX/bin so sim_runner's ../lib/sim_runner resolves.
PREFIX ?= /usr/local
DESTDIR ?=
BINDIR ?= $(PREFIX)/bin
LIBDIR ?= $(PREFIX)/lib

include $(ROOT)/make/setup.mk
include $(ROOT)/make/rust.mk
include $(ROOT)/make/elixir.mk
include $(ROOT)/make/ci.mk
include $(ROOT)/make/verify.mk

.PHONY: all build release install install-sim test sim sim-standalone test-rust test-elixir fmt clean help precommit prepush setup

all: build

build: setup-hooks build-rust build-elixir

# Production FUSE binary. Does not build or package the sim catalog.
release: setup-hooks release-rust

# FUSE node. Does not need Mix. Does not rewrite git hooks (safe as root).
install: install-arkfs

# Optional portable sim tool (ERTS). Not a FUSE release.
install-sim: install-sim-runner

test: setup-hooks test-rust test-elixir

sim: setup-hooks sim-escript

fmt: fmt-rust fmt-elixir

clean: clean-rust clean-elixir

help:
	@echo "ArkFS — Make is the monorepo organizer (see docs/maintainer.md)"
	@echo ""
	@echo "  Sim / test (debug; not cargo --release, not MIX_ENV=prod):"
	@echo "  make test        cargo test --workspace + mix test"
	@echo "  make sim         Elixir scenario catalog (escript; needs Mix)"
	@echo "  make sim-standalone  Portable sim_runner with ERTS (optional)"
	@echo "  make install-sim Install that portable sim tool"
	@echo ""
	@echo "  Release (FUSE node):"
	@echo "  make build       Debug compile of all packages"
	@echo "  make release     Optimized arkfs ELF only"
	@echo "  make install     Install arkfs to \$$(DESTDIR)\$$(PREFIX)/bin (no Mix)"
	@echo "                    (PREFIX=$(PREFIX); DESTDIR may be empty)"
	@echo ""
	@echo "  make test-rust   Rust libraries only"
	@echo "  make test-elixir Elixir libraries and apps"
	@echo "  make fmt         Format sources"
	@echo "  make clean       Remove build artifacts"
	@echo "  make setup       Install rustfmt/clippy/Kani/Miri/llvm-cov + enable git hooks"
	@echo "  make ci          GitHub minimal: fmt-check + package tests (no proofs)"
	@echo "  make prepush     Local push bar: all tests + clippy + Kani + Miri + llvm-cov"
	@echo "  make precommit   Alias of prepush (kept for scripts)"
	@echo ""
	@echo "After clone: make setup (tools). make build/test/release/sim also set git hooksPath."
	@echo ""
	@echo "Libraries live under libs/; apps under apps/."
