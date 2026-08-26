defmodule SimulationHarness.MixProject do
  use Mix.Project

  def project do
    [
      app: :simulation_harness,
      version: "0.1.0",
      elixir: "~> 1.16",
      start_permanent: Mix.env() == :prod,
      deps: deps(),
      description: "ArkFS simulation harness library (scenario DSL + oracle hooks)",
      package: [licenses: ["MIT"]]
    ]
  end

  def application do
    [
      extra_applications: [:logger]
    ]
  end

  defp deps do
    []
  end
end
