.PHONY: ci

ci: fmt-check clippy test
	@echo "CI passed."

.PHONY: clippy
clippy:
	cd $(ROOT) && cargo clippy --workspace --all-targets -- -D warnings

.PHONY: fmt-check
fmt-check:
	cd $(ROOT) && cargo fmt --all -- --check
	@for d in $(ELIXIR_PACKAGES); do \
		if [ -f "$$d/mix.exs" ]; then \
			echo "==> mix format --check-formatted $$d"; \
			(cd "$$d" && mix format --check-formatted) || exit 1; \
		fi; \
	done
