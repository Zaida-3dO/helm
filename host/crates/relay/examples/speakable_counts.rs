//! Reads a JSON array of answers on stdin and prints, for each, what the spoken-answer cleanup
//! took out of it and how it came out (words, sentences, whether the cap cut it). The voice
//! benchmark (`checks/voice-bench`) runs it over the answers it collected. It is pure: it reads
//! stdin, writes stdout and contacts nothing.

use helm_relay::speakable::speakable;
use serde_json::{json, Value};
use std::io::Read;

fn main() {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .expect("stdin could not be read");
    let answers: Vec<String> =
        serde_json::from_str(&input).expect("expected a JSON array of strings on stdin");
    let counts: Vec<Value> = answers
        .iter()
        .map(|answer| {
            let s = speakable(answer);
            let r = s.removed;
            json!({
                "code": r.code,
                "tables": r.tables,
                "links": r.links,
                "urls": r.urls,
                "paths": r.paths,
                "ids": r.ids,
                "markup": r.markup,
                "sentences": s.sentences.len(),
                "words": s.sentences.iter().map(|x| x.split_whitespace().count()).sum::<usize>(),
                "truncated": s.truncated,
            })
        })
        .collect();
    println!("{}", Value::Array(counts));
}
