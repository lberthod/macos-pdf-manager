//! Vérification cryptographique de signature numérique `/Sig` (ISO 32000-1
//! §12.8, gestionnaire `adbe.pkcs7.detached` — PKCS#7/CMS détaché, le format
//! très largement dominant en pratique). Sprint 59 (sprint.md Sprint 23+,
//! `audit50quest.md` — signatures numériques).
//!
//! **Portée volontairement restreinte à l'intégrité, pas à la confiance** :
//! ce module vérifie que (a) le contenu couvert par `/ByteRange` n'a pas
//! changé depuis la signature et (b) la signature cryptographique
//! correspond bien au certificat embarqué dans le PKCS#7 — **jamais** que
//! ce certificat est digne de confiance (pas de chaîne de certification
//! jusqu'à une autorité racine, pas de vérification de révocation
//! OCSP/CRL, pas d'horodatage qualifié). `SignatureStatus::Valid` signifie
//! "cohérent avec lui-même", pas "authentique au sens légal" — comme la
//! plupart des visionneuses PDF distinguent ces deux notions (icône verte
//! "signature valide" vs bandeau "identité du signataire non vérifiée").
//! Ajouter la validation de confiance est un projet séparé (magasin de
//! certificats racine, révocation) explicitement hors périmètre ici.
//!
//! Algorithmes pris en charge : clé **RSA** (PKCS#1 v1.5) avec digest
//! SHA-1/256/384/512 — couvre l'immense majorité des signatures PDF
//! réelles. ECDSA/Ed25519/DSA renvoient `SignatureStatus::UnsupportedAlgorithm`
//! plutôt que d'échouer silencieusement ou de paniquer.
//!
//! **Dépendances** : `cms` est en pré-version (`0.3.0-pre.2`, aucune
//! version stable publiée à ce jour — seule exception dans ce projet à la
//! préférence pour des crates stables, faute d'alternative mature pour
//! CMS/PKCS#7 en Rust pur) et tire `der`/`spki` en version 0.8, incompatible
//! **au niveau des types** avec `rsa`/`pkcs8` (version 0.7 tirée
//! transitivement) — deux crates de la même famille RustCrypto mais de
//! générations différentes, sans `From`/`TryFrom` entre elles. Contournée
//! systématiquement par aller-retour en octets DER bruts (`to_der()` d'un
//! côté, `from_public_key_der()` de l'autre) plutôt que conversion de type
//! directe — les octets DER produits par l'une sont lisibles par l'autre
//! même si leurs types Rust ne s'unifient pas.
//!
//! Chaque étape de décodage (`/Contents` est un bourrage à zéro de taille
//! fixe autour du DER réel, structure ASN.1 potentiellement malformée) est
//! un `Option`/`Result` propagé jusqu'à `SignatureStatus::Malformed` —
//! aucun `.unwrap()`/`.expect()` sur une donnée qui vient du fichier PDF
//! (contrôlée par son auteur, jamais approuvée a priori). Taille de module
//! RSA plafonnée (`MAX_RSA_MODULUS_BITS`) pour éviter qu'une clé
//! absurdement grande dans un PDF hostile ne rende la vérification
//! anormalement lente (déni de service).

use crate::document::Document;
use crate::error::Result;
use crate::object::Object;

use cms::cert::x509::der::{Decode, Encode};
use cms::cert::CertificateChoices;
use cms::content_info::ContentInfo;
use cms::signed_data::{SignedAttributes, SignedData};
use der::asn1::OctetStringRef;
use rsa::pkcs8::DecodePublicKey;
use rsa::traits::PublicKeyParts;
use rsa::{Pkcs1v15Sign, RsaPublicKey};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};

const MESSAGE_DIGEST_OID: &str = "1.2.840.113549.1.9.4";
const SHA1_OID: &str = "1.3.14.3.2.26";
const SHA256_OID: &str = "2.16.840.1.101.3.4.2.1";
const SHA384_OID: &str = "2.16.840.1.101.3.4.2.2";
const SHA512_OID: &str = "2.16.840.1.101.3.4.2.3";

