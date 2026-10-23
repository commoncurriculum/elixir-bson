defmodule MongoRustDecoder.Native do
  @moduledoc false

  use Rustler, otp_app: :mongodb_rust_decoder, crate: "mongodb_rust_decoder"

  def document(_binary), do: :erlang.nif_error(:nif_not_loaded)
  def document_dirty(_binary), do: :erlang.nif_error(:nif_not_loaded)
  def take(_binary, _count), do: :erlang.nif_error(:nif_not_loaded)
end
