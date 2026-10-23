use bson::{doc, RawDocument, Document};
use std::time::Instant;
use std::hint::black_box;

fn walk(d: &RawDocument) -> usize {
    let mut n = 0;
    for e in d.iter() {
        let (k, v) = e.unwrap();
        n += k.len();
        match v {
            bson::RawBsonRef::Document(sub) => n += walk(sub),
            bson::RawBsonRef::Array(a) => for x in a { if let bson::RawBsonRef::String(s) = x.unwrap() { n += s.len() } },
            bson::RawBsonRef::String(s) => n += s.len(),
            _ => n += 1,
        }
    }
    n
}

fn main() {
    let d = doc! {"_id": "subscription_1", "session_id": "session_1", "conditions": {"attributes.planbookId": ["pb_1"], "attributes.status": ["published"]}, "negations": {}};
    let mut bytes = Vec::new(); d.to_writer(&mut bytes).unwrap();
    let n = 300_000;
    for _ in 0..3 {
        let t = Instant::now();
        for _ in 0..n { let r = RawDocument::from_bytes(black_box(&bytes)).unwrap(); black_box(walk(r)); }
        let walk_ms = t.elapsed().as_secs_f64() * 1000.0;
        let t = Instant::now();
        for _ in 0..n { let r = RawDocument::from_bytes(black_box(&bytes)).unwrap(); let doc: Document = r.try_into().unwrap(); black_box(doc); }
        let owned_ms = t.elapsed().as_secs_f64() * 1000.0;
        println!("raw walk: {:.1} ms, owned Document: {:.1} ms (300k docs, {} bytes each)", walk_ms, owned_ms, bytes.len());
    }
}
