use pdf_core::display::DisplayItem;
use pdf_core::interp::Interpreter;
use pdf_core::{Document, Object};
use std::env;
use std::fs;
use std::process::ExitCode;

fn print_usage() {
    eprintln!("usage: pdf-cli dump <file.pdf>");
    eprintln!("       pdf-cli render-info <file.pdf> [page_index]");
    eprintln!("       pdf-cli render <file.pdf> <out.png> [page_index]");
    eprintln!("       pdf-cli text <file.pdf> [page_index]");
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("dump") => args.get(2).map(|path| run_dump(path)),
        Some("render-info") => args.get(2).map(|path| {
            let page_index: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            run_render_info(path, page_index)
        }),
        Some("render") => args.get(2).zip(args.get(3)).map(|(path, out)| {
            let page_index: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
            run_render(path, out, page_index)
        }),
        Some("text") => args.get(2).map(|path| {
            let page_index: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            run_text(path, page_index)
        }),
        _ => None,
    };

    match result {
        Some(Ok(())) => ExitCode::SUCCESS,
        Some(Err(e)) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
        None => {
            print_usage();
            ExitCode::FAILURE
        }
    }
}

fn read_document(path: &str) -> pdf_core::Result<Document> {
    let bytes = fs::read(path)
        .map_err(|e| pdf_core::PdfError::InvalidObject(0, format!("cannot read `{path}`: {e}")))?;
    Document::open(bytes)
}

fn run_dump(path: &str) -> pdf_core::Result<()> {
    let doc = read_document(path)?;

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

/// Exerce le pipeline complet (Sprint 5-6) : arbre des pages -> flux de
/// contenu décodé -> interpréteur -> DisplayList, et affiche un résumé.
fn run_render_info(path: &str, page_index: usize) -> pdf_core::Result<()> {
    let doc = read_document(path)?;
    let page = doc.page(page_index)?;
    let content = doc.page_content(&page)?;
    let display = Interpreter::run_page(&doc, page.resources.clone(), &content)?;

    println!("File: {path}");
    println!(
        "Page: {page_index} (MediaBox {:?}, Rotate {})",
        page.media_box, page.rotate
    );
    println!("Content stream: {} bytes decoded", content.len());

    let (mut paths, mut glyphs, mut images) = (0, 0, 0);
    for item in &display.items {
        match item {
            DisplayItem::Path { .. } => paths += 1,
            DisplayItem::Glyph { .. } => glyphs += 1,
            DisplayItem::Image { .. } => images += 1,
        }
    }
    println!(
        "DisplayList items: {} paths, {} glyphs, {} images",
        paths, glyphs, images
    );

    let text: String = display
        .items
        .iter()
        .filter_map(|item| match item {
            DisplayItem::Glyph { unicode, .. } => *unicode,
            _ => None,
        })
        .collect();
    if !text.is_empty() {
        println!("Recovered text (via /Encoding/Differences, /ToUnicode if present): {text:?}");
    }
    let estimated = display.items.iter().any(|item| {
        matches!(
            item,
            DisplayItem::Glyph {
                advance_is_estimated: true,
                ..
            }
        )
    });
    println!(
        "Glyph widths: {}",
        if estimated {
            "placeholder (no font resolved)"
        } else {
            "real (/Widths or Helvetica AFM fallback)"
        }
    );

    Ok(())
}

/// Rasterise une page en PNG (Sprint 7-8) : seuls les chemins vectoriels
/// sont dessinés pour l'instant, pas les glyphes ni les images (voir les
/// limitations documentées dans `pdf-render`).
fn run_render(path: &str, out_path: &str, page_index: usize) -> pdf_core::Result<()> {
    let doc = read_document(path)?;
    let page = doc.page(page_index)?;
    let content = doc.page_content(&page)?;
    let display = Interpreter::run_page(&doc, page.resources.clone(), &content)?;

    let pixmap = pdf_render::render_page_rotated(&display, page.media_box, page.rotate, 1.0)
        .ok_or_else(|| {
            pdf_core::PdfError::InvalidObject(0, "failed to allocate render target".to_string())
        })?;
    let png = pdf_render::encode_png(&pixmap);
    fs::write(out_path, &png).map_err(|e| {
        pdf_core::PdfError::InvalidObject(0, format!("cannot write `{out_path}`: {e}"))
    })?;

    println!(
        "Rendered page {page_index} of {path} to {out_path} ({}x{})",
        pixmap.width(),
        pixmap.height()
    );
    Ok(())
}

/// Extrait le texte d'une page (Sprint 9-10 : préalable à la recherche
/// texte) via `pdf-text::extract_text` sur la `DisplayList` interprétée.
fn run_text(path: &str, page_index: usize) -> pdf_core::Result<()> {
    let doc = read_document(path)?;
    let page = doc.page(page_index)?;
    let content = doc.page_content(&page)?;
    let display = Interpreter::run_page(&doc, page.resources.clone(), &content)?;

    println!("{}", pdf_text::extract_text(&display));
    Ok(())
}
