defmodule SimRunner.CLI do
  @moduledoc """
  Escript entry (`mix.exs` `escript: [main_module: SimRunner.CLI]`).

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
