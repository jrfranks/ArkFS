# GitHub / shared: format check + package tests. No clippy-as-error, no Kani/Miri/coverage.
# `.github/workflows/ci.yml` calls `make ci` only. Live FUSE must skip when CI=1
# (see fuse_facade/tests/fuse_drive.rs). Do not add ARKFS_REQUIRE_FUSE here.

.PHONY: ci ci-github clippy fmt-check

ci: ci-github

ci-github: fmt-check test
	@echo "GitHub CI (minimal) passed."

clippy:
	cd $(ROOT) && cargo clippy --workspace --all-targets -- -D warnings

fmt-check:
	cd $(ROOT) && cargo fmt --all -- --check
	@for d in $(ELIXIR_PACKAGES); do \
		if [ -f "$$d/mix.exs" ]; then \
			echo "==> mix format --check-formatted $$d"; \
			(cd "$$d" && mix format --check-formatted) || exit 1; \
		fi; \
	done