/// Préfixes `DigestInfo` PKCS#1 v1.5 (RFC 8017 §9.2, note 1) — constantes
/// bien connues, une par algorithme de hachage, préfixées à l'empreinte
/// avant chiffrement RSA. Construits à la main (`Pkcs1v15Sign { hash_len,
/// prefix }`) plutôt que via `Pkcs1v15Sign::new::<D>()`, qui exige que `D`
/// implémente `AssociatedOid` de la version de `pkcs8` utilisée par `rsa` —
/// non satisfait ici à cause du dédoublement de version décrit dans la doc
/// de module.
const SHA1_PREFIX: [u8; 15] = [
    0x30, 0x21, 0x30, 0x09, 0x06, 0x05, 0x2b, 0x0e, 0x03, 0x02, 0x1a, 0x05, 0x00, 0x04, 0x14,
];
const SHA256_PREFIX: [u8; 19] = [
    0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05,
    0x00, 0x04, 0x20,
];
const SHA384_PREFIX: [u8; 19] = [
    0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02, 0x05,
    0x00, 0x04, 0x30,
];
const SHA512_PREFIX: [u8; 19] = [
    0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x03, 0x05,
    0x00, 0x04, 0x40,
];

/// Au-delà de cette taille de module RSA (en bits), la vérification est
/// refusée (`UnsupportedAlgorithm`) plutôt que tentée : aucun certificat
/// réel n'utilise une clé aussi grande (4096 bits est déjà rare), donc rien
/// n'est perdu en pratique, et ça évite qu'un PDF hostile ne force une
/// opération RSA anormalement lente.
const MAX_RSA_MODULUS_BITS: usize = 8192;

/// Un champ de signature `/Sig` déjà signé (`/V` présent), tel que renvoyé
/// par `Document::signature_fields`.
#[derive(Debug, Clone)]
pub struct SignatureField {
    pub field_name: String,
    pub signer_name: Option<String>,
    pub reason: Option<String>,
    pub location: Option<String>,
    /// `/M`, date de signature au format PDF brut (`D:YYYYMMDDHHmmSS...`) —
    /// pas reparsée en date structurée, pas le périmètre de ce module.
    pub signing_time: Option<String>,
    /// Octets bruts de `/Contents` (PKCS#7/CMS détaché, DER) — inclut
    /// généralement un bourrage à zéro au-delà de la longueur réelle du DER
    /// (espace réservé fixe pour insérer la signature sans décaler les
    /// autres offsets), tronqué par `verify` avant analyse.
    contents: Vec<u8>,
    /// `/ByteRange`, exactement 2 paires `(offset, longueur)` couvrant tout
    /// le document **sauf** `/Contents` lui-même (le cas normal — un
    /// `/ByteRange` à un nombre de paires différent est rejeté à la
    /// lecture, voir `Document::signature_fields`).
    byte_range: [(usize, usize); 2],
}

/// Résultat de `SignatureField::verify` — voir la doc de module sur la
/// distinction intégrité/confiance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureStatus {
    /// Empreinte cohérente et signature RSA valide : intégrité confirmée,
    /// **pas** confiance (voir la doc de module).
    Valid { signer: Option<String> },
    /// Le contenu couvert par `/ByteRange` a changé depuis la signature (ou
    /// ne correspond pas à ce qui a réellement été signé).
    ContentModified,
    /// Structure PKCS#7 cohérente, mais la signature RSA elle-même ne
    /// correspond pas au certificat/contenu signé.
    InvalidSignature,
    /// Structure reconnue mais algorithme non pris en charge (clé non-RSA,
    /// digest non listé, module RSA anormalement grand) — voir la doc de
    /// module.
    UnsupportedAlgorithm,
    /// `/Contents` n'a pas pu être décodé comme PKCS#7/CMS détaché valide
    /// (fichier corrompu, ou gestionnaire de signature non standard).
    Malformed,
}

