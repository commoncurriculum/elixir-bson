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
      # 1.5 is the first release whose BSON.Decoder handles every NaN the NIF returns.
      # Batches are only decoded lazily, in chunks, with a driver that has BSON.LazyDocuments.
      {:mongodb_driver, "~> 1.5"},
      {:stream_data, "~> 1.1", only: :test}
    ]
  end
end
