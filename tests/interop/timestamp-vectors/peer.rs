// Decodes every vector as a `Stamp`; an `ok` row must also re-encode to its canonical bytes.
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

fn main() {
    let path = env::args().nth(1).expect("supply the vectors.tsv path");
    let table = fs::read_to_string(&path).expect("read vectors");
    let mut failures = 0;
    let mut rows = 0;
    for line in table.lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let cols: Vec<&str> = line.split('\t').collect();
        let [name, expect, _precision, input, canonical] = cols[..] else {
            panic!("malformed row {line:?}");
        };
        rows += 1;
        let decoded = api::decode_stamp(&unhex(input));
        let problem = match (expect, decoded) {
            ("err", Err(_)) => None,
            ("err", Ok(stamp)) => Some(format!("accepted {}", stamp.at)),
            ("ok", Err(e)) => Some(format!("refused: {e:?}")),
            ("ok", Ok(stamp)) => match api::encode_stamp(&stamp) {
                Ok(bytes) => {
                    let again = hex(&bytes);
                    (again != canonical).then(|| format!("encoded {again}, want {canonical}"))
                }
                Err(e) => Some(format!("re-encode refused: {e}")),
            },
            (other, _) => Some(format!("unknown expectation {other}")),
        };
        if let Some(problem) = problem {
            failures += 1;
            println!("FAIL {name}: {problem}");
        }
    }
    println!("rust: {} of {rows} timestamp vectors pass", rows - failures);
    if failures > 0 {
        process::exit(1);
    }
}
