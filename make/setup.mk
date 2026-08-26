# Tool install is `make setup` only. Checkout/merge hooks call scripts/setup-tools.sh.
# `make build` / `make test` / `make all` call setup-hooks so a clone that has
# not yet run `make setup` still gets `core.hooksPath=.githooks` (push gate).

.PHONY: setup setup-hooks setup-tools install-hooks

setup: setup-hooks setup-tools

setup-hooks:
	@if [ -d "$(ROOT)/.git" ] || [ -f "$(ROOT)/.git" ]; then \
		git -C "$(ROOT)" config core.hooksPath .githooks; \
		echo "git hooksPath -> .githooks"; \
	fi

setup-tools:
	@$(ROOT)/scripts/setup-tools.sh

install-hooks: setup-hooks
