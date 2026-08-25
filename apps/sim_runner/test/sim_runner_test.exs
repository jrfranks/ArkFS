defmodule SimRunnerTest do
  use ExUnit.Case, async: true

  test "all catalog scenarios pass harness + oracle" do
    results = SimRunner.run_all()
    assert length(results) == length(SimRunner.scenarios())

    for %{name: name, harness: h, oracle: o} <- results do
      assert h.ok, "#{name} harness failed"
      assert o == :ok, "#{name} oracle failed: #{inspect(o)}"
    end
  end
end
