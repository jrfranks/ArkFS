defmodule SimulationHarness.Chaos do
  @moduledoc """
  Bookkeeping for intended faults. Does not mutate any on-disk object.

  `inject_bit_flip/3` prepends `{node_id, block_id}` onto `:bit_flips`.
  `inject/2` also unions `:lost_nodes`. The Rust store applies real flips in
  `cargo test` (see `persistent_object_store` bit-flip test).
  """

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
