# Each Elixir package is an independent Mix project. No umbrella.
# ELIXIR_APPS includes apps/arkfs (Rust) which has no mix.exs; the `if [ -f mix.exs ]`
# guard skips it. Harness tests bookkeeping only — CAS/temporal live in cargo test.
#
# Sim/test is MIX_ENV=dev (mix test, escript). It is not cargo --release and
# not MIX_ENV=prod. Optional ERTS bundle is `make sim-standalone`.

ELIXIR_LIBS := $(wildcard $(ROOT)/libs/elixir/*)
ELIXIR_APPS := $(wildcard $(ROOT)/apps/*)
ELIXIR_PACKAGES := $(ELIXIR_LIBS) $(ELIXIR_APPS)
SIM_RUNNER := $(ROOT)/apps/sim_runner
SIM_RUNNER_ESCRIPT := $(SIM_RUNNER)/sim_runner
SIM_RUNNER_REL := $(SIM_RUNNER)/_build/prod/rel/sim_runner

.PHONY: build-elixir test-elixir sim-escript sim-standalone fmt-elixir clean-elixir install-sim-runner

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

# Scenario catalog as a Mix escript (needs Elixir on PATH). Not a release build.
sim-escript:
	@echo "==> mix escript.build sim_runner (dev)"
	cd "$(SIM_RUNNER)" && mix deps.get && mix escript.build
	"$(SIM_RUNNER_ESCRIPT)"

# Portable sim tool with ERTS. Not `make release` (that is arkfs only).
sim-standalone:
	@echo "==> MIX_ENV=prod mix release sim_runner (ERTS; sim tool, not FUSE)"
	rm -rf "$(SIM_RUNNER_REL)"
	cd "$(SIM_RUNNER)" && MIX_ENV=prod mix deps.get && MIX_ENV=prod mix release --overwrite
	@echo "sim standalone: $(SIM_RUNNER_REL)/bin/sim_runner eval 'SimRunner.CLI.main([])'"

# BINDIR must be PREFIX/bin so rel/cmd.sh's ../lib/sim_runner resolves.
install-sim-runner: sim-standalone
	@if [ -z "$(LIBDIR)" ]; then echo "LIBDIR is empty"; exit 1; fi
	rm -rf "$(DESTDIR)$(LIBDIR)/sim_runner"
	install -d "$(DESTDIR)$(LIBDIR)/sim_runner"
	cp -a "$(SIM_RUNNER_REL)/." "$(DESTDIR)$(LIBDIR)/sim_runner/"
	install -d "$(DESTDIR)$(BINDIR)"
	install -m 755 "$(SIM_RUNNER)/rel/cmd.sh" "$(DESTDIR)$(BINDIR)/sim_runner"
	@echo "installed $(DESTDIR)$(BINDIR)/sim_runner"

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
	rm -f "$(SIM_RUNNER_ESCRIPT)"
