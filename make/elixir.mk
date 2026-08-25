# Each Elixir package is an independent Mix project. No umbrella.

ELIXIR_LIBS := $(wildcard $(ROOT)/libs/elixir/*)
ELIXIR_APPS := $(wildcard $(ROOT)/apps/*)
ELIXIR_PACKAGES := $(ELIXIR_LIBS) $(ELIXIR_APPS)

.PHONY: build-elixir test-elixir fmt-elixir clean-elixir

build-elixir:
	@if [ -z "$(ELIXIR_PACKAGES)" ]; then \
		echo "No Elixir packages yet."; \
	else \
		for d in $(ELIXIR_PACKAGES); do \
			if [ -f "$$d/mix.exs" ]; then \
				echo "==> mix compile $$d"; \
				(cd "$$d" && mix deps.get && mix compile) || exit 1; \
			fi; \
		done; \
	fi

test-elixir:
	@if [ -z "$(ELIXIR_PACKAGES)" ]; then \
		echo "No Elixir packages yet."; \
	else \
		for d in $(ELIXIR_PACKAGES); do \
			if [ -f "$$d/mix.exs" ]; then \
				echo "==> mix test $$d"; \
				(cd "$$d" && mix deps.get && mix test) || exit 1; \
			fi; \
		done; \
	fi

fmt-elixir:
	@for d in $(ELIXIR_PACKAGES); do \
		if [ -f "$$d/mix.exs" ]; then \
			(cd "$$d" && mix format) || exit 1; \
		fi; \
	done

clean-elixir:
	@for d in $(ELIXIR_PACKAGES); do \
		if [ -f "$$d/mix.exs" ]; then \
			(cd "$$d" && mix clean) || true; \
			rm -rf "$$d/_build" "$$d/deps"; \
		fi; \
	done
