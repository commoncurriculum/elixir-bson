defmodule MongoRustDecoderTest do
  use ExUnit.Case, async: true
  use ExUnitProperties

  import ExUnit.CaptureLog

  defp encode(doc), do: doc |> BSON.encode() |> IO.iodata_to_binary()

  defp error_name(fun) do
    fun.()
    :no_error
  rescue
    error -> error.__struct__
  end

  defp assert_same(binary) do
    expected = BSON.Decoder.decode(binary)
    assert MongoRustDecoder.decode(binary) == expected
    expected
  end

  # A document with an element of the given type and raw value bytes, for types BSON.encode can't produce.
  defp raw_doc(type, value) do
    body = <<type, "v", 0>> <> value
    <<byte_size(body) + 5::little-32>> <> body <> <<0>>
  end

  describe "decode/1 matches BSON.Decoder" do
    test "for every common type" do
      doc = %{
        "string" => "héllo",
        "empty" => "",
        "int32" => 42,
        "negative" => -7,
        "int64" => 9_000_000_000,
        "float" => 1.5,
        "true" => true,
        "false" => false,
        "nil" => nil,
        "list" => [1, "two", [3], %{"four" => 4}],
        "empty_list" => [],
        "map" => %{"nested" => %{"deeper" => "yes"}},
        "empty_map" => %{},
        "object_id" => BSON.ObjectId.new(1, 2, 3, 4),
        "binary" => %BSON.Binary{binary: <<1, 2, 3>>},
        "uuid" => %BSON.Binary{binary: :crypto.strong_rand_bytes(16), subtype: :uuid},
        "md5" => %BSON.Binary{binary: "0123456789abcdef", subtype: :md5},
        "user_binary" => %BSON.Binary{binary: "x", subtype: 0x80},
        "regex" => %BSON.Regex{pattern: "^a.*z$", options: "i"},
        "js" => %BSON.JavaScript{code: "function() { return 1 }"},
        "js_scope" => %BSON.JavaScript{code: "function() { return x }", scope: %{"x" => 1}},
        "timestamp" => %BSON.Timestamp{value: 1_700_000_000, ordinal: 3},
        "date" => ~U[2024-02-29 23:59:59.123Z],
        "old_date" => ~U[1901-01-01 00:00:00.000Z],
        "bc_date" => ~U[-0044-03-15 12:00:00.000Z],
        "inf" => :inf,
        "neg_inf" => :"-inf",
        "nan" => :NaN,
        "min" => :BSON_min,
        "max" => :BSON_max
      }

      decoded = assert_same(encode(doc))
      assert decoded["date"] == doc["date"]
      assert decoded["regex"] == doc["regex"]
    end

    test "for the old binary subtype" do
      assert_same(raw_doc(0x05, <<8::little-32, 0x02, 4::little-32, "abcd">>))
    end

    test "for symbols, undefined and the other NaN encoding" do
      assert_same(raw_doc(0x0E, <<4::little-32, "sym", 0>>))
      assert_same(raw_doc(0x06, <<>>))
      assert_same(raw_doc(0x01, <<0, 0, 0, 0, 0, 0, 248, 255>>))
      assert_same(raw_doc(0x01, <<1, 0, 0, 0, 0, 0, 240, 127>>))
    end

    test "falls back to BSON.Decoder for Decimal128" do
      assert %{"d" => %Decimal{}} = assert_same(encode(%{"d" => Decimal.new("123.456"), "s" => "x"}))
    end

    test "falls back to BSON.Decoder for out-of-range dates, which it clamps and logs" do
      binary = raw_doc(0x09, <<6_312_846_085_200_000::little-signed-64>>)

      log = capture_log(fn -> assert %{"v" => ~U[9999-12-31 23:59:59.999Z]} = assert_same(binary) end)
      assert log =~ "Invalid unix time"
    end

    test "falls back to BSON.Decoder for invalid UTF-8 and duplicate keys" do
      assert_same(raw_doc(0x02, <<3::little-32, 0xFF, 0xFE, 0>>))

      body = <<0x10, "a", 0, 1::little-32, 0x10, "a", 0, 2::little-32>>
      assert_same(<<byte_size(body) + 5::little-32>> <> body <> <<0>>)
    end

    test "raises like BSON.Decoder on unsupported binary subtypes" do
      binary = raw_doc(0x05, <<1::little-32, 0x07, "x">>)
      assert_raise FunctionClauseError, fn -> BSON.Decoder.decode(binary) end
      assert_raise FunctionClauseError, fn -> MongoRustDecoder.decode(binary) end
    end

    test "raises like BSON.Decoder on malformed input" do
      binary = encode(%{"a" => "b"})

      for bad <- [binary_part(binary, 0, byte_size(binary) - 2), "", <<1, 2, 3>>, <<200, 0, 0, 0, 0>>] do
        assert error_name(fn -> BSON.Decoder.decode(bad) end) == error_name(fn -> MongoRustDecoder.decode(bad) end)
      end
    end

    test "deeply nested documents fall back instead of overflowing the stack" do
      for depth <- [100, 127, 300, 1_000, 10_000] do
        nested = Enum.reduce(1..depth, %{"leaf" => 1}, fn _, acc -> %{"d" => acc} end)
        assert_same(encode(nested))

        # Large enough to be decoded on a dirty scheduler, whose stack is smaller.
        padded = Enum.reduce(1..depth, %{"leaf" => String.duplicate("x", 40_000)}, fn _, acc -> %{"d" => acc} end)
        assert_same(encode(padded))
      end
    end

    test "short strings are copied, long ones refer to the reply" do
      %{"short" => short, "long" => long} = MongoRustDecoder.decode(encode(%{"short" => "abc", "long" => String.duplicate("x", 100)}))
      assert :binary.referenced_byte_size(short) == 3
      assert :binary.referenced_byte_size(long) > 100
    end

    test "for large documents decoded on the dirty scheduler" do
      doc = %{"items" => Enum.map(1..5_000, fn i -> %{"i" => i, "s" => String.duplicate("x", 20)} end)}
      binary = encode(doc)
      assert byte_size(binary) > 64 * 1024
      assert_same(binary)
    end
  end

  test "common documents are decoded natively, without falling back" do
    binary = encode(%{"a" => 1, "s" => "x", "l" => [%{"d" => ~U[2024-01-01 00:00:00.000Z]}], "id" => BSON.ObjectId.new(1, 2, 3, 4)})
    assert {%{"a" => 1}, ""} = MongoRustDecoder.Native.document(binary)
    assert :fallback == MongoRustDecoder.Native.document(encode(%{"d" => Decimal.new(1)}))
  end

  test "deeply nested documents decode on the smallest scheduler stacks" do
    # A node whose normal and dirty scheduler stacks are as small as the VM allows.
    args = ~w(+sss 20 +sssdcpu 20)c ++ Enum.flat_map(:code.get_path(), &[~c"-pa", &1])
    {:ok, peer, _node} = :peer.start_link(%{connection: :standard_io, args: args})

    try do
      for depth <- [100, 160, 1_000, 10_000], padding <- [1, 40_000] do
        nested = Enum.reduce(1..depth, %{"leaf" => String.duplicate("x", padding)}, fn i, acc -> if rem(i, 3) == 0, do: %{"l" => [acc]}, else: %{"d" => acc} end)
        binary = encode(nested)
        assert :peer.call(peer, MongoRustDecoder, :decode, [binary], 60_000) == BSON.Decoder.decode(binary)
      end
    after
      :peer.stop(peer)
    end
  end

  # Holds for release builds of the NIF (the default); debug builds use more stack per level.
  test "documents as deep as MongoDB stores them (100 levels) are decoded natively" do
    nested = Enum.reduce(1..100, %{"leaf" => 1}, fn i, acc -> if rem(i, 2) == 0, do: %{"d" => acc}, else: %{"l" => [acc]} end)
    binary = encode(nested)

    assert {decoded, ""} = MongoRustDecoder.Native.document(binary)
    assert decoded == BSON.Decoder.decode(binary)
  end

  test "builds structs with the same fields as the struct definitions" do
    binary =
      encode(%{
        "date" => ~U[2024-01-01 00:00:00.000Z],
        "id" => BSON.ObjectId.new(1, 2, 3, 4),
        "bin" => %BSON.Binary{binary: "x"},
        "re" => %BSON.Regex{pattern: "a"},
        "js" => %BSON.JavaScript{code: "x"},
        "ts" => %BSON.Timestamp{value: 1, ordinal: 2}
      })

    {decoded, ""} = MongoRustDecoder.Native.document(binary)

    for value <- Map.values(decoded) do
      assert Enum.sort(Map.keys(value)) == Enum.sort(Map.keys(struct(value.__struct__)))
    end
  end

  test "raises FunctionClauseError on input that is not a binary, like BSON.Decoder" do
    assert_raise FunctionClauseError, fn -> BSON.Decoder.document(123) end
    assert_raise FunctionClauseError, fn -> MongoRustDecoder.document(123) end
  end

  test "document/1 and documents/1 return the rest like BSON.Decoder" do
    binary = encode(%{"a" => 1}) <> encode(%{"b" => "two"}) <> encode(%{"c" => [3]})

    assert MongoRustDecoder.document(binary) == BSON.Decoder.document(binary)
    assert MongoRustDecoder.documents(binary) == BSON.Decoder.documents(binary)
  end

  defp value_gen do
    leaf =
      one_of([
        string(:printable),
        integer(-2_147_483_648..2_147_483_647),
        integer(-9_223_372_036_854_775_808..9_223_372_036_854_775_807),
        float(),
        boolean(),
        constant(nil),
        member_of([:inf, :"-inf", :NaN, :BSON_min, :BSON_max]),
        map(binary(length: 12), &%BSON.ObjectId{value: &1}),
        map(binary(), &%BSON.Binary{binary: &1}),
        map({string(:alphanumeric), member_of(["", "i", "im", "imsx"])}, fn {p, o} -> %BSON.Regex{pattern: p, options: o} end),
        map({integer(0..2_147_483_647), integer(0..2_147_483_647)}, fn {v, o} -> %BSON.Timestamp{value: v, ordinal: o} end),
        map(integer(-62_135_596_800_000..253_402_300_799_999), &DateTime.from_unix!(&1, :millisecond))
      ])

    tree(leaf, fn child ->
      one_of([list_of(child, max_length: 5), map_of(string(:printable, max_length: 8), child, max_length: 5)])
    end)
  end

  defp doc_gen, do: map_of(string(:printable, max_length: 10), value_gen(), max_length: 8)

  @tag timeout: 300_000
  property "decodes random documents exactly like BSON.Decoder" do
    check all(doc <- doc_gen(), max_runs: 1_000) do
      binary = encode(doc)
      assert_same(binary)
      # The generated values never need BSON.Decoder, so this checks the native path.
      refute MongoRustDecoder.Native.document(binary) == :fallback
    end
  end

  # The array body of a cursor batch, as BSON.LazyDocuments holds it.
  defp batch(docs) do
    <<_size::little-32, 0x04, "b", 0, array_size::little-32, rest::binary>> = encode(%{"b" => docs})
    binary_part(rest, 0, array_size - 5)
  end

  @tag timeout: 300_000
  property "take_lazy_documents/2 decodes a batch in chunks exactly like BSON.Decoder" do
    big = constant(%{"big" => String.duplicate("x", 40_000)})
    decimal = constant(%{"d" => Decimal.new("1.5")})

    check all(docs <- list_of(one_of([doc_gen(), big, decimal]), max_length: 30), count <- integer(1..10), max_runs: 300) do
      chunks =
        Stream.unfold(batch(docs), fn
          "" -> nil
          binary -> MongoRustDecoder.take_lazy_documents(binary, count)
        end)

      chunks = Enum.to_list(chunks)
      assert Enum.all?(chunks, &(length(&1) in 1..count))
      assert Enum.concat(chunks) == Enum.map(docs, &BSON.Decoder.decode(encode(&1)))
    end
  end
end
