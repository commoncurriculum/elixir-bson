//! BSON to Elixir term decoding for `mongodb_driver`, built on MongoDB's official
//! `bson` crate (its zero-copy `RawDocument` parser).
//!
//! The terms match what `BSON.Decoder` returns. Values this decoder does not build
//! itself (Decimal128, dates outside the range `BSON.Decoder` accepts, unusual NaN
//! payloads, binary subtypes `BSON.Decoder` rejects, duplicate keys, invalid UTF-8)
//! make the call return `:fallback`, and the Elixir side then decodes that document
//! with `BSON.Decoder`, so behaviour stays identical. So do documents nested so deeply that
//! the recursion would use more than `STACK_BUDGET` of the scheduler's stack, DBPointer, and
//! malformed input, for which `BSON.Decoder` raises its own error.

use bson::raw::{RawArray, RawBsonRef, RawDocument};
use bson::spec::BinarySubtype;
use rustler::sys::{enif_make_list_from_array, enif_make_map_from_arrays, ErlNifTerm};
use rustler::types::binary::{Binary, NewBinary};
use rustler::{Atom, Encoder, Env, Term};
use smallvec::SmallVec;
use std::mem::MaybeUninit;

/// Native stack a call may use for its recursion. Scheduler stacks can be as small as 160 KB
/// (`+sss 20`, `+sssdcpu 20`), part of which the VM has already used when the NIF is called.
/// Deeper documents are left to `BSON.Decoder`.
const STACK_BUDGET: usize = 64 * 1024;

/// Input decoded per 1% of a scheduler timeslice (1 ms): dense documents decode at about
/// 40 KB/ms.
const BYTES_PER_PERCENT: usize = 400;

/// Array index keys ("0", "1", ...) are at most this long.
const MAX_INDEX_KEY: usize = 20;

/// Strings and binaries up to this size are copied, like `BSON.Decoder` does (the BEAM copies
/// sub-binaries of up to 64 bytes); longer ones are sub-binaries of the reply.
const COPY_LIMIT: usize = 64;

/// The fields of a struct (13 for DateTime, with `__struct__`).
type StructFields = SmallVec<[ErlNifTerm; 13]>;

/// How many distinct keys a call caches.
const KEY_CACHE_SIZE: usize = 64;

/// Key strings and the binary terms made for them.
type KeyCache<'a> = SmallVec<[(&'a str, ErlNifTerm); KEY_CACHE_SIZE]>;

mod atoms {
    rustler::atoms! {
        fallback,
        nil,
        inf,
        neg_inf = "-inf",
        nan = "NaN",
        bson_min = "BSON_min",
        bson_max = "BSON_max",
        generic,
        function,
        binary_old,
        uuid_old,
        uuid,
        md5,
        __struct__,
        bson_binary = "Elixir.BSON.Binary",
        bson_object_id = "Elixir.BSON.ObjectId",
        bson_regex = "Elixir.BSON.Regex",
        bson_javascript = "Elixir.BSON.JavaScript",
        bson_timestamp = "Elixir.BSON.Timestamp",
        date_time = "Elixir.DateTime",
        calendar_iso = "Elixir.Calendar.ISO",
        binary,
        subtype,
        value,
        pattern,
        options,
        code,
        scope,
        ordinal,
        calendar,
        year,
        month,
        day,
        hour,
        minute,
        second,
        microsecond,
        std_offset,
        utc_offset,
        time_zone,
        zone_abbr,
    }
}

/// `None` means: hand this document to `BSON.Decoder` (which also raises on invalid input).
type Decoded<'a> = Option<Term<'a>>;

