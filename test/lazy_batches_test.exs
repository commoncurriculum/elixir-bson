# Only drivers with lazy cursor batches have BSON.LazyDocuments.
if Code.ensure_loaded?(BSON.LazyDocuments) do
  defmodule MongoRustDecoderLazyTest do
    use ExUnit.Case, async: true

    defp encode(doc), do: doc |> BSON.encode() |> IO.iodata_to_binary()

    test "cursor batches stay lazy and decode with the Rust decoder" do
      docs = Enum.map(1..300, fn i -> %{"i" => i, "name" => "doc #{i}", "tags" => ["a"]} end)
      binary = encode(%{"cursor" => %{"firstBatch" => docs, "id" => 0, "ns" => "db.c"}, "ok" => 1.0})

      {reply, ""} = MongoRustDecoder.lazy_cursor_document(binary)
      assert %BSON.LazyDocuments{decoder: MongoRustDecoder} = lazy = reply["cursor"]["firstBatch"]
      assert Enum.to_list(lazy) == docs
      assert {first, %BSON.LazyDocuments{} = rest} = BSON.LazyDocuments.take(lazy, 100)
      assert first == Enum.take(docs, 100)
      assert {second, %BSON.LazyDocuments{} = rest} = BSON.LazyDocuments.take(rest, 150)
      assert {third, []} = BSON.LazyDocuments.take(rest, 100)
      assert first ++ second ++ third == docs
    end

    test "a batch chunk that needs BSON.Decoder is decoded one by one" do
      docs = [%{"a" => 1}, %{"d" => Decimal.new("1.5")}, %{"c" => 3}]
      binary = encode(%{"cursor" => %{"nextBatch" => docs, "id" => 0, "ns" => "db.c"}, "ok" => 1.0})

      {reply, ""} = MongoRustDecoder.lazy_cursor_document(binary)
      lazy = reply["cursor"]["nextBatch"]
      assert :fallback == MongoRustDecoder.Native.take(lazy.binary, 10, 32 * 1024)
      assert {^docs, []} = BSON.LazyDocuments.take(lazy, 10)
    end

    test "a chunk holds at most 32 KB of input, and a larger first document is decoded on its own" do
      docs = Enum.map(1..10, fn i -> %{"i" => i, "s" => String.duplicate("x", 10_000)} end) ++ [%{"big" => String.duplicate("y", 100_000)}, %{"i" => 11}]
      binary = encode(%{"cursor" => %{"nextBatch" => docs, "id" => 0, "ns" => "db.c"}, "ok" => 1.0})
      {reply, ""} = MongoRustDecoder.lazy_cursor_document(binary)
      lazy = reply["cursor"]["nextBatch"]

      {first, rest} = BSON.LazyDocuments.take(lazy, 100)
      assert first == Enum.take(docs, 3)
      assert Enum.to_list(rest) == Enum.drop(docs, 3)
      assert {[_, _, _], rest} = BSON.LazyDocuments.take(rest, 100)
      assert {[_, _, _], rest} = BSON.LazyDocuments.take(rest, 100)
      assert {[%{"i" => 10}], rest} = BSON.LazyDocuments.take(rest, 100)
      assert :fallback == MongoRustDecoder.Native.take(rest.binary, 100, 32 * 1024)
      assert {[%{"big" => _}, %{"i" => 11}], []} = BSON.LazyDocuments.take(rest, 100)
    end
  end
end