impl Document {
    /// Liste les champs de formulaire `/FT /Sig` déjà signés (`/V` présent)
    /// de `/AcroForm/Fields` — un seul niveau, comme `pdf-edit::form_fields`
    /// (voir sa doc). `[]` si le document n'a pas de signature, pas
    /// d'`/AcroForm`, ou si `/AcroForm/Fields` est absent/vide.
    pub fn signature_fields(&self) -> Result<Vec<SignatureField>> {
        let root = self.root()?;
        let Some(acroform_obj) = root.get("AcroForm") else {
            return Ok(Vec::new());
        };
        let Ok(acroform) = self.get(acroform_obj) else {
            return Ok(Vec::new());
        };
        let Some(acroform_dict) = acroform.as_dict() else {
            return Ok(Vec::new());
        };
        let Some(fields_obj) = acroform_dict.get("Fields") else {
            return Ok(Vec::new());
        };
        let Ok(fields) = self.get(fields_obj) else {
            return Ok(Vec::new());
        };
        let Some(field_refs) = fields.as_array() else {
            return Ok(Vec::new());
        };

        let mut out = Vec::new();
        for field_obj in field_refs {
            let Ok(resolved) = self.get(field_obj) else {
                continue;
            };
            let Some(dict) = resolved.as_dict() else {
                continue;
            };
            if dict.get("FT").and_then(|o| o.as_name()) != Some("Sig") {
                continue;
            }
            let field_name = dict
                .get("T")
                .and_then(|o| o.as_text_string())
                .unwrap_or_default()
                .to_string();
            // Pas encore signé (`/V` absent) : rien à lister pour l'instant.
            let Some(v_obj) = dict.get("V") else {
                continue;
            };
            let Ok(v) = self.get(v_obj) else {
                continue;
            };
            let Some(sig_dict) = v.as_dict() else {
                continue;
            };

            let Some(Object::String(contents)) = sig_dict.get("Contents") else {
                continue;
            };
            let Some(byte_range_arr) = sig_dict.get("ByteRange").and_then(|o| o.as_array()) else {
                continue;
            };
            if byte_range_arr.len() != 4 {
                continue; // cas normal : exactement 2 paires (offset, longueur).
            }
            let mut nums = [0usize; 4];
            let mut valid = true;
            for (i, n) in byte_range_arr.iter().enumerate() {
                match n.as_int() {
                    Some(v) if v >= 0 => nums[i] = v as usize,
                    _ => {
                        valid = false;
                        break;
                    }
                }
            }
            if !valid {
                continue;
            }
            let byte_range = [(nums[0], nums[1]), (nums[2], nums[3])];

            out.push(SignatureField {
                field_name,
                signer_name: sig_dict.get("Name").and_then(|o| o.as_text_string()),
                reason: sig_dict.get("Reason").and_then(|o| o.as_text_string()),
                location: sig_dict.get("Location").and_then(|o| o.as_text_string()),
                signing_time: sig_dict.get("M").and_then(|o| o.as_text_string()),
                contents: contents.clone(),
                byte_range,
            });
        }
        Ok(out)
    }
}

impl SignatureField {
    /// Vérifie l'intégrité cryptographique de cette signature contre les
    /// octets bruts de `doc` — voir la doc de module pour ce que "valide"
    /// signifie ici (intégrité, pas confiance).
    pub fn verify(&self, doc: &Document) -> SignatureStatus {
        verify_pkcs7_detached(&self.contents, doc.raw_bytes(), &self.byte_range)
    }
}

