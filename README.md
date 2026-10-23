# mongodb_rust_decoder

A Rust BSON decoder for [`mongodb_driver`](https://github.com/zookzook/elixir-mongodb-driver),
built on MongoDB's official [`bson`](https://crates.io/crates/bson) crate (its zero-copy
`RawDocument` parser) through [Rustler](https://github.com/rusterlium/rustler).

It is a drop-in replacement for `BSON.Decoder`: it returns exactly the same terms. Documents
with values it does not build natively are decoded by `BSON.Decoder` instead, so behaviour does
not change. Those values are Decimal128, dates outside -9999..9999, DBPointer, NaN payloads
other than the usual ones, invalid UTF-8, and duplicate keys.

## Requirements

- The lazy cursor batch change to `mongodb_driver` (`BSON.LazyDocuments`). For now it lives on
  branch `claude/project-thread-cdrmxc` of `commoncurriculum/elixir-mongodb-driver`.
- A Rust toolchain at compile time.

## Usage

```elixir
# mix.exs
{:mongodb_rust_decoder, github: "commoncurriculum/elixir-bson"}

# config/config.exs
config :mongodb_driver, decoder: MongoRustDecoder
```

`mongodb_driver` reads the decoder at compile time, so run
`mix deps.compile mongodb_driver --force` after changing it.

Key order is not preserved. Use `BSON.PreserveOrderDecoder` if you need that.

## How it works

- `Mongo.Stream` keeps each cursor batch as raw BSON and asks the decoder for 100 documents at a
  time (`take_lazy_documents/2`). The NIF decodes the whole chunk in one call and allocates
  each distinct key binary only once per chunk.
- String values are sub-binaries of the reply, as with `BSON.Decoder`. Keys are copied.
- A document over 64 KB is decoded on a dirty CPU scheduler.

## Benchmark

`bench/find.exs` runs `Mongo.find(conn, coll, %{}) |> Enum.count()` over 300,000 documents of
about 256 bytes each. Setup: local MongoDB 8.0, Elixir 1.17.3, OTP 25.

| Batch size | mongodb_driver 1.6.4 | lazy batches (Elixir) | + MongoRustDecoder |
|---|---|---|---|
| default | 5.5 s | 1.25 s | 0.47 s |
| 1,000 | 1.5 to 2.0 s | 1.4 s | 0.63 s |
| 10,000 | 4.3 s | 1.15 s | 0.52 s |
| 100,000 | 5.3 to 5.8 s | 1.1 to 1.2 s | 0.43 to 0.6 s |

Fetching the same batches without decoding takes about 0.3 s.

```sh
MIX_ENV=bench mix run bench/find.exs
MIX_ENV=bench mix run bench/decode.exs
```

## Tests

`mix test` checks every BSON type against `BSON.Decoder`, including the fallback cases, and runs
a property test over 1,000 random documents.
