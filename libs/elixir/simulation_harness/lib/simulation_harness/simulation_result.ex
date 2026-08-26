defmodule SimulationHarness.SimulationResult do
  @moduledoc """
  Result of `SimulationHarness.simulate_environment/2`.

  `ok` is currently always true when steps ≥ 0; oracles in `SimRunner` inspect
  `state` (logical, wall_ns, bit_flips, mars_delay_ms) for real assertions.
  """

  defstruct name: nil,
            steps: 0,
            final_logical: 0,
            ok: false,
            messages: [],
            state: %{}

  @type t :: %__MODULE__{
          name: String.t() | nil,
          steps: non_neg_integer(),
          final_logical: non_neg_integer(),
          ok: boolean(),
          messages: [String.t()],
          state: map()
        }
end
