defmodule SimulationHarnessTest do
  use ExUnit.Case, async: true

  alias SimulationHarness.Scenario

  test "simulate_environment advances logical clock" do
    sc = Scenario.new("smoke", [%{id: "n1", body: :earth}])
    assert {:ok, result} = SimulationHarness.simulate_environment(sc, steps: 5)
    assert result.ok
    assert result.steps == 5
    assert result.final_logical == 5
    assert result.state.wall_ns == 5_000_000
  end

  test "clock_rate scales wall ticks" do
    sc = Scenario.new("fast", [%{id: "n1", body: :earth}], clock_rate: 2.0)
    assert {:ok, result} = SimulationHarness.simulate_environment(sc, steps: 3)
    assert result.state.wall_ns == 6_000_000
  end

  test "inject_bit_flip and set_mars_delay" do
    sc = Scenario.new("chaos", [%{id: "n1", body: :mars}], mars_delay_ms: 100)
    {:ok, %{state: state}} = SimulationHarness.simulate_environment(sc)
    state = SimulationHarness.inject_bit_flip(state, "n1", "block-a")
    state = SimulationHarness.set_mars_delay(state, 500)
    assert state.mars_delay_ms == 500
    assert {"n1", "block-a"} in state.bit_flips
  end

  test "validate_temporal_consistency" do
    history = [%{at: 1, content: "a"}, %{at: 2, content: "b"}]
    assert :ok = SimulationHarness.validate_temporal_consistency(history)

    assert {:error, :duplicate_timestamps} =
             SimulationHarness.validate_temporal_consistency([
               %{at: 1, content: "a"},
               %{at: 1, content: "b"}
             ])

    assert {:error, :empty_history} = SimulationHarness.validate_temporal_consistency([])

    assert {:error, :malformed_entry} =
             SimulationHarness.validate_temporal_consistency([%{at: 1}])
  end
end
