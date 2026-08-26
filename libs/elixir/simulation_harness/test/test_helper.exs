# ExUnit bootstrap for this Mix package. Keep empty of cluster boots.
# Also append NDJSON to the ArkFS test-review log (same file as cargo test).

defmodule Arkfs.TestReview do
  @moduledoc false
  use GenServer

  @impl true
  def init(_opts), do: {:ok, nil}

  @impl true
  def handle_cast({:test_started, test}, state) do
    write(%{"event" => "start", "test" => name(test)})
    {:noreply, state}
  end

  def handle_cast({:test_finished, test}, state) do
    ok = match?(%ExUnit.Test{state: nil}, test)
    write(%{"event" => "end", "test" => name(test), "ok" => ok})
    {:noreply, state}
  end

  def handle_cast(_msg, state), do: {:noreply, state}

  defp name(%ExUnit.Test{module: m, name: n}), do: "#{inspect(m)}/#{n}"

  defp write(map) do
    if System.get_env("ARKFS_TEST_REVIEW") in ["0", "false"] do
      :ok
    else
      dir =
        System.get_env("ARKFS_TEST_REVIEW_DIR") ||
          Path.expand("../../../target/arkfs-test-review")

      File.mkdir_p!(dir)
      ts = System.system_time(:millisecond)
      test = Map.get(map, "test", "")
      event = Map.get(map, "event", "")
      ok = Map.get(map, "ok")

      line =
        [
          "{\"ts_ms\":#{ts}",
          "\"test\":\"#{escape(test)}\"",
          "\"event\":\"#{escape(event)}\""
        ]
        |> then(fn parts ->
          if is_nil(ok), do: parts, else: parts ++ ["\"ok\":#{ok}"]
        end)
        |> Enum.join(",")
        |> Kernel.<>("}\n")

      File.write!(Path.join(dir, "events.ndjson"), line, [:append])
    end
  end

  defp escape(s) do
    s
    |> to_string()
    |> String.replace("\\", "\\\\")
    |> String.replace("\"", "\\\"")
    |> String.replace("\n", "\\n")
  end
end

ExUnit.start(formatters: [ExUnit.CLIFormatter, Arkfs.TestReview])