fn verify_pkcs7_detached(
    contents: &[u8],
    document_bytes: &[u8],
    byte_range: &[(usize, usize); 2],
) -> SignatureStatus {
    let Some(total) = der_object_len(contents) else {
        return SignatureStatus::Malformed;
    };
    let Some(der_bytes) = contents.get(..total) else {
        return SignatureStatus::Malformed;
    };
    let Ok(ci) = ContentInfo::from_der(der_bytes) else {
        return SignatureStatus::Malformed;
    };
    let Ok(signed_data) = ci.content.decode_as::<SignedData>() else {
        return SignatureStatus::Malformed;
    };
    let Some(si) = signed_data.signer_infos.0.iter().next() else {
        return SignatureStatus::Malformed;
    };
    // Cas sans attributs signés (rare en pratique pour un PDF) non géré :
    // le hachage porterait directement sur le contenu plutôt que sur les
    // attributs signés, un chemin de vérification distinct non implémenté
    // ici plutôt que traité (à tort) comme le cas courant.
    let Some(signed_attrs) = si.signed_attrs.as_ref() else {
        return SignatureStatus::UnsupportedAlgorithm;
    };
    let digest_oid = si.digest_alg.oid.to_string();

    let mut ranged = Vec::new();
    for &(off, len) in byte_range {
        let Some(end) = off.checked_add(len) else {
            return SignatureStatus::Malformed;
        };
        let Some(slice) = document_bytes.get(off..end) else {
            return SignatureStatus::Malformed;
        };
        ranged.extend_from_slice(slice);
    }

    let content_digest: Vec<u8> = match digest_oid.as_str() {
        SHA1_OID => Sha1::digest(&ranged).to_vec(),
        SHA256_OID => Sha256::digest(&ranged).to_vec(),
        SHA384_OID => Sha384::digest(&ranged).to_vec(),
        SHA512_OID => Sha512::digest(&ranged).to_vec(),
        _ => return SignatureStatus::UnsupportedAlgorithm,
    };

    let Some(claimed_digest) = find_message_digest(signed_attrs) else {
        return SignatureStatus::Malformed;
    };
    if claimed_digest != content_digest {
        return SignatureStatus::ContentModified;
    }

    let Some(certs) = signed_data.certificates.as_ref() else {
        return SignatureStatus::Malformed;
    };
    let Some(cert) = certs.0.iter().find_map(|c| match c {
        CertificateChoices::Certificate(cert) => Some(cert),
        _ => None,
    }) else {
        return SignatureStatus::Malformed;
    };
    let signer = extract_common_name(&cert.tbs_certificate().subject().to_string());

    let spki = cert.tbs_certificate().subject_public_key_info();
    let Ok(spki_der) = spki.to_der() else {
        return SignatureStatus::Malformed;
    };
    let Ok(rsa_pub) = RsaPublicKey::from_public_key_der(&spki_der) else {
        return SignatureStatus::UnsupportedAlgorithm; // clé non-RSA (ECDSA, Ed25519...).
    };
    if rsa_pub.n().bits() > MAX_RSA_MODULUS_BITS {
        return SignatureStatus::UnsupportedAlgorithm;
    }

    let Ok(signed_attrs_der) = signed_attrs.to_der() else {
        return SignatureStatus::Malformed;
    };
    let sig_bytes = si.signature.as_bytes();

    let verified = match digest_oid.as_str() {
        SHA1_OID => verify_rsa::<Sha1>(&rsa_pub, &signed_attrs_der, sig_bytes, &SHA1_PREFIX),
        SHA256_OID => verify_rsa::<Sha256>(&rsa_pub, &signed_attrs_der, sig_bytes, &SHA256_PREFIX),
        SHA384_OID => verify_rsa::<Sha384>(&rsa_pub, &signed_attrs_der, sig_bytes, &SHA384_PREFIX),
        SHA512_OID => verify_rsa::<Sha512>(&rsa_pub, &signed_attrs_der, sig_bytes, &SHA512_PREFIX),
        _ => unreachable!("filtré par le match précédent sur digest_oid"),
    };

    if verified {
        SignatureStatus::Valid { signer }
    } else {
        SignatureStatus::InvalidSignature
    }
}

/// Hache `signed_attrs_der` avec `D` puis vérifie `sig_bytes` (signature
/// RSA PKCS#1 v1.5) contre ce hachage — `prefix`/`hash_len` construits à la
/// main plutôt que via `Pkcs1v15Sign::new::<D>()`, voir la doc de module.
fn verify_rsa<D: Digest>(
    pubkey: &RsaPublicKey,
    signed_attrs_der: &[u8],
    sig_bytes: &[u8],
    prefix: &[u8],
) -> bool {
    let digest = D::digest(signed_attrs_der);
    let scheme = Pkcs1v15Sign {
        hash_len: Some(digest.len()),
        prefix: prefix.to_vec().into_boxed_slice(),
    };
    pubkey.verify(scheme, &digest, sig_bytes).is_ok()
}

fn find_message_digest(attrs: &SignedAttributes) -> Option<Vec<u8>> {
    for attr in attrs.iter() {
        if attr.oid.to_string() == MESSAGE_DIGEST_OID {
            let val = attr.values.iter().next()?;
            let os: &OctetStringRef = val.decode_as().ok()?;
            return Some(os.as_bytes().to_vec());
        }
    }
    None
}

/// Longueur totale (en-tête + contenu) d'un objet DER encodé en tête de
/// `data`, en forme courte ou longue (ISO/IEC 8825-1 §8.1.3) — `/Contents`
/// est habituellement bourré à zéro au-delà de la longueur réelle du DER
/// (espace réservé fixe pour insérer la signature sans décaler les
/// offsets), il faut donc tronquer avant de parser. `None` si `data` est
/// trop court pour contenir un en-tête DER valide — jamais de panique sur
/// une entrée `/Contents` malformée (donnée contrôlée par l'auteur du PDF,
/// jamais approuvée a priori).
fn der_object_len(data: &[u8]) -> Option<usize> {
    let length_byte = *data.get(1)?;
    if length_byte & 0x80 == 0 {
        Some(2 + length_byte as usize)
    } else {
        let num_len_bytes = (length_byte & 0x7f) as usize;
        let len_bytes = data.get(2..2 + num_len_bytes)?;
        let mut len = 0usize;
        for &b in len_bytes {
            len = len.checked_shl(8)?.checked_add(b as usize)?;
        }
        2usize.checked_add(num_len_bytes)?.checked_add(len)
    }
}

