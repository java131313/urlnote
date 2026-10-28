//! Compile a trimmed IPADIC lexicon (top-60k entries by cost) into a vibrato
//! dictionary. The stock IPADIC trie weighs ~30-50MB in memory and blows the
//! Workers isolate limit; the trimmed one is a few MB. Run:
//!   cargo run --release /tmp/ipadic-utf8 out.dic

use std::env;
use std::fs::File;

use vibrato::SystemDictionaryBuilder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let dir = args.get(1).map(String::as_str).unwrap_or("/tmp/ipadic-utf8");
    let out = args.get(2).cloned().unwrap_or_else(|| format!("{dir}/system.dic"));

    let lex = File::open(format!("{dir}/lex.csv"))?;
    let matrix = File::open(format!("{dir}/matrix.def"))?;
    let char_def = File::open(format!("{dir}/char.def"))?;
    let unk = File::open(format!("{dir}/unk.def"))?;

    let dict = SystemDictionaryBuilder::from_readers(lex, matrix, char_def, unk)?;
    let written = dict.write(File::create(&out)?)?;
    println!("wrote {out} ({written} bytes)");
    Ok(())
}
