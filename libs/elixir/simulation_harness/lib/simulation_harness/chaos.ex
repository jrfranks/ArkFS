defmodule SimulationHarness.Chaos do
  @moduledoc false

  def inject_bit_flip(state, node_id, block_id) do
    flips = Map.get(state, :bit_flips, [])
    Map.put(state, :bit_flips, [{node_id, block_id} | flips])
  end

  def inject(state, opts) do
    state =
      Enum.reduce(Keyword.get(opts, :bit_flips, []), state, fn {n, b}, st ->
        inject_bit_flip(st, n, b)
      end)

    lost = Keyword.get(opts, :lost_nodes, [])
    existing = Map.get(state, :lost_nodes, [])
    Map.put(state, :lost_nodes, Enum.uniq(existing ++ lost))
  end
end
