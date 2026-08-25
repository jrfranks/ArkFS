defmodule SimulationHarness.SimulationResult do
  @moduledoc false

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
