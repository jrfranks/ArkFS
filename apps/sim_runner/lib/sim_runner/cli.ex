defmodule SimRunner.CLI do
  @moduledoc """
  CLI for the scenario catalog (sim/test, not a FUSE release).

  `make test` runs ExUnit. `make sim` builds this escript and runs the catalog
  (needs Mix). Optional portable ERTS bundle: `make sim-standalone` /
  `make install-sim`.

  Prints one line per scenario and exits 1 if any harness or oracle fails.
  Arguments are ignored; the catalog is `SimRunner.scenarios/0`.
  """

  def main(_args) do
    results = SimRunner.run_all()

    Enum.each(results, fn %{name: name, harness: h, oracle: o} ->
      status =
        cond do
          not h.ok -> "FAIL harness"
          o != :ok -> "FAIL oracle #{inspect(o)}"
          true -> "ok"
        end

      IO.puts("#{name}: #{status}")
    end)

    if Enum.all?(results, fn %{harness: h, oracle: o} -> h.ok and o == :ok end) do
      IO.puts("All scenarios passed.")
    else
      System.halt(1)
    end
  end
end
