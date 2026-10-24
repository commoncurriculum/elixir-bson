# mongodb_rust_decoder

A Rust BSON decoder for [`mongodb_driver`](https://github.com/zookzook/elixir-mongodb-driver),
built on MongoDB's official [`bson`](https://crates.io/crates/bson) crate (its zero-copy
`RawDocument` parser) through [Rustler](https://github.com/rusterlium/rustler).

It is a drop-in replacement for `BSON.Decoder`: it returns exactly the same terms. Documents
with values it does not build natively are decoded by `BSON.Decoder` instead, so behaviour does
not change. Those values are Decimal128, dates outside -9999..9999, DBPointer, NaN payloads
other than the usual ones, binary subtypes `BSON.Decoder` has no clause for (0x06 to 0x7F),
invalid UTF-8, and duplicate keys. So are documents nested so deeply that decoding them would
use more than 64 KB of the scheduler's stack (about 160 levels; MongoDB stores at most 100),
and malformed input, for which `BSON.Decoder` raises its usual error.

Data full of Decimal128 values gains little: each such document is decoded by `BSON.Decoder`.

If the NIF cannot be loaded (for example, it was built for another platform), the decoder
does not quietly fall back: a release fails to boot, and elsewhere every decode raises.
Reloading the NIF in a running system (hot code upgrade) is not supported.

## Requirements

- `mongodb_driver` 1.5 or later. Any such release works, since they all read the `:decoder`
  setting. With the lazy cursor batch change (`BSON.LazyDocuments`, for now on branch
  `claude/project-thread-cdrmxc` of `commoncurriculum/elixir-mongodb-driver`), getMore
  batches are also decoded in chunks, as they are consumed. That is the fastest
  setup; see the benchmark below. To use the branch, add
  `{:mongodb_driver, github: "commoncurriculum/elixir-mongodb-driver", branch: "claude/project-thread-cdrmxc", override: true}`.
- Rust 1.91 or later at compile time.

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
  time (`take_lazy_documents/2`). The NIF decodes the chunk in one call, up to 16 KB of input
  so that the call stays well under a millisecond, and charges that time to the scheduler. It
  allocates each key binary only once per chunk, for the first 64 distinct keys.
- As with `BSON.Decoder`, strings and binaries of up to 64 bytes are copied, and longer ones
  are sub-binaries of the reply. Keys are copied.
- A document over 16 KB is decoded on a dirty CPU scheduler.

## Benchmark

`bench/find.exs` runs `Mongo.find(conn, coll, %{}) |> Enum.count()` over 300,000 documents of
183 bytes each. Setup: local MongoDB 8.0, Elixir 1.17.3, OTP 25.

| Batch size | mongodb_driver 1.6.4 | lazy batches (Elixir) | + MongoRustDecoder |
|---|---|---|---|
| default | 4.1 to 5.5 s | 0.8 to 0.9 s | 0.34 to 0.39 s |
| 1,000 | 1.1 to 2.0 s | 0.8 to 0.95 s | 0.45 to 0.55 s |
| 10,000 | 3.0 to 4.3 s | 0.9 to 1.0 s | 0.37 to 0.44 s |
| 100,000 | 4.3 to 5.8 s | 1.9 to 2.2 s | 0.53 to 0.66 s |

With a batch size of 1,000, the read takes 300 getMore round trips. MongoDB alone needs about
0.27 s to serve them, measured with a raw socket and no driver, so small batches cost the most.
Batches are decoded 100 documents at a time either way, so the default batch size uses no more
memory.

The first batch of a cursor is decoded in one go, so that `%Mongo.Stream{docs: docs}` stays a
list; later batches are decoded 100 documents at a time as they are consumed. A very
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
cargo run --release --manifest-path bench/rust_raw/Cargo.toml   # the pure-Rust numbers
```

## Tests

`mix test` checks every BSON type against `BSON.Decoder`, including the fallback cases, deep
nesting and malformed input. Property tests compare 1,000 random documents, and 300 random
batches decoded in chunks, with `BSON.Decoder`. To check deep nesting on the smallest
scheduler stacks, run `ELIXIR_ERL_OPTIONS="+sss 20 +sssdcpu 20" mix test`.

## License

Apache-2.0, like `mongodb_driver`.
