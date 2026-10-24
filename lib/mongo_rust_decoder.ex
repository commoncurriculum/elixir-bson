defmodule MongoRustDecoder do
  @moduledoc """
  A drop-in replacement for `BSON.Decoder` that decodes documents in Rust, with
  MongoDB's official [`bson`](https://crates.io/crates/bson) crate.

  It returns exactly the same terms as `BSON.Decoder`. Documents containing values it
  does not build natively (Decimal128, dates outside the range `BSON.Decoder` accepts,
  DBPointer, invalid UTF-8, duplicate keys, ...) are decoded by `BSON.Decoder` instead.

  Enable it with:

      config :mongodb_driver, decoder: MongoRustDecoder

  `mongodb_driver` reads this setting at compile time, so recompile it afterwards
  (`mix deps.compile mongodb_driver --force`). Key order is not preserved; use
  `BSON.PreserveOrderDecoder` if you need that.

  If the NIF cannot be loaded (for example, it was built for another platform), loading
  `MongoRustDecoder.Native` fails: a release does not boot, and elsewhere every call raises.
  Reloading the NIF in a running system (hot code upgrade) is not supported.
  """

  alias MongoRustDecoder.Native

  # Only drivers with lazy cursor batches define this, and only those call the hooks below.
  @compile {:no_warn_undefined, [{BSON.Decoder, :lazy_cursor_document, 2}]}

  # A NIF call on a normal scheduler decodes at most this much input, so that it stays under
  # a millisecond (documents decode at 16 to 40 KB/ms). A larger document is decoded on a
  # dirty scheduler, and a chunk of a batch stops before going over it.
  @max_bytes 16 * 1024

  @spec decode(binary) :: BSON.document()
  def decode(binary) do
    {map, ""} = document(binary)
    map
  end

  @spec documents(binary) :: [BSON.document()]
  def documents(binary), do: documents(binary, [])

  defp documents("", acc), do: Enum.reverse(acc)

  defp documents(binary, acc) do
    {doc, rest} = document(binary)
    documents(rest, [doc | acc])
  end

  @spec document(binary) :: {BSON.document(), binary}
  # Decides on the document's own size: the binary may be the rest of a large batch.
  def document(<<size::little-signed-32, _::binary>> = binary) when size > @max_bytes, do: native_result(Native.document_dirty(binary), binary)
  def document(binary) when is_binary(binary), do: native_result(Native.document(binary), binary)

  defp native_result(:fallback, binary), do: BSON.Decoder.document(binary)
  defp native_result(result, _binary), do: result

  @doc false
  # The reply envelope is tiny; only the batch documents are worth decoding natively.
  def lazy_cursor_document(binary), do: BSON.Decoder.lazy_cursor_document(binary, __MODULE__)

  @doc false
  # Called by BSON.LazyDocuments.take/2: decodes a chunk of a batch in one NIF call. The NIF
  # stops early at a document that needs BSON.Decoder or a dirty scheduler; if that is the
  # first one, it is decoded on its own, and the next call carries on.
  def take_lazy_documents(binary, count) do
    case Native.take(binary, count, @max_bytes) do
      {[], _binary} -> take_one(binary)
      result -> result
    end
  end

  defp take_one(<<0x03, rest::binary>>) do
    [_index, rest] = :binary.split(rest, <<0x00>>)
    {doc, rest} = take_one_document(rest)
    {[doc], rest}
  end

  # The NIF did not decode it: it is either too large for a normal scheduler or needs
  # BSON.Decoder.
  defp take_one_document(<<size::little-signed-32, _::binary>> = binary) when size > @max_bytes, do: native_result(Native.document_dirty(binary), binary)
  defp take_one_document(binary), do: BSON.Decoder.document(binary)
end
