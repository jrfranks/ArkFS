# ArkFS monorepo organizer. Mix/Cargo are per-package tools; Make owns the tree.
ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))

include $(ROOT)/make/rust.mk
include $(ROOT)/make/elixir.mk
include $(ROOT)/make/ci.mk

.PHONY: all build test test-rust test-elixir fmt clean help

all: build

build: build-rust build-elixir

test: test-rust test-elixir

fmt: fmt-rust fmt-elixir

clean: clean-rust clean-elixir

help:
	@echo "ArkFS — Make is the monorepo organizer"
	@echo ""
	@echo "  make build       Build all packages"
	@echo "  make test        Run all package tests"
	@echo "  make test-rust   Rust libraries only"
	@echo "  make test-elixir Elixir libraries and apps"
	@echo "  make fmt         Format sources"
	@echo "  make clean       Remove build artifacts"
	@echo "  make ci          Full CI pipeline"
	@echo ""
	@echo "Libraries live under libs/; apps under apps/."
