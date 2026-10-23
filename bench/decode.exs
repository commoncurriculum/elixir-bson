# Decoder only: one of the report's documents decoded 300k times on one core.
doc = %{"_id" => "subscription_1", "session_id" => "session_1", "conditions" => %{"attributes.planbookId" => ["pb_1"], "attributes.status" => ["published"]}, "negations" => %{}}
binary = doc |> BSON.encode() |> IO.iodata_to_binary()

for _run <- 1..3, decoder <- [BSON.Decoder, MongoRustDecoder] do
  {us, _} = :timer.tc(fn -> Enum.each(1..300_000, fn _ -> decoder.decode(binary) end) end)
  IO.puts("#{inspect(decoder)}: #{div(us, 1000)} ms for 300k decodes")
end
