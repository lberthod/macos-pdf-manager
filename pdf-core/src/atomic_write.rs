//! Écriture atomique de fichier — AUDIT_FIXES_2026-09-03.md #10.
//!
//! `std::fs::write` tronque le fichier de destination puis écrit dedans : un
//! crash, un `kill`, ou un disque plein pendant l'écriture laisse derrière
//! lui un fichier tronqué (voire vide), sans recours puisque l'app ne garde
//! qu'une copie des octets en mémoire, pas de sauvegarde séparée sur disque.
//! `write_atomic` écrit dans un fichier temporaire du même répertoire (donc
//! sur le même système de fichiers), le force sur disque (`sync_all`), puis
//! le renomme à la place de la destination — `rename` est atomique sur APFS
//! (et la plupart des systèmes de fichiers POSIX) : soit l'ancien fichier
//! reste intact, soit le nouveau est intégralement en place, jamais un état
//! intermédiaire visible.

use std::io::{self, Write};
use std::path::Path;

/// Écrit `bytes` vers `path` de façon atomique (voir la doc de module).
/// Copie les permissions du fichier existant à `path`, s'il y en a un, vers
/// le fichier temporaire avant le renommage final. En cas d'échec à
/// n'importe quelle étape, le fichier à `path` n'est jamais touché — soit
/// l'écriture réussit intégralement, soit elle échoue sans effet visible.
///
/// Si `path` est un lien symbolique (ou traverse un dossier qui en est un),
/// c'est la **cible** qui est remplacée. Audit 2026-10-26 #32 : renommer le
/// fichier temporaire sur le chemin tel quel remplaçait le lien par un
/// fichier ordinaire, et la cible n'était jamais mise à jour. Un
/// `contrat.pdf` pointant vers un dossier synchronisé n'était ainsi plus
/// synchronisé après un ⌘S. Un chemin qui n'existe pas encore (nouvel
/// export) est utilisé tel quel.
pub fn write_atomic(path: impl AsRef<Path>, bytes: &[u8]) -> io::Result<()> {
    let resolved = std::fs::canonicalize(path.as_ref()).ok();
    let path = resolved.as_deref().unwrap_or(path.as_ref());
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;

    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(format!(".tmp-{}", std::process::id()));
    let tmp_path = match dir {
        Some(dir) => dir.join(&tmp_name),
        None => Path::new(&tmp_name).to_path_buf(),
    };

    let write_result = (|| -> io::Result<()> {
        let mut file = std::fs::File::create(&tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    if let Ok(metadata) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp_path, metadata.permissions());
    }

    let rename_result = std::fs::rename(&tmp_path, path);
    if rename_result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    rename_result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Répertoire propre à chaque appel. Le compteur est indispensable : les
    /// tests tournent en parallèle dans le même processus (même pid), et
    /// l'horloge réelle de macOS n'a qu'une résolution d'une microseconde.
    /// Deux tests démarrant dans la même microseconde partageaient sinon le
    /// même répertoire et le même `out.pdf`, l'un réécrivant ou supprimant
    /// le fichier de l'autre. D'où un échec intermittent de
    /// `preserves_existing_file_permissions_after_overwrite` (permissions
    /// 0o644 lues au lieu de 0o640).
    fn temp_dir() -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "pdf-core-write-atomic-test-{}-{}-{n}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn writes_bytes_and_leaves_no_tmp_file_behind() {
        let dir = temp_dir();
        let path = dir.join("out.pdf");

        write_atomic(&path, b"hello atomic").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello atomic");

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no *.tmp-* file should remain, got {leftovers:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preserves_existing_file_permissions_after_overwrite() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir();
        let path = dir.join("out.pdf");
        std::fs::write(&path, b"original").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();

        write_atomic(&path, b"new content").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
        assert_eq!(std::fs::read(&path).unwrap(), b"new content");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn leaves_the_original_file_untouched_when_the_write_fails() {
        let dir = temp_dir();
        let path = dir.join("readonly-dir").join("out.pdf");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"original").unwrap();

        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            path.parent().unwrap(),
            std::fs::Permissions::from_mode(0o500),
        )
        .unwrap();

        let result = write_atomic(&path, b"should not land");

        // Toujours restaurer les permissions avant d'asserter quoi que ce
        // soit : sinon un `assert!` qui panique laisserait le répertoire
        // en lecture seule, gênant le nettoyage `remove_dir_all` plus bas.
        std::fs::set_permissions(
            path.parent().unwrap(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();

        assert!(
            result.is_err(),
            "write into a read-only directory should fail"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"original",
            "the original file must be untouched after a failed write"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Audit 2026-10-26 #32 : écrire à travers un lien symbolique remplaçait
    /// le lien par un fichier ordinaire, sans jamais mettre la cible à jour.
    #[test]
    fn writes_through_a_symlink_to_its_target() {
        let dir = temp_dir();
        let target = dir.join("target.pdf");
        let link = dir.join("link.pdf");
        std::fs::write(&target, b"original").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        write_atomic(&link, b"new content").unwrap();

        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "le lien doit rester un lien"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"new content");

        std::fs::remove_dir_all(&dir).ok();
    }
}
