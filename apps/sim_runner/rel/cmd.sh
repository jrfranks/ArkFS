#!/bin/sh
# Installed as $PREFIX/bin/sim_runner. Release tree is $PREFIX/lib/sim_runner
# (BINDIR is PREFIX/bin). Mix/Elixir are not required at runtime; ERTS is bundled.
set -eu
bindir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$bindir/../lib/sim_runner" && pwd)
exec "$root/bin/sim_runner" eval 'SimRunner.CLI.main([])'
