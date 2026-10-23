//! BSON to Elixir term decoding for `mongodb_driver`, built on MongoDB's official
//! `bson` crate (its zero-copy `RawDocument` parser).
//!
//! The terms match what `BSON.Decoder` returns. Values this decoder does not build
//! itself (Decimal128, dates outside the range `BSON.Decoder` accepts, unusual NaN
//! payloads, binary subtypes `BSON.Decoder` rejects, duplicate keys, invalid UTF-8)
//! make the call return `:fallback`, and the Elixir side then decodes that document
//! with `BSON.Decoder`, so behaviour stays identical.

use bson::raw::{RawArray, RawBsonRef, RawDocument};
use bson::spec::BinarySubtype;
use rustler::sys::{enif_make_list_from_array, enif_make_map_from_arrays, ErlNifTerm};
use rustler::types::binary::{Binary, NewBinary};
use rustler::{Atom, Encoder, Env, NifResult, Term};
use smallvec::SmallVec;
use std::cell::RefCell;
use std::mem::MaybeUninit;

/// Most documents have few fields, so keep their terms on the stack.
type Terms = SmallVec<[ErlNifTerm; 16]>;

/// Key bytes and the binary term made for them.
type KeyCache<'a> = SmallVec<[(&'a [u8], ErlNifTerm); 32]>;

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

/// Why decoding stopped: either hand the document to `BSON.Decoder`, or the input is not valid BSON.
enum Stop {
    Fallback,
    Invalid,
}

type DResult<T> = Result<T, Stop>;

struct Ctx<'a> {
    env: Env<'a>,
    /// The binary term being decoded; strings become sub-binaries of it.
    source: Binary<'a>,
    base: usize,
    /// Key binaries already made in this call. Documents of a batch share their keys,
    /// so each distinct key is allocated once per call instead of once per document.
    keys: RefCell<KeyCache<'a>>,
}

impl<'a> Ctx<'a> {
    fn new(env: Env<'a>, source: Binary<'a>) -> Self {
        let base = source.as_slice().as_ptr() as usize;
        Ctx {
            env,
            source,
            base,
            keys: RefCell::new(SmallVec::new()),
        }
    }

    fn key(&self, key: &[u8]) -> ErlNifTerm {
        let mut cache = self.keys.borrow_mut();
        if let Some((_, term)) = cache.iter().find(|(k, _)| *k == key) {
            return *term;
        }
        let term = self.copied_binary(key).as_c_arg();
        if cache.len() < 64 {
            // Keys point into the source binary, which outlives this call's Ctx.
            let offset = (key.as_ptr() as usize).wrapping_sub(self.base);
            if offset + key.len() <= self.source.len() {
                cache.push((&self.source.as_slice()[offset..offset + key.len()], term));
            }
        }
        term
    }

