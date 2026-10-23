defmodule MongoRustDecoder.MixProject do
  use Mix.Project

  def project do
    [
      app: :mongodb_rust_decoder,
      version: "0.1.0",
      elixir: "~> 1.15",
      start_permanent: Mix.env() == :prod,
      elixirc_paths: elixirc_paths(Mix.env()),
      deps: deps()
    ]
  end

  def application do
    [extra_applications: [:logger]]
  end

  defp elixirc_paths(:test), do: ["lib", "test/support"]
  defp elixirc_paths(_env), do: ["lib"]

  defp deps do
    [
      {:rustler, "~> 0.39", runtime: false},
      # Needs BSON.LazyDocuments / lazy_cursor_document/1, from the lazy cursor batch change.
      {:mongodb_driver, github: "commoncurriculum/elixir-mongodb-driver", branch: "claude/project-thread-cdrmxc"},
      {:stream_data, "~> 1.1", only: :test}
    ]
  end
end
