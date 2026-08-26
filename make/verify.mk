# Local-only correctness. GitHub never runs these targets.
# prepush requires Kani, Miri (nightly), llvm-cov, and a working /dev/fuse.
# Tools are installed by `make setup` (clone/checkout hooks), not on every push.
# Commits are not gated. Bypass push: git push --no-verify  or  ARKFS_SKIP_PREPUSH=1.
# `prepush` depends on setup-hooks (not `setup`) so the push gate is armed without
# re-running the Kani/Miri installer.

.PHONY: prove prove-fuse prove-tools-strict prepush precommit

prove: prepush

prove-fuse:
	cd $(ROOT) && ARKFS_REQUIRE_FUSE=1 cargo test -p fuse_facade -p arkfs --all-targets

prepush: setup-hooks
	ARKFS_REQUIRE_FUSE=1 $(MAKE) fmt-check clippy test prove-tools-strict
	@echo "Pre-push (full tests + live FUSE + proofs) passed."

precommit: prepush

prove-tools-strict:
	@command -v cargo-llvm-cov >/dev/null 2>&1 || { \
		echo "cargo-llvm-cov is required for local push."; \
		echo "  rustup component add llvm-tools-preview"; \
		echo "  cargo install cargo-llvm-cov --locked"; \
		exit 1; \
	}
	@cargo kani --version >/dev/null 2>&1 || { \
		echo "Kani is required for local push."; \
		echo "  cargo install --locked kani-verifier && cargo kani setup"; \
		exit 1; \
	}
	@rustup +nightly component list --installed 2>/dev/null | grep -q '^miri' || { \
		echo "Miri (nightly) is required for local push."; \
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
