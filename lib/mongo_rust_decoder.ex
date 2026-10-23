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
  """

  alias MongoRustDecoder.Native

  require Logger

  # Only drivers with lazy cursor batches define these, and only those call the hooks below.
  @compile {:no_warn_undefined, [{BSON.Decoder, :lazy_cursor_document, 1}, {BSON.Decoder, :next_lazy_document, 1}]}

  # Above this size a document is decoded on a dirty scheduler, so a large reply does not
  # block a normal scheduler for too long (dense documents decode at about 40 KB/ms). A
  # chunk of a batch decoded in one call stops after about this much input too.
  @dirty_threshold 32 * 1024

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
  def document(binary) do
    case native?() do
      true -> document_native(binary)
      false -> BSON.Decoder.document(binary)
    end
  end

  defp document_native(<<size::little-signed-32, _::binary>> = binary) when size > @dirty_threshold, do: native_result(Native.document_dirty(binary), binary)
  defp document_native(binary), do: native_result(Native.document(binary), binary)

  defp native_result(:fallback, binary), do: BSON.Decoder.document(binary)
  defp native_result(result, _binary), do: result

  # If the NIF fails to load (for example, built for another platform), MongoRustDecoder.Native
  # is not loaded, and every document is decoded by BSON.Decoder instead of every query failing.
  defp native? do
    case :persistent_term.get(__MODULE__, nil) do
      nil ->
        loaded? = Code.ensure_loaded?(Native)
        unless loaded?, do: Logger.warning("MongoRustDecoder: the NIF is not loaded; decoding with BSON.Decoder instead")
        :persistent_term.put(__MODULE__, loaded?)
        loaded?

      loaded? ->
        loaded?
    end
  end

  @doc false
  # The reply envelope is tiny; only the batch documents are worth decoding natively.
  def lazy_cursor_document(binary) do
    {reply, rest} = BSON.Decoder.lazy_cursor_document(binary)
    {with_decoder(reply), rest}
  end

  defp with_decoder(%{"cursor" => %{} = cursor} = reply) do
    cursor =
      Enum.reduce(["firstBatch", "nextBatch"], cursor, fn key, cursor ->
        case cursor do
          # A map pattern, not a struct one: drivers without lazy batches have no such struct.
          %{^key => %{__struct__: BSON.LazyDocuments} = lazy} -> %{cursor | key => %{lazy | decoder: __MODULE__}}
          _other -> cursor
        end
      end)

    %{reply | "cursor" => cursor}
  end

  defp with_decoder(reply), do: reply

  @doc false
  # Called by BSON.LazyDocuments.take/2: decodes a chunk of a batch in one NIF call. It may
  # return fewer than count documents, to keep the call short.
  def take_lazy_documents(binary, count) do
    case native?() and Native.take(binary, count, @dirty_threshold) do
      fallback when fallback in [:fallback, false] -> take_one_by_one(binary, count, [])
      result -> result
    end
  end

  defp take_one_by_one("", _count, acc), do: {Enum.reverse(acc), ""}
  defp take_one_by_one(binary, 0, acc), do: {Enum.reverse(acc), binary}

  defp take_one_by_one(binary, count, acc) do
    {doc, rest} = next_lazy_document(binary)
    take_one_by_one(rest, count - 1, [doc | acc])
  end

  @doc false
  def next_lazy_document(""), do: nil

  def next_lazy_document(<<0x03, rest::binary>> = binary) do
    case :binary.match(rest, <<0x00>>) do
      {pos, 1} ->
        <<_index::binary-size(pos), 0x00, rest::binary>> = rest
        document(rest)

      # Malformed: let BSON.Decoder raise its usual error.
      :nomatch ->
        BSON.Decoder.next_lazy_document(binary)
    end
  end

  def next_lazy_document(binary), do: BSON.Decoder.next_lazy_document(binary)
end
