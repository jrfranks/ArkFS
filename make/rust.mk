# Rust library targets (Cargo workspace is a convenience; Make remains the organizer).
# `test-rust` is `cargo test --workspace` — includes fuse_facade unit/conformance
# tests that do not need /dev/fuse. Live kernel tests skip unless FUSE is present.

.PHONY: build-rust test-rust fmt-rust clean-rust

build-rust:
	cd $(ROOT) && cargo build --workspace

test-rust:
	cd $(ROOT) && cargo test --workspace
	@echo "Test review log: $(ROOT)/target/arkfs-test-review/events.ndjson"

fmt-rust:
	cd $(ROOT) && cargo fmt --all

clean-rust:
	cd $(ROOT) && cargo clean