    /// A sub-binary of the source for a slice that points into it, so string values are not copied.
    fn sub_binary(&self, bytes: &[u8]) -> DResult<Term<'a>> {
        let offset = (bytes.as_ptr() as usize).wrapping_sub(self.base);
        if offset + bytes.len() > self.source.len() {
            return Ok(self.copied_binary(bytes));
        }
        // SAFETY: the bounds were checked just above.
        Ok(unsafe { self.source.make_subbinary_unchecked(offset, bytes.len()) }.encode(self.env))
    }

    fn copied_binary(&self, bytes: &[u8]) -> Term<'a> {
        let mut bin = NewBinary::new(self.env, bytes.len());
        bin.as_mut_slice().copy_from_slice(bytes);
        Term::from(bin)
    }

    /// enif_make_map_from_arrays without the extra copies of rustler's wrapper. `None` on duplicate keys.
    fn map(&self, keys: &[ErlNifTerm], values: &[ErlNifTerm]) -> Option<Term<'a>> {
        let mut map = MaybeUninit::uninit();
        // SAFETY: both slices hold valid terms of this env and have the same length.
        let ok = unsafe {
            enif_make_map_from_arrays(
                self.env.as_c_arg(),
                keys.as_ptr(),
                values.as_ptr(),
                keys.len(),
                map.as_mut_ptr(),
            )
        };
        (ok != 0).then(|| unsafe { Term::new(self.env, map.assume_init()) })
    }

    fn make_struct(&self, name: Atom, fields: &[(Atom, Term<'a>)]) -> DResult<Term<'a>> {
        let mut keys: Terms = SmallVec::new();
        let mut values: Terms = SmallVec::new();
        keys.push(atoms::__struct__().to_term(self.env).as_c_arg());
        values.push(name.to_term(self.env).as_c_arg());
        for (k, v) in fields {
            keys.push(k.to_term(self.env).as_c_arg());
            values.push(v.as_c_arg());
        }
        self.map(&keys, &values).ok_or(Stop::Invalid)
    }

    fn document(&self, doc: &RawDocument) -> DResult<Term<'a>> {
        let mut keys: Terms = SmallVec::new();
        let mut values: Terms = SmallVec::new();
        for element in doc {
            let (key, value) = element.map_err(|_| Stop::Fallback)?;
            // Keys are copied, like BSON.Decoder does, so they don't keep the reply binary alive.
            keys.push(self.key(key.as_str().as_bytes()));
            values.push(self.value(value)?.as_c_arg());
        }
        // Fails on duplicate keys; BSON.Decoder keeps the first one, so let it decide.
        self.map(&keys, &values).ok_or(Stop::Fallback)
    }

    fn array(&self, array: &RawArray) -> DResult<Term<'a>> {
        let mut values: Terms = SmallVec::new();
        for element in array {
            values.push(self.value(element.map_err(|_| Stop::Fallback)?)?.as_c_arg());
        }
        // SAFETY: every element is a valid term of this env.
        Ok(unsafe {
            Term::new(
                self.env,
                enif_make_list_from_array(
                    self.env.as_c_arg(),
                    values.as_ptr(),
                    values.len() as u32,
                ),
            )
        })
    }

    fn value(&self, value: RawBsonRef<'_>) -> DResult<Term<'a>> {
        let env = self.env;
        Ok(match value {
            RawBsonRef::Double(f) => double(env, f)?,
            RawBsonRef::String(s) | RawBsonRef::Symbol(s) => self.sub_binary(s.as_bytes())?,
            RawBsonRef::Document(d) => self.document(d)?,
            RawBsonRef::Array(a) => self.array(a)?,
            RawBsonRef::Boolean(b) => b.encode(env),
            RawBsonRef::Null | RawBsonRef::Undefined => atoms::nil().encode(env),
            RawBsonRef::Int32(i) => i.encode(env),
            RawBsonRef::Int64(i) => i.encode(env),
            RawBsonRef::ObjectId(oid) => self.make_struct(
                atoms::bson_object_id(),
                &[(atoms::value(), self.copied_binary(&oid.bytes()))],
            )?,
            RawBsonRef::Binary(bin) => {
                let subtype = match bin.subtype {
                    BinarySubtype::Generic => atoms::generic().encode(env),
                    BinarySubtype::Function => atoms::function().encode(env),
                    BinarySubtype::BinaryOld => atoms::binary_old().encode(env),
                    BinarySubtype::UuidOld => atoms::uuid_old().encode(env),
                    BinarySubtype::Uuid => atoms::uuid().encode(env),
                    BinarySubtype::Md5 => atoms::md5().encode(env),
                    BinarySubtype::UserDefined(n) => n.encode(env),
                    // BSON.Decoder has no clause for these subtypes; keep its behaviour.
                    _ => return Err(Stop::Fallback),
                };
                self.make_struct(
                    atoms::bson_binary(),
                    &[
                        (atoms::binary(), self.sub_binary(bin.bytes)?),
                        (atoms::subtype(), subtype),
                    ],
                )?
            }
            RawBsonRef::RegularExpression(re) => self.make_struct(
                atoms::bson_regex(),
                &[
                    (
                        atoms::pattern(),
                        self.copied_binary(re.pattern.as_str().as_bytes()),
                    ),
                    (
                        atoms::options(),
                        self.copied_binary(re.options.as_str().as_bytes()),
                    ),
                ],
            )?,
            RawBsonRef::JavaScriptCode(code) => self.make_struct(
                atoms::bson_javascript(),
                &[
                    (atoms::code(), self.sub_binary(code.as_bytes())?),
                    (atoms::scope(), atoms::nil().encode(env)),
                ],
            )?,
            RawBsonRef::JavaScriptCodeWithScope(js) => self.make_struct(
                atoms::bson_javascript(),
                &[
                    (atoms::code(), self.sub_binary(js.code.as_bytes())?),
                    (atoms::scope(), self.document(js.scope)?),
                ],
            )?,
            // BSON.Decoder reads both halves as signed 32-bit integers.
            RawBsonRef::Timestamp(ts) => self.make_struct(
                atoms::bson_timestamp(),
                &[
                    (atoms::value(), (ts.time as i32).encode(env)),
                    (atoms::ordinal(), (ts.increment as i32).encode(env)),
                ],
            )?,
            RawBsonRef::DateTime(dt) => self.date_time(dt.timestamp_millis())?,
            RawBsonRef::MinKey => atoms::bson_min().encode(env),
            RawBsonRef::MaxKey => atoms::bson_max().encode(env),
            RawBsonRef::Decimal128(_) | RawBsonRef::DbPointer(_) => return Err(Stop::Fallback),
        })
    }

    fn date_time(&self, unix_ms: i64) -> DResult<Term<'a>> {
        // BSON.Decoder clamps (and logs) outside -9999-01-01..9999-12-31; leave that to it.
        const UNIX_START: i64 = -377_705_116_800_000;
        const UNIX_END: i64 = 253_402_300_799_999;
        if !(UNIX_START..=UNIX_END).contains(&unix_ms) {
            return Err(Stop::Fallback);
        }
        let env = self.env;
        let days = unix_ms.div_euclid(86_400_000);
        let ms_of_day = unix_ms.rem_euclid(86_400_000);
        let (y, m, d) = civil_from_days(days);
        let secs = ms_of_day / 1000;
        let micro = (ms_of_day % 1000) * 1000;
        self.make_struct(
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
                (atoms::time_zone(), self.copied_binary(b"Etc/UTC")),
                (atoms::zone_abbr(), self.copied_binary(b"UTC")),
            ],
        )
    }
}

