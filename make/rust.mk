# Rust library targets (Cargo workspace is a convenience; Make remains the organizer).
# `test-rust` is `cargo test --workspace` — includes fuse_facade unit/conformance
# tests that do not need /dev/fuse. Live kernel tests skip unless FUSE is present.

.PHONY: build-rust release-rust test-rust fmt-rust clean-rust install-arkfs

build-rust:
	cd $(ROOT) && cargo build --workspace

release-rust:
	cd $(ROOT) && cargo build --release --workspace
	@echo "release binary: $(ROOT)/target/release/arkfs"

test-rust:
	cd $(ROOT) && cargo test --workspace
	@echo "Test review log: $(ROOT)/target/arkfs-test-review/events.ndjson"

fmt-rust:
	cd $(ROOT) && cargo fmt --all

clean-rust:
	cd $(ROOT) && cargo clean

# Native ELF; Cargo is not required on the machine that mounts.
install-arkfs: release-rust
	install -d "$(DESTDIR)$(BINDIR)"
	install -m 755 "$(ROOT)/target/release/arkfs" "$(DESTDIR)$(BINDIR)/arkfs"
	@echo "installed $(DESTDIR)$(BINDIR)/arkfs"
