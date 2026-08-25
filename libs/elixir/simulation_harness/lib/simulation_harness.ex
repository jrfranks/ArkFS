defmodule SimulationHarness do
  @moduledoc """
  Elixir scenario DSL and oracles for clock, delay, and chaos bookkeeping.

  Durable store and temporal mechanics are tested in the Rust crates. This
  library does not reimplement them.
  """

  alias SimulationHarness.{Chaos, Scenario, SimulationResult}

  @type node_id :: String.t()
  @type block_id :: String.t()

  @doc """
  Run a scenario environment for `steps` virtual ticks (default 1).
  """
  @spec simulate_environment(Scenario.t(), keyword()) ::
          {:ok, SimulationResult.t()} | {:error, term()}
  def simulate_environment(%Scenario{} = scenario, opts \\ []) do
    steps = Keyword.get(opts, :steps, 1)

    if steps < 0 do
      {:error, :negative_steps}
    else
      state = tick(Scenario.init_state(scenario), steps)

      {:ok,
       %SimulationResult{
         name: scenario.name,
         steps: steps,
         final_logical: state.logical,
         ok: true,
         messages: [],
         state: state
       }}
    end
  end

  defp tick(state, 0), do: state

  defp tick(state, steps) do
    step_ns =
      state.scenario.clock_rate
      |> Kernel.*(1_000_000)
      |> round()
      |> max(0)

    Enum.reduce(1..steps, state, fn _, st ->
      %{st | logical: st.logical + 1, wall_ns: st.wall_ns + step_ns}
    end)
  end

  @doc """
  Record a bit-flip injection against a node/block for later integrity checks.
  """
  @spec inject_bit_flip(map(), node_id(), block_id()) :: map()
  def inject_bit_flip(state, node_id, block_id) do
    Chaos.inject_bit_flip(state, node_id, block_id)
  end

  @doc """
  Override Mars one-way delay (milliseconds) on scenario state.
  """
  @spec set_mars_delay(map(), non_neg_integer()) :: map()
  def set_mars_delay(state, delay_ms) when is_integer(delay_ms) and delay_ms >= 0 do
    Map.put(state, :mars_delay_ms, delay_ms)
  end

  @doc """
  Inject structured chaos options: `:bit_flips`, `:lost_nodes`.
  """
  @spec inject_chaos(map(), keyword()) :: map()
  def inject_chaos(state, opts) when is_list(opts) do
    Chaos.inject(state, opts)
  end

  @doc """
  Check a claimed path history: non-empty, well-formed, unique timestamps.
  """
  @spec validate_temporal_consistency([map()]) :: :ok | {:error, term()}
  def validate_temporal_consistency(history) when is_list(history) do
    cond do
      history == [] ->
        {:error, :empty_history}

      not Enum.all?(history, &well_formed_version/1) ->
        {:error, :malformed_entry}

      true ->
        ats = Enum.map(history, & &1.at)

        if length(Enum.uniq(ats)) != length(ats) do
          {:error, :duplicate_timestamps}
        else
          :ok
        end
    end
  end

  defp well_formed_version(entry) do
    is_map(entry) and Map.has_key?(entry, :at) and Map.has_key?(entry, :content)
  end
end
