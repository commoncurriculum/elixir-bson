import Config

# `MIX_ENV=bench mix run bench/find.exs` runs the driver with this decoder.
if config_env() == :bench do
  config :mongodb_driver, decoder: MongoRustDecoder
end
