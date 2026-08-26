defmodule SimulationHarness.Scenario do
  @moduledoc """
  Scenario definition and runtime state for SimulationHarness.

  `nodes` are `%{id: String.t(), body: :earth | :moon | :mars}`. `clock_rate`
  of `1.0` means 1 ms of virtual wall time per `simulate_environment` step
  (1_000_000 ns). `init_state/1` is the blank runtime map tests thread through
  chaos helpers.
  """

  @enforce_keys [:name, :nodes]
  defstruct name: nil,
            nodes: [],
            clock_rate: 1.0,
            mars_delay_ms: nil

  @type node_t :: %{id: String.t(), body: :earth | :moon | :mars}
  @type t :: %__MODULE__{
          name: String.t(),
          nodes: [node_t()],
          clock_rate: float(),
          mars_delay_ms: non_neg_integer() | nil
        }

  @type state :: %{
          optional(:bit_flips) => [{String.t(), String.t()}],
          optional(:lost_nodes) => [String.t()],
          optional(:mars_delay_ms) => non_neg_integer() | nil,
          logical: non_neg_integer(),
          wall_ns: non_neg_integer(),
          scenario: t()
        }

  @spec new(String.t(), [node_t()], keyword()) :: t()
  def new(name, nodes, opts \\ []) do
    %__MODULE__{
      name: name,
      nodes: nodes,
      clock_rate: Keyword.get(opts, :clock_rate, 1.0),
      mars_delay_ms: Keyword.get(opts, :mars_delay_ms)
    }
  end

  @spec init_state(t()) :: state()
  def init_state(%__MODULE__{} = scenario) do
    %{
      scenario: scenario,
      logical: 0,
      wall_ns: 0,
      bit_flips: [],
      lost_nodes: [],
      mars_delay_ms: scenario.mars_delay_ms
    }
  end
end