struct Ctx<'a> {
    env: Env<'a>,
    /// The binary term being decoded; long strings become sub-binaries of it.
    source: Binary<'a>,
    /// Key binaries already made in this call: the documents of a batch share their keys.
    key_cache: KeyCache<'a>,
    /// The keys and values of the documents and arrays being decoded, innermost last. One
    /// buffer for the whole call keeps the recursion's stack frames small, and is reused by
    /// every document.
    keys: Vec<ErlNifTerm>,
    values: Vec<ErlNifTerm>,
    /// The "Etc/UTC" and "UTC" binaries of DateTime structs, made once per call.
    utc: Option<(Term<'a>, Term<'a>)>,
    /// The stack address below which the recursion stops (stacks grow down).
    stack_limit: usize,
}

impl<'a> Ctx<'a> {
    fn new(env: Env<'a>, source: Binary<'a>) -> Self {
        Ctx {
            env,
            source,
            key_cache: SmallVec::new(),
            keys: Vec::with_capacity(64),
            values: Vec::with_capacity(64),
            utc: None,
            stack_limit: stack_address().saturating_sub(STACK_BUDGET),
        }
    }

    fn key(&mut self, key: &'a str) -> ErlNifTerm {
        if let Some((_, term)) = self.key_cache.iter().find(|(k, _)| *k == key) {
            return *term;
        }
        // Copied, like BSON.Decoder does, so keys don't keep the reply binary alive.
        let term = copied_binary(self.env, key.as_bytes()).as_c_arg();
        if self.key_cache.len() < KEY_CACHE_SIZE {
            self.key_cache.push((key, term));
        }
        term
    }