/// Extrait `CN=...` d'un sujet X.509 formaté façon RFC 4514
/// (`cert.tbs_certificate().subject().to_string()`, ex.
/// `"CN=Test Signer"`) — pas un parseur RDN complet, juste le composant le
/// plus utile à afficher.
fn extract_common_name(subject_rfc4514: &str) -> Option<String> {
    subject_rfc4514.split(',').find_map(|part| {
        let part = part.trim();
        part.strip_prefix("CN=").map(|s| s.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    /// `signature_fields` retrouve le champ du fixture (`Signature1`, signé
    /// par pyHanko avec un certificat auto-signé `CN=Test Signer`) avec ses
    /// métadonnées — `Reason`/`Name` viennent de `pyhanko.sign.signers.PdfSignatureMetadata`
    /// à la génération du fixture (voir `pdf-core/tests/fixtures/README.md`).
    #[test]
    fn signature_fields_lists_the_signed_field_with_metadata() {
        let bytes = include_bytes!("../tests/fixtures/signed_document.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        let fields = doc.signature_fields().unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field_name, "Signature1");
        assert_eq!(fields[0].signer_name.as_deref(), Some("Test Signer"));
        assert_eq!(fields[0].reason.as_deref(), Some("Testing"));
    }

    /// `verify` sur le fichier tel que signé : intégrité + signature RSA
    /// toutes deux valides — voir la doc de module sur ce que "valide"
    /// signifie ici (pas de validation de confiance).
    #[test]
    fn verify_accepts_an_unmodified_signed_document() {
        let bytes = include_bytes!("../tests/fixtures/signed_document.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        let fields = doc.signature_fields().unwrap();
        let status = fields[0].verify(&doc);
        assert_eq!(
            status,
            SignatureStatus::Valid {
                signer: Some("Test Signer".to_string())
            }
        );
    }

    /// Le test négatif qui compte le plus pour ce module : falsifier un
    /// octet du contenu couvert par `/ByteRange` **après** signature doit
    /// être détecté, pas silencieusement accepté comme "valide" — c'est
    /// tout l'intérêt d'une vérification de signature. Le flux de contenu
    /// de la page est compressé (`FlateDecode`), donc pas de texte lisible
    /// à chercher dans le fichier brut — on falsifie directement un octet
    /// à l'intérieur du premier segment de `/ByteRange` (retrouvé via
    /// `signature_fields`, donc garanti dans la zone couverte).
    #[test]
    fn verify_rejects_a_document_modified_after_signing() {
        let original = include_bytes!("../tests/fixtures/signed_document.pdf").to_vec();
        let doc = Document::open(original.clone()).unwrap();
        let field = doc.signature_fields().unwrap().into_iter().next().unwrap();
        let (first_offset, first_len) = field.byte_range[0];
        assert!(
            first_len > 10,
            "expected a non-trivial first ByteRange segment"
        );

        let mut tampered = original;
        let flip_at = first_offset + first_len / 2;
        tampered[flip_at] ^= 0xFF;

        let doc = Document::open(tampered).unwrap();
        let fields = doc.signature_fields().unwrap();
        let status = fields[0].verify(&doc);
        assert_eq!(status, SignatureStatus::ContentModified);
    }

    /// Un `/Contents` tronqué à quelques octets (bien avant tout DER
    /// valide) doit renvoyer `Malformed`, jamais paniquer — c'est le
    /// scénario qu'un PDF hostile essaierait en premier (voir la doc de
    /// module sur pourquoi rien n'utilise `.unwrap()`/`.expect()` sur des
    /// données venant du fichier).
    #[test]
    fn verify_rejects_truncated_contents_without_panicking() {
        let field = SignatureField {
            field_name: "X".to_string(),
            signer_name: None,
            reason: None,
            location: None,
            signing_time: None,
            contents: vec![0x30, 0x03], // en-tête SEQUENCE annonçant 3 octets, aucun.
            byte_range: [(0, 1), (0, 1)],
        };
        let bytes = include_bytes!("../tests/fixtures/signed_document.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        assert_eq!(field.verify(&doc), SignatureStatus::Malformed);
    }
}
