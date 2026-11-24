// Decodes every vector as the shape its kind names; an `ok` row must also re-encode
// to its canonical bytes, and an `err` row must fail to decode.
// The peer uses only the codec, so the generated service surface is unused here.
#[allow(dead_code, unused_imports)]
#[path = "mod.rs"]
mod api;

use std::{env, fs, process};

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("vector hex"))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decode `input` as the kind's type and re-encode it, or say why decode failed.
fn round_trip(kind: &str, input: &[u8]) -> Result<Result<Vec<u8>, String>, String> {
    fn again<T, E: std::fmt::Display>(
        decoded: Result<T, api::CsilCborError>,
        encode: impl Fn(&T) -> Result<Vec<u8>, E>,
    ) -> Result<Result<Vec<u8>, String>, String> {
        let value = decoded.map_err(|e| e.to_string())?;
        Ok(encode(&value).map_err(|e| e.to_string()))
    }
    match kind {
        "holder" => again(api::decode_holder(input), api::encode_holder),
        "contact" => again(api::decode_contact(input), api::encode_contact),
        "output" => again(
            api::decode_lookup_read_counts_output(input),
            api::encode_lookup_read_counts_output,
        ),
        other => panic!("unknown kind {other}"),
    }
}

fn main() {
    let path = env::args().nth(1).expect("supply the vectors.tsv path");
    let table = fs::read_to_string(&path).expect("read vectors");
    let mut failures = 0;
    for line in table.lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let cols: Vec<&str> = line.split('\t').collect();
        let [name, kind, expect, input, canonical] = cols[..] else {
            panic!("malformed row {line:?}");
        };
        let problem = match (expect, round_trip(kind, &unhex(input))) {
            ("err", Err(_)) => None,
            ("err", Ok(_)) => Some("decoded a value that must be refused".to_string()),
            ("ok", Err(e)) => Some(format!("refused: {e}")),
            ("ok", Ok(Err(e))) => Some(format!("re-encode refused: {e}")),
            ("ok", Ok(Ok(bytes))) => {
                let again = hex(&bytes);
                (again != canonical).then(|| format!("encoded {again}, want {canonical}"))
            }
            (other, _) => Some(format!("unknown expectation {other}")),
        };
        if let Some(problem) = problem {
            failures += 1;
            println!("FAIL {name}: {problem}");
        }
    }
    if failures > 0 {
        process::exit(1);
    }
    println!("all wire vectors pass");
}
