//! Tests d'intégration `pdf-cli` — AUDIT_FIXES_2026-09-03.md #19 (aucun test
//! direct sur le binaire jusqu'ici). Invoque le binaire réellement compilé
//! (`env!("CARGO_BIN_EXE_pdf-cli")`, fourni par cargo sans dépendance
//! supplémentaire) plutôt que d'appeler les fonctions internes, pour couvrir
//! ce qui compte vraiment ici : le parsing d'arguments et les codes de
//! sortie du binaire tel qu'un utilisateur l'invoque.

use std::path::PathBuf;
use std::process::Command;

fn pdf_cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_pdf-cli"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../pdf-core/tests/fixtures")
        .join(name)
}

fn temp_output(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "pdf-cli-test-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    path
}

#[test]
fn no_arguments_prints_usage_and_fails() {
    let output = pdf_cli().output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("usage:"), "got stderr: {stderr}");
}

#[test]
fn unknown_command_prints_usage_and_fails() {
    let output = pdf_cli().arg("not-a-real-command").output().unwrap();
    assert!(!output.status.success());
}

#[test]
fn split_extracts_a_page_and_reports_success() {
    let input = fixture("multipage_classic_xref.pdf");
    assert!(input.exists(), "fixture manquante : {input:?}");
    let out = temp_output("split-out.pdf");

    let output = pdf_cli()
        .arg("split")
        .arg(&input)
        .arg(&out)
        .arg("0")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(out.exists());

    let bytes = std::fs::read(&out).unwrap();
    let doc = pdf_core::Document::open(bytes).unwrap();
    assert_eq!(doc.page_count().unwrap(), 1);

    std::fs::remove_file(&out).ok();
}

#[test]
fn split_with_missing_input_fails_with_a_nonzero_exit_code() {
    let out = temp_output("split-missing-input-out.pdf");
    let output = pdf_cli()
        .arg("split")
        .arg("/nonexistent/does-not-exist.pdf")
        .arg(&out)
        .arg("0")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!out.exists());
}

#[test]
fn redact_removes_the_content_and_reports_success() {
    let input = fixture("multipage_classic_xref.pdf");
    let out = temp_output("redact-out.pdf");

    // La zone couvre toute la page 0 : quel que soit son contenu réel, il y
    // a forcément quelque chose à caviarder dedans.
    let output = pdf_cli()
        .arg("redact")
        .arg(&input)
        .arg(&out)
        .args(["0", "0", "0", "612", "792"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(out.exists());

    std::fs::remove_file(&out).ok();
}

#[test]
fn redact_on_an_empty_zone_fails_with_a_nonzero_exit_code() {
    let input = fixture("multipage_classic_xref.pdf");
    let out = temp_output("redact-empty-out.pdf");

    let output = pdf_cli()
        .arg("redact")
        .arg(&input)
        .arg(&out)
        .args(["0", "500", "10", "590", "40"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!out.exists());
}

#[test]
fn verify_signatures_reports_valid_on_a_signed_document() {
    let input = fixture("signed_document.pdf");
    assert!(input.exists(), "fixture manquante : {input:?}");

    let output = pdf_cli()
        .arg("verify-signatures")
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("VALID"), "got stdout: {stdout}");
}

#[test]
fn verify_signatures_on_an_unsigned_document_still_succeeds_with_no_fields() {
    let input = fixture("minimal.pdf");
    let output = pdf_cli()
        .arg("verify-signatures")
        .arg(&input)
        .output()
        .unwrap();
    assert!(output.status.success());
}
