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
    eprintln!(
        "       pdf-cli highlight <in.pdf> <out.pdf> <page_index> <x0> <y0> <x1> <y1> [r] [g] [b]"
    );
    eprintln!("       pdf-cli fill-form <in.pdf> <out.pdf> <field_name> <value>");
    eprintln!("       pdf-cli insert-blank-page <in.pdf> <out.pdf> <at_index> <width> <height>");
    eprintln!("       pdf-cli insert-image-page <in.pdf> <out.pdf> <at_index> <image.jpg>");
    eprintln!("       pdf-cli delete-page <in.pdf> <out.pdf> <page_index>");
    eprintln!("       pdf-cli move-page <in.pdf> <out.pdf> <from_index> <to_index>");
    eprintln!("       pdf-cli rotate-page <in.pdf> <out.pdf> <page_index> <degrees>");
    eprintln!("       pdf-cli merge <base.pdf> <other.pdf> <out.pdf>");
    eprintln!("       pdf-cli split <in.pdf> <out.pdf> <page_index> [page_index...]");
    eprintln!("       pdf-cli optimize <in.pdf> <out.pdf>");
    eprintln!(
        "       pdf-cli add-text <in.pdf> <out.pdf> <page_index> <x0> <y0> <x1> <y1> <font_size> <text>"
    );
    eprintln!(
        "       pdf-cli replace-text <in.pdf> <out.pdf> <page_index> <x0> <y0> <x1> <y1> <font_size> <text>"
    );
    eprintln!("       pdf-cli remove-annotation <in.pdf> <out.pdf> <page_index> <annot_index>");
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
        Some("highlight") => run_highlight_args(&args),
        Some("fill-form") => args.get(2).zip(args.get(3)).and_then(|(input, out)| {
            args.get(4)
                .zip(args.get(5))
                .map(|(field, value)| run_fill_form(input, out, field, value).map_err(pdf_error))
        }),
        Some("insert-blank-page") => run_insert_blank_page_args(&args),
        Some("insert-image-page") => run_insert_image_page_args(&args),
        Some("delete-page") => run_delete_page_args(&args),
        Some("move-page") => run_move_page_args(&args),
        Some("rotate-page") => run_rotate_page_args(&args),
        Some("merge") => run_merge_args(&args),
        Some("split") => run_split_args(&args),
        Some("optimize") => args
            .get(2)
            .zip(args.get(3))
            .map(|(input, out)| run_optimize(input, out).map_err(pdf_error)),
        Some("add-text") => run_add_text_args(&args),
        Some("replace-text") => run_replace_text_args(&args),
        Some("remove-annotation") => run_remove_annotation_args(&args),
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

/// `pdf-edit` renvoie `Result<_, String>` (pas de dépendance de `pdf-edit`
/// vers l'enum d'erreurs de `pdf-core`) ; ce binaire ne connaît qu'un seul
/// type d'erreur de bout en bout, donc on enveloppe.
fn pdf_error(e: String) -> pdf_core::PdfError {
    pdf_core::PdfError::InvalidObject(0, e)
}

/// Analyse les arguments de `pdf-cli highlight`, séparément du reste
/// (contrairement aux autres commandes, elle a un nombre d'arguments
/// variable : couleur optionnelle).
fn run_highlight_args(args: &[String]) -> Option<pdf_core::Result<()>> {
    let input = args.get(2)?;
    let out = args.get(3)?;
    let page_index: usize = args.get(4)?.parse().ok()?;
    let x0: f64 = args.get(5)?.parse().ok()?;
    let y0: f64 = args.get(6)?.parse().ok()?;
    let x1: f64 = args.get(7)?.parse().ok()?;
    let y1: f64 = args.get(8)?.parse().ok()?;
    let r: f32 = args.get(9).and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let g: f32 = args.get(10).and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let b: f32 = args.get(11).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    Some(run_highlight(input, out, page_index, [x0, y0, x1, y1], (r, g, b)).map_err(pdf_error))
}

/// Sprint 13-14 : ajoute une annotation `/Highlight` (`pdf-edit`) et
/// sauvegarde incrémentalement — preuve en ligne de commande que le
/// pipeline complet (annotation -> `/AP` -> sauvegarde incrémentale ->
/// relecture) fonctionne sur un vrai fichier, pas seulement en test.
fn run_highlight(
    input: &str,
    out: &str,
    page_index: usize,
    rect: [f64; 4],
    color: (f32, f32, f32),
) -> Result<(), String> {
    let mut session = pdf_edit::EditSession::open(input)?;
    session.add_highlight_annotation(page_index, rect, color, vec![])?;
    session.save_as(out)?;
    println!("Added highlight annotation on page {page_index} of {input}, saved to {out}");
    Ok(())
}