    /// A string or binary value: a sub-binary of the source if it is long, so it is not copied.
    fn value_binary(&self, bytes: &'a [u8]) -> Term<'a> {
        if bytes.len() <= COPY_LIMIT {
            return copied_binary(self.env, bytes);
        }
        let offset = (bytes.as_ptr() as usize)
            .checked_sub(self.source.as_slice().as_ptr() as usize)
            .expect("decoded slices lie within the source binary");
        // SAFETY: every slice decoded here lies within the source binary.
        unsafe { self.source.make_subbinary_unchecked(offset, bytes.len()) }.encode(self.env)
    }

    fn document(&mut self, doc: &'a RawDocument) -> Decoded<'a> {
        if stack_address() < self.stack_limit {
            return None;
        }
        let (keys_start, values_start) = (self.keys.len(), self.values.len());
        for element in doc {
            let (key, value) = element.ok()?;
            let key = self.key(key.as_str());
            self.keys.push(key);
            // A nested document or array pushes its own fields, and pops them again.
            let value = self.value(value)?;
            self.values.push(value.as_c_arg());
        }
        // Fails on duplicate keys; BSON.Decoder keeps the first one, so let it decide.
        let map = make_map(
            self.env,
            &self.keys[keys_start..],
            &self.values[values_start..],
        );
        self.keys.truncate(keys_start);
        self.values.truncate(values_start);
        map
    }

    fn array(&mut self, array: &'a RawArray) -> Decoded<'a> {
        if stack_address() < self.stack_limit {
            return None;
        }
        let values_start = self.values.len();
        for element in array {
            let value = self.value(element.ok()?)?;
            self.values.push(value.as_c_arg());
        }
        let list = make_list(self.env, &self.values[values_start..]);
        self.values.truncate(values_start);
        list
    }

    /// Only the recursive cases are handled here, so that this frame, which is on the stack
    /// once per nesting level, stays small.
    fn value(&mut self, value: RawBsonRef<'a>) -> Decoded<'a> {
        match value {
            RawBsonRef::Document(d) => self.document(d),
            RawBsonRef::Array(a) => self.array(a),
            RawBsonRef::JavaScriptCodeWithScope(js) => {
                let scope = self.document(js.scope)?;
                Some(self.make_struct(
                    atoms::bson_javascript(),
                    &[
                        (atoms::code(), self.value_binary(js.code.as_bytes())),
                        (atoms::scope(), scope),
                    ],
                ))
            }
            value => self.scalar(value),
        }
    }

    #[inline(never)]
    fn make_struct(&self, name: Atom, fields: &[(Atom, Term<'a>)]) -> Term<'a> {
        let env = self.env;
        let mut keys = StructFields::new();
        let mut values = StructFields::new();
        keys.push(atoms::__struct__().to_term(env).as_c_arg());
        values.push(name.to_term(env).as_c_arg());
        for (k, v) in fields {
            keys.push(k.to_term(env).as_c_arg());
            values.push(v.as_c_arg());
        }
        make_map(env, &keys, &values).expect("struct fields are distinct atoms")
    }

    #[inline(never)]
    fn scalar(&mut self, value: RawBsonRef<'a>) -> Decoded<'a> {
        let env = self.env;
        Some(match value {
            RawBsonRef::Double(f) => double(env, f)?,
            RawBsonRef::String(s) | RawBsonRef::Symbol(s) => self.value_binary(s.as_bytes()),
            RawBsonRef::Boolean(b) => b.encode(env),
            RawBsonRef::Null | RawBsonRef::Undefined => atoms::nil().encode(env),
            RawBsonRef::Int32(i) => i.encode(env),
            RawBsonRef::Int64(i) => i.encode(env),
            RawBsonRef::ObjectId(oid) => self.make_struct(
                atoms::bson_object_id(),
                &[(atoms::value(), copied_binary(env, &oid.bytes()))],
            ),
            RawBsonRef::Binary(bin) => self.make_struct(
                atoms::bson_binary(),
                &[
                    (atoms::binary(), self.value_binary(bin.bytes)),
                    (atoms::subtype(), binary_subtype(env, bin.subtype)?),
                ],
            ),
            RawBsonRef::RegularExpression(re) => self.make_struct(
                atoms::bson_regex(),
                &[
                    (
                        atoms::pattern(),
                        copied_binary(env, re.pattern.as_str().as_bytes()),
                    ),
                    (
                        atoms::options(),
                        copied_binary(env, re.options.as_str().as_bytes()),
                    ),
                ],
            ),
            RawBsonRef::JavaScriptCode(code) => self.make_struct(
                atoms::bson_javascript(),
                &[
                    (atoms::code(), self.value_binary(code.as_bytes())),
                    (atoms::scope(), atoms::nil().encode(env)),
                ],
            ),
            // BSON.Decoder reads both halves as signed 32-bit integers.
            RawBsonRef::Timestamp(ts) => self.make_struct(
                atoms::bson_timestamp(),
                &[
                    (atoms::value(), (ts.time as i32).encode(env)),
                    (atoms::ordinal(), (ts.increment as i32).encode(env)),
                ],
            ),
            RawBsonRef::DateTime(dt) => self.date_time(dt.timestamp_millis())?,
            RawBsonRef::MinKey => atoms::bson_min().encode(env),
            RawBsonRef::MaxKey => atoms::bson_max().encode(env),
            RawBsonRef::Decimal128(_) | RawBsonRef::DbPointer(_) => return None,
            RawBsonRef::Document(_)
            | RawBsonRef::Array(_)
            | RawBsonRef::JavaScriptCodeWithScope(_) => unreachable!("handled by value()"),
        })
    }

    fn date_time(&mut self, unix_ms: i64) -> Decoded<'a> {
        // BSON.Decoder clamps (and logs) outside -9999-01-01..9999-12-31; leave that to it.
        const UNIX_START: i64 = -377_705_116_800_000;
        const UNIX_END: i64 = 253_402_300_799_999;
        if !(UNIX_START..=UNIX_END).contains(&unix_ms) {
            return None;
        }
        let env = self.env;
        let (y, m, d) = civil_from_days(unix_ms.div_euclid(86_400_000));
        let ms_of_day = unix_ms.rem_euclid(86_400_000);
        let secs = ms_of_day / 1000;
        let micro = (ms_of_day % 1000) * 1000;
        let (time_zone, zone_abbr) = *self
            .utc
            .get_or_insert_with(|| (copied_binary(env, b"Etc/UTC"), copied_binary(env, b"UTC")));
        Some(self.make_struct(
            atoms::date_time(),
            &[
                (atoms::calendar(), atoms::calendar_iso().encode(env)),
                (atoms::year(), y.encode(env)),
                (atoms::month(), m.encode(env)),
                (atoms::day(), d.encode(env)),
                (atoms::hour(), (secs / 3600).encode(env)),
                (atoms::minute(), (secs / 60 % 60).encode(env)),
                (atoms::second(), (secs % 60).encode(env)),
                (atoms::microsecond(), (micro, 3).encode(env)),
                (atoms::std_offset(), 0.encode(env)),
                (atoms::utc_offset(), 0.encode(env)),
                (atoms::time_zone(), time_zone),
                (atoms::zone_abbr(), zone_abbr),
            ],
        ))
    }
}

fn copied_binary<'a>(env: Env<'a>, bytes: &[u8]) -> Term<'a> {
    let mut bin = NewBinary::new(env, bytes.len());
    bin.as_mut_slice().copy_from_slice(bytes);
    Term::from(bin)
}

/// enif_make_map_from_arrays without the extra copies of rustler's wrapper. `None` on duplicate keys.
fn make_map<'a>(env: Env<'a>, keys: &[ErlNifTerm], values: &[ErlNifTerm]) -> Option<Term<'a>> {
    let mut map = MaybeUninit::uninit();
    // SAFETY: both slices hold valid terms of this env and have the same length.
    let ok = unsafe {
        enif_make_map_from_arrays(
            env.as_c_arg(),
            keys.as_ptr(),
            values.as_ptr(),
            keys.len(),
            map.as_mut_ptr(),
        )
    };
    (ok != 0).then(|| unsafe { Term::new(env, map.assume_init()) })
}

fn make_list<'a>(env: Env<'a>, items: &[ErlNifTerm]) -> Option<Term<'a>> {
    let len = u32::try_from(items.len()).ok()?;
    // SAFETY: every element is a valid term of this env.
    Some(unsafe {
        Term::new(
            env,
            enif_make_list_from_array(env.as_c_arg(), items.as_ptr(), len),
        )
    })
}

/// The current stack address, approximately.
#[inline(always)]
fn stack_address() -> usize {
    let marker = 0u8;
    std::hint::black_box(&marker) as *const u8 as usize
}

/// Charges the scheduler for the time spent decoding `bytes` of input.
fn consume_timeslice(env: Env<'_>, bytes: usize) {
    let percent = (bytes / BYTES_PER_PERCENT).clamp(1, 100);
    rustler::schedule::consume_timeslice(env, percent as i32);
}

fn binary_subtype(env: Env<'_>, subtype: BinarySubtype) -> Decoded<'_> {
    Some(match subtype {
        BinarySubtype::Generic => atoms::generic().encode(env),
        BinarySubtype::Function => atoms::function().encode(env),
        BinarySubtype::BinaryOld => atoms::binary_old().encode(env),
        BinarySubtype::UuidOld => atoms::uuid_old().encode(env),
        BinarySubtype::Uuid => atoms::uuid().encode(env),
        BinarySubtype::Md5 => atoms::md5().encode(env),
        BinarySubtype::UserDefined(n) => n.encode(env),
        // BSON.Decoder has no clause for these subtypes; keep its behaviour.
        _ => return None,
    })
}

fn double(env: Env<'_>, f: f64) -> Decoded<'_> {
    if f.is_nan() {
        // BSON.Decoder only recognises these NaN encodings.
        return match f.to_bits() {
            0x7FF8_0000_0000_0000 | 0xFFF8_0000_0000_0000 | 0x7FF0_0000_0000_0001 => {
                Some(atoms::nan().encode(env))
            }
            _ => None,
        };
    }
    Some(if f == f64::INFINITY {
        atoms::inf().encode(env)
    } else if f == f64::NEG_INFINITY {
        atoms::neg_inf().encode(env)
    } else {
        f.encode(env)
    })
}

/// Howard Hinnant's days-to-civil algorithm, valid for the whole proleptic Gregorian calendar.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The document framed at the start of `bytes` (by its int32 size header).
fn framed(bytes: &[u8]) -> Option<&RawDocument> {
    let size = i32::from_le_bytes(bytes.get(..4)?.try_into().ok()?);
    RawDocument::from_bytes(bytes.get(..usize::try_from(size).ok()?)?).ok()
}

/// The document of the array element at the start of `bytes` (type 0x03, then its index key).
fn element_document(bytes: &[u8]) -> Option<(usize, &RawDocument)> {
    let (&type_byte, rest) = bytes.split_first()?;
    if type_byte != 0x03 {
        return None;
    }
    let key_len = rest.iter().take(MAX_INDEX_KEY + 1).position(|&b| b == 0)?;
    let start = 1 + key_len + 1;
    framed(&bytes[start..]).map(|doc| (start, doc))
}

/// `dirty`: on a dirty scheduler, which has no timeslice to charge.
fn decode_one<'a>(env: Env<'a>, binary: Binary<'a>, dirty: bool) -> Term<'a> {
    let bytes: &'a [u8] = binary.as_slice();
    let mut ctx = Ctx::new(env, binary);
    let Some(doc) = framed(bytes) else {
        return atoms::fallback().encode(env);
    };
    let size = doc.as_bytes().len();
    if !dirty {
        consume_timeslice(env, size);
    }
    match ctx.document(doc) {
        // SAFETY: size <= bytes.len().
        Some(term) => (term, unsafe {
            binary.make_subbinary_unchecked(size, bytes.len() - size)
        })
            .encode(env),
        None => atoms::fallback().encode(env),
    }
}

/// Decodes the BSON document at the start of `binary`. Returns `{map, rest}`, or `:fallback`
/// (also for input that is not BSON, so that `BSON.Decoder` raises its usual error).
#[rustler::nif]
fn document<'a>(env: Env<'a>, binary: Binary<'a>) -> Term<'a> {
    decode_one(env, binary, false)
}

/// Same as `document/1`, on a dirty CPU scheduler, for large inputs.
#[rustler::nif(schedule = "DirtyCpu")]
fn document_dirty<'a>(env: Env<'a>, binary: Binary<'a>) -> Term<'a> {
    decode_one(env, binary, true)
}

/// Decodes up to `count` documents from the body of a BSON array (the raw batch held by
/// `BSON.LazyDocuments`), within `max_bytes` of input so a call stays short. Returns
/// `{docs, rest}`; it stops early, before the element at the start of `rest`, at an element
/// it cannot decode natively or that does not fit the budget, which the caller decodes on its own.
#[rustler::nif]
fn take<'a>(env: Env<'a>, binary: Binary<'a>, count: usize, max_bytes: usize) -> Term<'a> {
    let bytes: &'a [u8] = binary.as_slice();
    let mut ctx = Ctx::new(env, binary);
    let mut docs: SmallVec<[ErlNifTerm; 128]> = SmallVec::new();
    let mut pos = 0;
    while docs.len() < count {
        let Some((start, doc)) = element_document(&bytes[pos..]) else {
            break;
        };
        let end = pos + start + doc.as_bytes().len();
        if end > max_bytes {
            break;
        }
        let Some(term) = ctx.document(doc) else {
            break;
        };
        docs.push(term.as_c_arg());
        pos = end;
    }
    consume_timeslice(env, pos);
    // SAFETY: pos <= bytes.len().
    let rest = unsafe { binary.make_subbinary_unchecked(pos, bytes.len() - pos) };
    let docs = make_list(env, &docs).expect("count is a small chunk size");
    (docs, rest).encode(env)
}

rustler::init!("Elixir.MongoRustDecoder.Native");
