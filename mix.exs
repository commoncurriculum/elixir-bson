defmodule MongoRustDecoder.MixProject do
  use Mix.Project

  def project do
    [
      app: :mongodb_rust_decoder,
      version: "0.1.0",
      elixir: "~> 1.15",
      start_permanent: Mix.env() == :prod,
      deps: deps(),
      description: "A Rust (NIF) BSON decoder for mongodb_driver, built on MongoDB's bson crate",
      package: [
        licenses: ["Apache-2.0"],
        links: %{"GitHub" => "https://github.com/commoncurriculum/elixir-bson"},
        files: ~w(lib native/mongodb_rust_decoder/src native/mongodb_rust_decoder/Cargo.toml native/mongodb_rust_decoder/Cargo.lock mix.exs README.md LICENSE)
      ]
    ]
  end

  def application do
    [extra_applications: [:logger]]
  end

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