/// Sprint 13-14 : remplit un champ AcroForm (`pdf-edit`) et sauvegarde
/// incrémentalement.
fn run_fill_form(input: &str, out: &str, field_name: &str, value: &str) -> Result<(), String> {
    let mut session = pdf_edit::EditSession::open(input)?;
    session.set_form_field_value(field_name, value)?;
    session.save_as(out)?;
    println!("Set field '{field_name}' = {value:?} in {input}, saved to {out}");
    Ok(())
}

/// Sprint 15-16 : manipulation de pages (`pdf-edit`), toutes suivant le
/// même schéma — ouvrir en `EditSession`, appliquer une opération, sauver
/// incrémentalement — preuve en ligne de commande sur un vrai fichier.
fn run_insert_blank_page_args(args: &[String]) -> Option<pdf_core::Result<()>> {
    let input = args.get(2)?;
    let out = args.get(3)?;
    let at_index: usize = args.get(4)?.parse().ok()?;
    let width: f64 = args.get(5)?.parse().ok()?;
    let height: f64 = args.get(6)?.parse().ok()?;
    Some(
        (|| -> Result<(), String> {
            let mut session = pdf_edit::EditSession::open(input)?;
            session.insert_blank_page(at_index, [0.0, 0.0, width, height])?;
            session.save_as(out)?;
            println!("Inserted blank page at index {at_index} of {input}, saved to {out}");
            Ok(())
        })()
        .map_err(pdf_error),
    )
}

fn run_insert_image_page_args(args: &[String]) -> Option<pdf_core::Result<()>> {
    let input = args.get(2)?;
    let out = args.get(3)?;
    let at_index: usize = args.get(4)?.parse().ok()?;
    let image_path = args.get(5)?;
    Some(
        (|| -> Result<(), String> {
            let jpeg_bytes = fs::read(image_path).map_err(|e| e.to_string())?;
            let mut session = pdf_edit::EditSession::open(input)?;
            session.insert_image_page(at_index, &jpeg_bytes)?;
            session.save_as(out)?;
            println!("Inserted image page at index {at_index} of {input}, saved to {out}");
            Ok(())
        })()
        .map_err(pdf_error),
    )
}

fn run_delete_page_args(args: &[String]) -> Option<pdf_core::Result<()>> {
    let input = args.get(2)?;
    let out = args.get(3)?;
    let index: usize = args.get(4)?.parse().ok()?;
    Some(
        (|| -> Result<(), String> {
            let mut session = pdf_edit::EditSession::open(input)?;
            session.delete_page(index)?;
            session.save_as(out)?;
            println!("Deleted page {index} of {input}, saved to {out}");
            Ok(())
        })()
        .map_err(pdf_error),
    )
}

fn run_move_page_args(args: &[String]) -> Option<pdf_core::Result<()>> {
    let input = args.get(2)?;
    let out = args.get(3)?;
    let from: usize = args.get(4)?.parse().ok()?;
    let to: usize = args.get(5)?.parse().ok()?;
    Some(
        (|| -> Result<(), String> {
            let mut session = pdf_edit::EditSession::open(input)?;
            session.move_page(from, to)?;
            session.save_as(out)?;
            println!("Moved page {from} -> {to} in {input}, saved to {out}");
            Ok(())
        })()
        .map_err(pdf_error),
    )
}

fn run_rotate_page_args(args: &[String]) -> Option<pdf_core::Result<()>> {
    let input = args.get(2)?;
    let out = args.get(3)?;
    let index: usize = args.get(4)?.parse().ok()?;
    let degrees: i32 = args.get(5)?.parse().ok()?;
    Some(
        (|| -> Result<(), String> {
            let mut session = pdf_edit::EditSession::open(input)?;
            session.rotate_page(index, degrees)?;
            session.save_as(out)?;
            println!("Rotated page {index} of {input} by {degrees} degrees, saved to {out}");
            Ok(())
        })()
        .map_err(pdf_error),
    )
}

fn run_merge_args(args: &[String]) -> Option<pdf_core::Result<()>> {
    let base = args.get(2)?;
    let other = args.get(3)?;
    let out = args.get(4)?;
    Some(
        (|| -> Result<(), String> {
            let other_bytes = fs::read(other).map_err(|e| e.to_string())?;
            let other_doc = Document::open(other_bytes).map_err(|e| e.to_string())?;
            let mut session = pdf_edit::EditSession::open(base)?;
            session.merge_document(&other_doc)?;
            session.save_as(out)?;
            println!("Merged {other} into {base}, saved to {out}");
            Ok(())
        })()
        .map_err(pdf_error),
    )
}

