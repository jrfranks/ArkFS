defmodule SimRunner.Application do
  @moduledoc """
  OTP application for `sim_runner`. Empty supervisor: this app is a CLI, not a
  long-running service. Mix still requires `mod:` in `mix.exs`.
  """
  use Application

  @impl true
  def start(_type, _args) do
    children = []
    opts = [strategy: :one_for_one, name: SimRunner.Supervisor]
    Supervisor.start_link(children, opts)
  end
end
