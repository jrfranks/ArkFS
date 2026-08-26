# ArkFS monorepo organizer. Mix/Cargo are per-package tools; Make owns the tree.
# Human map: docs/maintainer.md. GitHub runs `make ci`. Local hooks run `make precommit`.
ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))

include $(ROOT)/make/rust.mk
include $(ROOT)/make/elixir.mk
include $(ROOT)/make/ci.mk
include $(ROOT)/make/verify.mk

.PHONY: all build test test-rust test-elixir fmt clean help precommit

all: build

build: build-rust build-elixir

test: test-rust test-elixir

fmt: fmt-rust fmt-elixir

clean: clean-rust clean-elixir

help:
	@echo "ArkFS — Make is the monorepo organizer (see docs/maintainer.md)"
	@echo ""
	@echo "  make build       Build all packages"
	@echo "  make test        Run all package tests"
	@echo "  make test-rust   Rust libraries only"
	@echo "  make test-elixir Elixir libraries and apps"
	@echo "  make fmt         Format sources"
	@echo "  make clean       Remove build artifacts"
	@echo "  make ci          GitHub minimal: fmt-check + package tests (no proofs)"
	@echo "  make precommit   Local commit bar: all tests + clippy + Kani + Miri + llvm-cov"
	@echo "  make install-hooks  Point git at .githooks (pre-commit -> make precommit)"
	@echo ""
	@echo "Libraries live under libs/; apps under apps/."
