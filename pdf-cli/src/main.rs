use pdf_core::{Document, Object};
use std::env;
use std::fs;
use std::process::ExitCode;

fn print_usage() {
    eprintln!("usage: pdf-cli dump <file.pdf>");
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("dump") => match args.get(2) {
            Some(path) => match run_dump(path) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            },
            None => {
                print_usage();
                ExitCode::FAILURE
            }
        },
        _ => {
            print_usage();
            ExitCode::FAILURE
        }
    }
}

fn run_dump(path: &str) -> pdf_core::Result<()> {
    let bytes = fs::read(path)
        .map_err(|e| pdf_core::PdfError::InvalidObject(0, format!("cannot read `{path}`: {e}")))?;
    let doc = Document::open(bytes)?;

    println!("File: {path}");
    println!("Objects (via xref): {}", doc.object_count());

    match doc.root() {
        Ok(root) => {
            if let Some(t) = root.get("Type").and_then(|o| o.as_name()) {
                println!("Root /Type: /{t}");
            }
        }
        Err(e) => println!("Root: unavailable ({e})"),
    }

    match doc.page_count() {
        Ok(n) => println!("Page count: {n}"),
        Err(e) => println!("Page count: unavailable ({e})"),
    }

    if let Some(info) = doc.metadata_dict() {
        for (key, value) in info.iter() {
            if let Object::String(s) = value {
                println!("Info /{key}: {}", String::from_utf8_lossy(s));
            }
        }
    }

    Ok(())
}
