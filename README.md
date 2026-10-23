# mongodb_rust_decoder

A Rust BSON decoder for [`mongodb_driver`](https://github.com/zookzook/elixir-mongodb-driver),
built on MongoDB's official [`bson`](https://crates.io/crates/bson) crate (its zero-copy
`RawDocument` parser) through [Rustler](https://github.com/rusterlium/rustler).

It is a drop-in replacement for `BSON.Decoder`: it returns exactly the same terms. Documents
with values it does not build natively are decoded by `BSON.Decoder` instead, so behaviour does
not change. Those values are Decimal128, dates outside -9999..9999, DBPointer, NaN payloads
other than the usual ones, invalid UTF-8, and duplicate keys.

## Requirements

- `mongodb_driver` 1.4 or later. Any release works, since they all read the `:decoder`
  setting. With the lazy cursor batch change (`BSON.LazyDocuments`, for now on branch
  `claude/project-thread-cdrmxc` of `commoncurriculum/elixir-mongodb-driver`), getMore
  batches are also decoded in chunks while the next batch is fetched. That is the fastest
  setup; see the benchmark below. To use the branch, add
  `{:mongodb_driver, github: "commoncurriculum/elixir-mongodb-driver", branch: "claude/project-thread-cdrmxc", override: true}`.
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
| default | 5.5 s | 0.95 s | 0.33 to 0.38 s |
| 1,000 | 1.5 to 2.0 s | 1.1 s | 0.77 to 0.89 s |
| 10,000 | 4.3 s | 1.0 to 1.15 s | 0.31 to 0.58 s |
| 100,000 | 5.3 to 5.8 s | 2.7 to 2.9 s | 0.51 to 0.77 s |

The first batch of a cursor is decoded in one go, so that `%Mongo.Stream{docs: docs}` stays a
list; later batches are decoded 100 documents at a time while the next one is fetched. A very
large `batch_size` therefore makes the first batch the slow part.

Fetching the same batches without decoding takes about 0.3 s. With a stock `mongodb_driver`
(1.6.4) and MongoRustDecoder, the same read takes 0.6 to 1.0 s at every batch size.

Decoding alone, 300k of these documents on one core: the NIF takes about 0.22 s, against
0.10 s for the `bson` crate just walking and validating the raw bytes in pure Rust, and
0.40 s for the crate building its own `bson::Document`. What remains is building
Erlang terms on the BEAM heap.

```sh
MIX_ENV=bench mix run bench/find.exs
MIX_ENV=bench mix run bench/decode.exs
```

## Tests

`mix test` checks every BSON type against `BSON.Decoder`, including the fallback cases, and runs
a property test over 1,000 random documents.