fn run_split_args(args: &[String]) -> Option<pdf_core::Result<()>> {
    let input = args.get(2)?;
    let out = args.get(3)?;
    let indices: Vec<usize> = args[4..].iter().filter_map(|s| s.parse().ok()).collect();
    if indices.is_empty() {
        return None;
    }
    Some(
        (|| -> Result<(), String> {
            let bytes = fs::read(input).map_err(|e| e.to_string())?;
            let doc = Document::open(bytes).map_err(|e| e.to_string())?;
            let extracted = pdf_edit::extract_pages(&doc, &indices)?;
            fs::write(out, extracted).map_err(|e| e.to_string())?;
            println!(
                "Extracted {} page(s) from {input}, saved to {out}",
                indices.len()
            );
            Ok(())
        })()
        .map_err(pdf_error),
    )
}

/// "Export / optimisation" (Sprint 15-16) : réécrit le document en entier
/// via `pdf_edit::export_optimized` — un vrai garbage collector par
/// reconstruction plutôt qu'une simple sauvegarde incrémentale, voir sa doc.
fn run_optimize(input: &str, out: &str) -> Result<(), String> {
    let bytes = fs::read(input).map_err(|e| e.to_string())?;
    let doc = Document::open(bytes).map_err(|e| e.to_string())?;
    let optimized = pdf_edit::export_optimized(&doc)?;
    fs::write(out, optimized).map_err(|e| e.to_string())?;
    println!("Optimized {input}, saved to {out}");
    Ok(())
}

/// Sprint 17+ (6a) : ajoute une annotation `/FreeText` (nouveau texte,
/// noir sur fond transparent) et sauvegarde incrémentalement.
fn run_add_text_args(args: &[String]) -> Option<pdf_core::Result<()>> {
    let (input, out, page_index, rect, font_size, text) = parse_text_op_args(args)?;
    Some(
        (|| -> Result<(), String> {
            let mut session = pdf_edit::EditSession::open(input)?;
            session.add_free_text_annotation(
                page_index,
                rect,
                &text,
                font_size,
                (0.0, 0.0, 0.0),
            )?;
            session.save_as(out)?;
            println!("Added free text on page {page_index} of {input}, saved to {out}");
            Ok(())
        })()
        .map_err(pdf_error),
    )
}

/// Sprint 17+ (6b) : recouvre `rect` d'un fond blanc puis redessine `text`
/// par-dessus (masquer l'ancien + redessiner, pas une édition chirurgicale
/// du flux d'origine — voir sprint.md).
fn run_replace_text_args(args: &[String]) -> Option<pdf_core::Result<()>> {
    let (input, out, page_index, rect, font_size, text) = parse_text_op_args(args)?;
    Some(
        (|| -> Result<(), String> {
            let mut session = pdf_edit::EditSession::open(input)?;
            session.replace_text_with_overlay(
                page_index,
                rect,
                &text,
                font_size,
                (0.0, 0.0, 0.0),
                (1.0, 1.0, 1.0),
            )?;
            session.save_as(out)?;
            println!("Replaced text region on page {page_index} of {input}, saved to {out}");
            Ok(())
        })()
        .map_err(pdf_error),
    )
}

/// Arguments communs à `add-text`/`replace-text`.
type TextOpArgs<'a> = (&'a str, &'a str, usize, [f64; 4], f64, String);

/// Analyse `<in> <out> <page_index> <x0> <y0> <x1> <y1> <font_size>
/// <text...>` (le texte peut contenir des espaces, donc pris comme le reste
/// des arguments joints).
fn parse_text_op_args(args: &[String]) -> Option<TextOpArgs<'_>> {
    let input = args.get(2)?;
    let out = args.get(3)?;
    let page_index: usize = args.get(4)?.parse().ok()?;
    let x0: f64 = args.get(5)?.parse().ok()?;
    let y0: f64 = args.get(6)?.parse().ok()?;
    let x1: f64 = args.get(7)?.parse().ok()?;
    let y1: f64 = args.get(8)?.parse().ok()?;
    let font_size: f64 = args.get(9)?.parse().ok()?;
    if args.len() <= 10 {
        return None;
    }
    let text = args[10..].join(" ");
    Some((input, out, page_index, [x0, y0, x1, y1], font_size, text))
}

fn run_remove_annotation_args(args: &[String]) -> Option<pdf_core::Result<()>> {
    let input = args.get(2)?;
    let out = args.get(3)?;
    let page_index: usize = args.get(4)?.parse().ok()?;
    let annot_index: usize = args.get(5)?.parse().ok()?;
    Some(
        (|| -> Result<(), String> {
            let mut session = pdf_edit::EditSession::open(input)?;
            session.remove_annotation(page_index, annot_index)?;
            session.save_as(out)?;
            println!(
                "Removed annotation {annot_index} from page {page_index} of {input}, saved to {out}"
            );
            Ok(())
        })()
        .map_err(pdf_error),
    )
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
    let display = Interpreter::run_page_with_annotations(&doc, &page, &content)?;

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
    let display = Interpreter::run_page_with_annotations(&doc, &page, &content)?;

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
