# Reads 300k small documents with Mongo.find |> Enum.count, the shape from the original report.
# Needs a MongoDB on 127.0.0.1:27017. Run with MIX_ENV=bench to use MongoRustDecoder.
{:ok, conn} = Mongo.start_link(url: System.get_env("MONGODB_URL", "mongodb://127.0.0.1:27017/test"))
Process.sleep(1_000)
coll = "decode_test"

if Mongo.estimated_document_count(conn, coll, []) != {:ok, 300_000} do
  Mongo.drop_collection(conn, coll)

  1..300_000
  |> Enum.map(fn i ->
    %{_id: "subscription_#{i}", session_id: "session_#{rem(i, 50_000)}", conditions: %{"attributes.planbookId" => ["pb_#{i}"], "attributes.status" => ["published"]}, negations: %{}}
  end)
  |> Enum.chunk_every(10_000)
  |> Enum.each(&Mongo.insert_many(conn, coll, &1))
end

IO.puts("decoder: #{inspect(Application.get_env(:mongodb_driver, :decoder, BSON.Decoder))}")

for run <- 1..3, batch_size <- [nil, 1_000, 10_000, 100_000] do
  opts = if batch_size, do: [batch_size: batch_size], else: []
  {us, count} = :timer.tc(fn -> conn |> Mongo.find(coll, %{}, opts) |> Enum.count() end)
  IO.puts("run #{run} batch_size #{inspect(batch_size)}: #{div(us, 1000)} ms for #{count} docs")
end
