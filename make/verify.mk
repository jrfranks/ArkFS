# Local-only correctness. GitHub never runs these targets.
# precommit requires Kani, Miri (nightly), llvm-cov, and a working /dev/fuse.
# Install once: `make install-hooks` (sets core.hooksPath to .githooks).
# Bypass (emergency): git commit --no-verify  or  ARKFS_SKIP_PRECOMMIT=1.

.PHONY: prove prove-fuse prove-tools-strict precommit install-hooks

prove: precommit

prove-fuse:
	cd $(ROOT) && ARKFS_REQUIRE_FUSE=1 cargo test -p fuse_facade -p arkfs --all-targets

precommit: install-hooks
	ARKFS_REQUIRE_FUSE=1 $(MAKE) fmt-check clippy test prove-tools-strict
	@echo "Pre-commit (full tests + live FUSE + proofs) passed."

prove-tools-strict:
	@command -v cargo-llvm-cov >/dev/null 2>&1 || { \
		echo "cargo-llvm-cov is required for local commits."; \
		echo "  rustup component add llvm-tools-preview"; \
		echo "  cargo install cargo-llvm-cov --locked"; \
		exit 1; \
	}
	@cargo kani --version >/dev/null 2>&1 || { \
		echo "Kani is required for local commits."; \
		echo "  cargo install --locked kani-verifier && cargo kani setup"; \
		exit 1; \
	}
	@rustup +nightly component list --installed 2>/dev/null | grep -q '^miri' || { \
		echo "Miri (nightly) is required for local commits."; \
		echo "  rustup toolchain install nightly --component miri"; \
		exit 1; \
	}
	@echo "==> kani (fuse_facade io_buf + xattr; PathKey is Miri)"
	cd $(ROOT) && cargo kani -p fuse_facade --default-unwind 32 \
		--harness write_then_read_returns_payload \
		--harness negative_offset_reads_from_zero \
		--harness range_iff_too_small
	@echo "==> miri (PathKey)"
	cd $(ROOT) && cargo +nightly miri test -p arkfs_core --lib path_parse
	@echo "==> llvm-cov fuse_facade"
	cd $(ROOT) && ARKFS_REQUIRE_FUSE=1 cargo llvm-cov -p fuse_facade --fail-under-lines 80

install-hooks:
	@git -C "$(ROOT)" config core.hooksPath .githooks
	@echo "git hooksPath -> .githooks (pre-commit runs make precommit)"
