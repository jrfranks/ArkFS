# Independent Mix app that *includes* libs/elixir/simulation_harness via path dep.
# Make is still the organizer; do not add an umbrella.
defmodule SimRunner.MixProject do
  use Mix.Project

  def project do
    [
      app: :sim_runner,
      version: "0.1.0",
      elixir: "~> 1.16",
      start_permanent: Mix.env() == :prod,
      deps: deps(),
      # Escript = make sim (dev). Mix release = make sim-standalone (not make release).
      escript: [main_module: SimRunner.CLI],
      releases: [
        sim_runner: [
          include_erts: true,
          include_executables_for: [:unix]
        ]
      ],
      description: "ArkFS app: runs SimulationHarness scenarios (includes harness library)",
      package: [licenses: ["MIT"]]
    ]
  end

  def application do
    [
      extra_applications: [:logger],
      mod: {SimRunner.Application, []}
    ]
  end

  defp deps do
    [
      {:simulation_harness, path: "../../libs/elixir/simulation_harness"}
    ]
  end
end
