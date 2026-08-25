defmodule SimRunner do
  @moduledoc """
  Thin app that **includes** the `SimulationHarness` library and runs named scenarios.

  Catalog entries only assert harness behavior this process can actually run
  (clock, delay, chaos log, history oracle). Store and temporal guarantees are
  covered by `cargo test` on the Rust libraries.
  """

  alias SimulationHarness.Scenario

  @scenarios ~w(
    clock_ticks
    mars_delay_override
    chaos_bit_flip
  )

  def scenarios, do: @scenarios

  def run_all do
    Enum.map(@scenarios, &run_scenario/1)
  end

  def run_scenario(name) when is_binary(name) do
    sc = scenario_def(name)
    {:ok, result} = SimulationHarness.simulate_environment(sc, steps: 3)
    %{name: name, harness: result, oracle: oracle(name, sc, result)}
  end

  defp oracle("clock_ticks", _sc, result) do
    if result.final_logical == 3 and result.state.wall_ns == 3_000_000,
      do: :ok,
      else: {:error, :clock}
  end

  defp oracle("mars_delay_override", sc, result) do
    state = SimulationHarness.set_mars_delay(result.state, 500)

    if sc.mars_delay_ms == 240_000 and state.mars_delay_ms == 500,
      do: :ok,
      else: {:error, :delay}
  end

  defp oracle("chaos_bit_flip", _sc, result) do
    state = SimulationHarness.inject_bit_flip(result.state, "n0", "block0")

    if {"n0", "block0"} in state.bit_flips,
      do: :ok,
      else: {:error, :no_flip}
  end

  defp scenario_def("clock_ticks") do
    Scenario.new("clock_ticks", [%{id: "n0", body: :earth}])
  end

  defp scenario_def("mars_delay_override") do
    Scenario.new(
      "mars_delay_override",
      [%{id: "earth", body: :earth}, %{id: "mars", body: :mars}],
      mars_delay_ms: 240_000
    )
  end

  defp scenario_def("chaos_bit_flip") do
    Scenario.new("chaos_bit_flip", [%{id: "n0", body: :earth}])
  end
end