fn double(env: Env<'_>, f: f64) -> DResult<Term<'_>> {
    if f.is_nan() {
        // BSON.Decoder only recognises these NaN encodings.
        return match f.to_bits() {
            0x7FF8_0000_0000_0000 | 0xFFF8_0000_0000_0000 | 0x7FF0_0000_0000_0001 => {
                Ok(atoms::nan().encode(env))
            }
            _ => Err(Stop::Fallback),
        };
    }
    if f == f64::INFINITY {
        return Ok(atoms::inf().encode(env));
    }
    if f == f64::NEG_INFINITY {
        return Ok(atoms::neg_inf().encode(env));
    }
    Ok(f.encode(env))
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

/// Decodes the BSON document at the start of `binary`. Returns `{map, rest}`, `:fallback`,
/// or raises `ArgumentError` for input that is not BSON.
fn document_impl<'a>(env: Env<'a>, binary: Binary<'a>) -> NifResult<Term<'a>> {
    let bytes = binary.as_slice();
    if bytes.len() < 5 {
        return Err(rustler::Error::BadArg);
    }
    let size = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if size < 5 || size as usize > bytes.len() {
        return Err(rustler::Error::BadArg);
    }
    let size = size as usize;
    let ctx = Ctx::new(env, binary);
    let doc = match RawDocument::from_bytes(&bytes[..size]) {
        Ok(doc) => doc,
        Err(_) => return Ok(atoms::fallback().encode(env)),
    };
    match ctx.document(doc) {
        Ok(term) => {
            let rest = binary.make_subbinary(size, bytes.len() - size)?;
            Ok((term, rest).encode(env))
        }
        Err(Stop::Fallback) => Ok(atoms::fallback().encode(env)),
        Err(Stop::Invalid) => Err(rustler::Error::BadArg),
    }
}

#[rustler::nif]
fn document<'a>(env: Env<'a>, binary: Binary<'a>) -> NifResult<Term<'a>> {
    document_impl(env, binary)
}

/// Same as `document/1`, on a dirty CPU scheduler, for large inputs.
#[rustler::nif(schedule = "DirtyCpu")]
fn document_dirty<'a>(env: Env<'a>, binary: Binary<'a>) -> NifResult<Term<'a>> {
    document_impl(env, binary)
}

/// Decodes up to `count` documents from the body of a BSON array (the raw batch held by
/// `BSON.LazyDocuments`). Returns `{docs, rest}`, or `:fallback` if any of them needs
/// `BSON.Decoder`; the caller then decodes the same documents one by one.
#[rustler::nif]
fn take<'a>(env: Env<'a>, binary: Binary<'a>, count: usize) -> NifResult<Term<'a>> {
    let bytes = binary.as_slice();
    let ctx = Ctx::new(env, binary);
    let mut docs: SmallVec<[ErlNifTerm; 128]> = SmallVec::new();
    let mut pos = 0;
    while pos < bytes.len() && docs.len() < count {
        if bytes[pos] != 0x03 {
            return Ok(atoms::fallback().encode(env));
        }
        let key_end = match bytes[pos + 1..].iter().position(|&b| b == 0) {
            Some(n) => pos + 1 + n,
            None => return Err(rustler::Error::BadArg),
        };
        let start = key_end + 1;
        if start + 4 > bytes.len() {
            return Err(rustler::Error::BadArg);
        }
        let size = i32::from_le_bytes([
            bytes[start],
            bytes[start + 1],
            bytes[start + 2],
            bytes[start + 3],
        ]);
        if size < 5 || start + size as usize > bytes.len() {
            return Err(rustler::Error::BadArg);
        }
        let end = start + size as usize;
        let doc = match RawDocument::from_bytes(&bytes[start..end]) {
            Ok(doc) => doc,
            Err(_) => return Ok(atoms::fallback().encode(env)),
        };
        match ctx.document(doc) {
            Ok(term) => docs.push(term.as_c_arg()),
            Err(Stop::Fallback) => return Ok(atoms::fallback().encode(env)),
            Err(Stop::Invalid) => return Err(rustler::Error::BadArg),
        }
        pos = end;
    }
    // SAFETY: every element is a valid term of this env.
    let list = unsafe {
        Term::new(
            env,
            enif_make_list_from_array(env.as_c_arg(), docs.as_ptr(), docs.len() as u32),
        )
    };
    let rest = binary.make_subbinary(pos, bytes.len() - pos)?;
    Ok((list, rest).encode(env))
}

rustler::init!("Elixir.MongoRustDecoder.Native");
