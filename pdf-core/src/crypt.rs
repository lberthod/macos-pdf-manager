//! Déchiffrement `/Encrypt` (gestionnaire de sécurité standard, ISO 32000-1
//! §7.6 pour les révisions 2-4 (RC4/AES-128), ISO 32000-2 §7.6.4 — extension
//! Adobe niveau 3 pour la révision 5 — pour la révision 6 (AES-256), Sprint
//! 22, `audit50quest.md` #50.
//!
//! Mot de passe utilisateur vide **et** réel tous deux gérés depuis le
//! Sprint 58 : `Decryptor::new` prend le mot de passe candidat en paramètre
//! et **vérifie** qu'il correspond bien à `/U` (Algorithme 4/5, ou le sel de
//! validation pour les révisions 5/6) avant de renvoyer un contexte
//! utilisable — un mot de passe incorrect renvoie une erreur explicite
//! (`DecryptError::WrongPassword`, propagée en `PdfError::IncorrectPassword`
//! par `Document::open_with_password`) plutôt que de produire silencieusement
//! du contenu corrompu avec une clé erronée, comme c'était le cas avant
//! cette passe. Câblé jusqu'à l'UI : `pdf-ui` propose une fenêtre de saisie
//! quand l'ouverture avec un mot de passe vide échoue ainsi.
//!
//! Primitives cryptographiques (MD5/SHA-2/RC4/AES) déléguées à des crates
//! auditées (`md-5`, `sha2`, `rc4`, `aes`, `cbc`) plutôt que réimplémentées à
//! la main : ce ne sont pas des briques spécifiques à un moteur PDF, au même
//! titre que `zune-jpeg` pour le décodage JPEG.

use crate::object::{Dictionary, Object, Stream};

use aes::{Aes128, Aes256};
use cbc::cipher::block_padding::{NoPadding, Pkcs7};
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use md5::{Digest as _, Md5};
use sha2::{Sha256, Sha384, Sha512};

/// Chaîne de remplissage standard (ISO 32000-1 Algorithme 2, étape a) —
/// utilisée pour compléter un mot de passe à 32 octets ; un mot de passe
/// vide est donc remplacé exactement par cette chaîne.
const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// Complète (ou tronque) `password` à exactement 32 octets avec `PAD`
/// (ISO 32000-1 Algorithme 2, étape a) — un mot de passe vide redonne donc
/// exactement `PAD`, comme avant que cette fonction n'existe.
fn pad_password(password: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let n = password.len().min(32);
    out[..n].copy_from_slice(&password[..n]);
    out[n..].copy_from_slice(&PAD[..32 - n]);
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CryptMethod {
    Rc4,
    AesV2, // AES-128-CBC
    AesV3, // AES-256-CBC
    Identity,
}

/// Pourquoi `Decryptor::new` a échoué — distingue "gestionnaire de sécurité
/// non standard/non reconnu" (rien à faire, message générique) de "mot de
/// passe incorrect" (l'utilisateur peut réessayer avec un autre mot de
/// passe) : deux situations que `Document::open_with_password` doit
/// distinguer pour donner un message utile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecryptError {
    Unsupported,
    WrongPassword,
}

/// Contexte de déchiffrement d'un document — construit une fois à
/// l'ouverture (`Document::open`), réutilisé pour chaque objet résolu.
pub struct Decryptor {
    file_key: Vec<u8>,
    /// Méthode de chiffrement des flux (`/StmF`) — distincte de
    /// `string_method` (`/StrF`) depuis AUDIT_FIXES_2026-09-03.md #15.
    stream_method: CryptMethod,
    string_method: CryptMethod,
    /// Version de l'algorithme (`/V`) : détermine si la clé par objet doit
    /// être dérivée (`V` <= 4, Algorithme 1) ou si `file_key` sert
    /// directement de clé pour tous les objets (`V` == 5).
    v: i64,
    /// Numéro de l'objet `/Encrypt` lui-même — jamais déchiffré (ses
    /// chaînes, notamment `/O`/`/U`/`/OE`/`/UE`, sont en clair par
    /// construction).
    encrypt_obj_num: Option<u32>,
}

impl Decryptor {
    /// Construit le contexte de déchiffrement à partir du dictionnaire
    /// `/Encrypt` (déjà résolu, non chiffré), du premier élément de `/ID`
    /// du trailer (ISO 32000-1 exige `/ID` pour les révisions <= 4) et du
    /// mot de passe utilisateur candidat (`password`, octets bruts — chaîne
    /// vide pour "pas de mot de passe fourni", le cas normal). **Vérifie**
    /// le mot de passe contre `/U` (Algorithme 4/5 pour les révisions <= 4,
    /// sel de validation pour les révisions 5/6) avant de renvoyer un
    /// contexte utilisable : sans cette vérification, un mot de passe
    /// incorrect produirait silencieusement une clé de fichier erronée et
    /// donc du contenu corrompu au lieu d'une erreur claire — c'était le cas
    /// avant cette passe (Sprint 58, `audit50quest.md` #50), voir
    /// `Document::open_with_password`.
    pub fn new(
        encrypt: &Dictionary,
        id0: &[u8],
        encrypt_obj_num: Option<u32>,
        password: &[u8],
    ) -> Result<Self, DecryptError> {
        use DecryptError::Unsupported;

        if encrypt.get("Filter").and_then(|o| o.as_name()) != Some("Standard") {
            return Err(Unsupported); // gestionnaire de sécurité non standard : non géré.
        }
        let v = encrypt.get("V").and_then(|o| o.as_int()).unwrap_or(0);
        let r = encrypt
            .get("R")
            .and_then(|o| o.as_int())
            .ok_or(Unsupported)?;
        let o_entry = string_bytes(encrypt.get("O").ok_or(Unsupported)?).ok_or(Unsupported)?;
        let u_entry = string_bytes(encrypt.get("U").ok_or(Unsupported)?).ok_or(Unsupported)?;
        let p = encrypt
            .get("P")
            .and_then(|o| o.as_int())
            .ok_or(Unsupported)? as i32;

        if v <= 4 {
            let length_bits = encrypt.get("Length").and_then(|o| o.as_int()).unwrap_or(40);
            let key_len = if r == 2 {
                5
            } else {
                (length_bits / 8) as usize
            };
            // Audit 2026-10-26 #1 : un `/Length` forgé ≤ 7 bits (ou absent
            // avec `/R 2`… non, R2 force 5) donne `key_len == 0`, d'où une
            // clé RC4 vide qui fait paniquer `rc4_apply` (`key[0]` sur un
            // slice vide) dès l'ouverture, avant toute saisie de mot de
            // passe. Une clé de chiffrement standard valide fait 40 à 128
            // bits (5 à 16 octets, ISO 32000-1 §7.6.3.3) : tout ce qui sort
            // de cette plage est un fichier malformé, pas un mot de passe
            // incorrect.
            if !(5..=16).contains(&key_len) {
                return Err(Unsupported);
            }
            let encrypt_metadata = encrypt
                .get("EncryptMetadata")
                .map(|o| matches!(o, Object::Boolean(true)))
                .unwrap_or(true);

            let padded_password = pad_password(password);
            let mut input = Vec::with_capacity(32 + 32 + 4 + id0.len() + 4);
            input.extend_from_slice(&padded_password);
            input.extend_from_slice(&o_entry[..32.min(o_entry.len())]);
            input.extend_from_slice(&p.to_le_bytes());
            input.extend_from_slice(id0);
            if r >= 4 && !encrypt_metadata {
                input.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
            }
            let mut hash: [u8; 16] = Md5::digest(&input).into();
            if r >= 3 {
                for _ in 0..50 {
                    hash = Md5::digest(&hash[..key_len.min(16)]).into();
                }
            }
            let file_key = hash[..key_len.min(16)].to_vec();

            // Algorithme 4 (R2) / 5 (R3-4), ISO 32000-1 §7.6.4.4 : recalcule
            // `/U` à partir de `file_key` et compare — la seule façon de
            // savoir si `password` était le bon avant de déchiffrer quoi
            // que ce soit avec une clé potentiellement fausse.
            let expected_u = if r == 2 {
                rc4_apply(&file_key, &PAD)
            } else {
                let mut hash_input = Vec::with_capacity(32 + id0.len());
                hash_input.extend_from_slice(&PAD);
                hash_input.extend_from_slice(id0);
                let mut step: [u8; 16] = Md5::digest(&hash_input).into();
                let mut buf = rc4_apply(&file_key, &step);
                for i in 1u8..=19 {
                    let round_key: Vec<u8> = file_key.iter().map(|b| b ^ i).collect();
                    buf = rc4_apply(&round_key, &buf);
                }
                step.copy_from_slice(&buf[..16]);
                step.to_vec()
            };
            let matches = if r == 2 {
                expected_u == u_entry
            } else {
                u_entry.len() >= 16 && expected_u[..16] == u_entry[..16]
            };
            if !matches {
                return Err(DecryptError::WrongPassword);
            }

            let (stream_method, string_method) = if v == 4 {
                (
                    crypt_filter_method(encrypt, "StmF").unwrap_or(CryptMethod::Rc4),
                    crypt_filter_method(encrypt, "StrF").unwrap_or(CryptMethod::Rc4),
                )
            } else {
                (CryptMethod::Rc4, CryptMethod::Rc4)
            };

            Ok(Self {
                file_key,
                stream_method,
                string_method,
                v,
                encrypt_obj_num,
            })
        } else {
            // V5 (R5/R6, AES-256) : voir Algorithme 2.A (ISO 32000-2 §7.6.4.3.3)
            // pour retrouver la clé de fichier à partir du mot de passe
            // utilisateur.
            if u_entry.len() < 48 {
                return Err(Unsupported);
            }
            let validation_salt = &u_entry[32..40];
            let key_salt = &u_entry[40..48];
            let ue = string_bytes(encrypt.get("UE").ok_or(Unsupported)?).ok_or(Unsupported)?;
            if ue.len() != 32 {
                return Err(Unsupported);
            }

            // Sel de *validation* : recalcule le hachage stocké dans les 32
            // premiers octets de `/U` et compare, avant de dériver quoi que
            // ce soit à partir du sel de *clé* — même principe que
            // l'Algorithme 5 ci-dessus pour les révisions <= 4.
            let expected_validation = if r >= 6 {
                hardened_hash(password, validation_salt, &[])
            } else {
                let mut hasher = Sha256::new();
                hasher.update(password);
                hasher.update(validation_salt);
                hasher.finalize().to_vec()
            };
            if expected_validation != u_entry[0..32] {
                return Err(DecryptError::WrongPassword);
            }

            let intermediate = if r >= 6 {
                hardened_hash(password, key_salt, &[])
            } else {
                let mut hasher = Sha256::new();
                hasher.update(password);
                hasher.update(key_salt);
                hasher.finalize().to_vec()
            };
            let mut key = [0u8; 32];
            key.copy_from_slice(&intermediate);
            let mut iv = [0u8; 16];
            let file_key = aes256_cbc_decrypt_no_padding(&key, &iv, &ue);
            iv.fill(0);

            Ok(Self {
                file_key,
                stream_method: crypt_filter_method(encrypt, "StmF").unwrap_or(CryptMethod::AesV3),
                string_method: crypt_filter_method(encrypt, "StrF").unwrap_or(CryptMethod::AesV3),
                v,
                encrypt_obj_num,
            })
        }
    }

    pub fn encrypt_obj_num(&self) -> Option<u32> {
        self.encrypt_obj_num
    }

    /// Clé de déchiffrement de l'objet `(num, gen)` — dérivée de la clé de
    /// fichier (Algorithme 1, ISO 32000-1 §7.6.2) pour `V` <= 4 ; identique
    /// à la clé de fichier pour `V` == 5 (pas de dérivation par objet).
    fn object_key(&self, num: u32, gen: u16, method: CryptMethod) -> Vec<u8> {
        if self.v >= 5 {
            return self.file_key.clone();
        }
        let mut input = self.file_key.clone();
        input.extend_from_slice(&num.to_le_bytes()[..3]);
        input.extend_from_slice(&gen.to_le_bytes()[..2]);
        if method == CryptMethod::AesV2 {
            input.extend_from_slice(b"sAlT");
        }
        let hash: [u8; 16] = Md5::digest(&input).into();
        let len = (self.file_key.len() + 5).min(16);
        hash[..len].to_vec()
    }

    /// `method` distingue `stream_method` (`/StmF`) de `string_method`
    /// (`/StrF`, AUDIT_FIXES_2026-09-03.md #15) : la clé par objet en dépend
    /// elle-même (le sel `"sAlT"` de l'Algorithme 1 ne s'applique qu'en
    /// AESV2), donc ce n'est pas juste le dernier choix qui compterait.
    fn decrypt_bytes(&self, data: &[u8], num: u32, gen: u16, method: CryptMethod) -> Vec<u8> {
        if data.is_empty() {
            return Vec::new();
        }
        let key = self.object_key(num, gen, method);
        match method {
            CryptMethod::Identity => data.to_vec(),
            CryptMethod::Rc4 => rc4_apply(&key, data),
            CryptMethod::AesV2 => aes_cbc_decrypt_pkcs7(&key, data, false),
            CryptMethod::AesV3 => aes_cbc_decrypt_pkcs7(&key, data, true),
        }
    }

    /// Déchiffre récursivement toutes les chaînes/flux contenus dans
    /// `object` (qui vient d'être résolu à `(num, gen)`) — appelé par
    /// `Document::resolve` juste après le parsing, avant la mise en cache.
    /// Chaînes déchiffrées avec `string_method` (`/StrF`), flux avec
    /// `stream_method` (`/StmF`) — jusqu'ici les deux utilisaient `/StmF`
    /// sans distinction (AUDIT_FIXES_2026-09-03.md #15).
    pub fn decrypt_object(&self, object: &mut Object, num: u32, gen: u16) {
        match object {
            Object::String(bytes) => {
                *bytes = self.decrypt_bytes(bytes, num, gen, self.string_method);
            }
            Object::Array(items) => {
                for item in items {
                    self.decrypt_object(item, num, gen);
                }
            }
            Object::Dictionary(dict) => {
                for value in dict.0.values_mut() {
                    self.decrypt_object(value, num, gen);
                }
            }
            Object::Stream(stream) => {
                let Stream { dict, raw_data } = stream;
                for value in dict.0.values_mut() {
                    self.decrypt_object(value, num, gen);
                }
                *raw_data = self.decrypt_bytes(raw_data, num, gen, self.stream_method);
            }
            Object::Null
            | Object::Boolean(_)
            | Object::Integer(_)
            | Object::Real(_)
            | Object::Name(_)
            | Object::Reference(_) => {}
        }
    }
}

fn string_bytes(o: &Object) -> Option<Vec<u8>> {
    match o {
        Object::String(bytes) => Some(bytes.clone()),
        _ => None,
    }
}

/// Méthode de chiffrement effective pour le filtre nommé par `key`
/// (`/StmF` pour les flux, `/StrF` pour les chaînes — ISO 32000-1 §7.6.5) :
/// résout le nom vers `/CF`, puis son `/CFM`. `/StrF` était auparavant
/// ignoré, `/StmF` faisant systématiquement foi pour les deux — un document
/// conforme avec `/StmF AESV2 /StrF Identity` (métadonnées en clair, flux de
/// contenu chiffrés — pas rare pour des raisons d'indexation) voyait alors
/// ses chaînes mal déchiffrées sans erreur (AUDIT_FIXES_2026-09-03.md #15).
fn crypt_filter_method(encrypt: &Dictionary, key: &str) -> Option<CryptMethod> {
    let filter_name = encrypt
        .get(key)
        .and_then(|o| o.as_name())
        .unwrap_or("Identity");
    if filter_name == "Identity" {
        return Some(CryptMethod::Identity);
    }
    let cf = encrypt.get("CF")?.as_dict()?;
    let filter = cf.get(filter_name)?.as_dict()?;
    match filter.get("CFM").and_then(|o| o.as_name()) {
        Some("AESV2") => Some(CryptMethod::AesV2),
        Some("AESV3") => Some(CryptMethod::AesV3),
        Some("V2") => Some(CryptMethod::Rc4),
        Some("None") => Some(CryptMethod::Identity),
        _ => None,
    }
}

/// RC4 (symétrique : la même opération chiffre et déchiffre) — implémenté à
/// la main plutôt que via une crate : l'algorithme (KSA + PRGA) est
/// trivial et bien connu, et RC4 est de toute façon un chiffrement obsolète
/// qu'aucune crate ne "sécurise" davantage qu'une implémentation directe
/// correcte ; on ne le supporte ici que pour la compatibilité en lecture de
/// vieux PDF, jamais pour protéger quoi que ce soit.
fn rc4_apply(key: &[u8], data: &[u8]) -> Vec<u8> {
    // Audit 2026-10-26 #1 : garde défensive. `Decryptor::new` rejette déjà
    // une clé hors de 5..=16 octets, mais RC4 est aussi appelé en interne
    // (recalcul de `/U`) — une clé vide ne doit jamais indexer `key[0]` sur
    // un slice vide. Avec une clé vide, RC4 est l'identité : on renvoie les
    // données inchangées plutôt que de paniquer.
    if key.is_empty() {
        return data.to_vec();
    }
    let mut s: [u8; 256] = [0; 256];
    for (i, entry) in s.iter_mut().enumerate() {
        *entry = i as u8;
    }
    let mut j: u8 = 0;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let mut out = Vec::with_capacity(data.len());
    let (mut i, mut j) = (0u8, 0u8);
    for &byte in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        let k = s[(s[i as usize].wrapping_add(s[j as usize])) as usize];
        out.push(byte ^ k);
    }
    out
}

fn aes256_cbc_decrypt_no_padding(key: &[u8; 32], iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    let mut buf = data.to_vec();
    let decryptor = cbc::Decryptor::<Aes256>::new(key.into(), iv.into());
    decryptor
        .decrypt_padded_mut::<NoPadding>(&mut buf)
        .map(|out| out.to_vec())
        .unwrap_or(buf)
}

/// Déchiffre un flux/chaîne AES (ISO 32000-1 §7.6.2 : IV de 16 octets en
/// tête des données, PKCS#7 pour le bourrage — contrairement au décodage
/// "brut" sans bourrage utilisé pour `/OE`/`/UE` en révision 5/6).
fn aes_cbc_decrypt_pkcs7(key: &[u8], data: &[u8], aes256: bool) -> Vec<u8> {
    if data.len() < 16 {
        return Vec::new();
    }
    let (iv, ciphertext) = data.split_at(16);
    let mut buf = ciphertext.to_vec();
    let plaintext = if aes256 && key.len() == 32 {
        let key_arr: [u8; 32] = key.try_into().unwrap();
        cbc::Decryptor::<Aes256>::new(&key_arr.into(), iv.into())
            .decrypt_padded_mut::<Pkcs7>(&mut buf)
            .map(|out| out.len())
    } else if key.len() == 16 {
        let key_arr: [u8; 16] = key.try_into().unwrap();
        cbc::Decryptor::<Aes128>::new(&key_arr.into(), iv.into())
            .decrypt_padded_mut::<Pkcs7>(&mut buf)
            .map(|out| out.len())
    } else {
        return Vec::new();
    };
    match plaintext {
        Ok(len) => {
            buf.truncate(len);
            buf
        }
        Err(_) => Vec::new(),
    }
}

/// Algorithme 2.B (ISO 32000-2 Annexe C, "hardened hash") — hachage itératif
/// utilisé pour la révision 6 (contrairement à la révision 5, qui utilise
/// un simple SHA-256). `input` = mot de passe (toujours vide ici), `salt` =
/// sel de clé ou de validation (8 octets), `udata` = données utilisateur
/// supplémentaires (vide pour le mot de passe utilisateur).
fn hardened_hash(password: &[u8], salt: &[u8], udata: &[u8]) -> Vec<u8> {
    let mut k: Vec<u8> = {
        let mut hasher = Sha256::new();
        hasher.update(password);
        hasher.update(salt);
        hasher.update(udata);
        hasher.finalize().to_vec()
    };

    let mut round = 0u32;
    loop {
        let mut k1 = Vec::with_capacity(64 * (password.len() + k.len() + udata.len()));
        for _ in 0..64 {
            k1.extend_from_slice(password);
            k1.extend_from_slice(&k);
            k1.extend_from_slice(udata);
        }

        let key: [u8; 16] = k[0..16].try_into().unwrap();
        let iv: [u8; 16] = k[16..32].try_into().unwrap();
        let mut buf = k1.clone();
        let encryptor = cbc::Encryptor::<Aes128>::new(&key.into(), &iv.into());
        let e_len = encryptor
            .encrypt_padded_mut::<NoPadding>(&mut buf, k1.len())
            .map(|out| out.len())
            .unwrap_or(0);
        let e = &buf[..e_len];

        let modulus: u32 = e[..16].iter().map(|&b| b as u32).sum::<u32>() % 3;
        k = match modulus {
            0 => Sha256::digest(e).to_vec(),
            1 => Sha384::digest(e).to_vec(),
            _ => Sha512::digest(e).to_vec(),
        };

        round += 1;
        if round >= 64 && (*e.last().unwrap_or(&0) as u32) <= round - 32 {
            break;
        }
    }

    k.truncate(32);
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construit un dictionnaire `/Encrypt` V4/R4 RC4-128 auto-cohérent avec
    /// mot de passe utilisateur vide (`/O` placeholder, `/U` calculé pour que
    /// `Decryptor::new` le valide) — pas besoin d'un vrai fichier signé par
    /// Adobe/qpdf, seule l'auto-cohérence algorithmique compte ici. Renvoie
    /// aussi `file_key` pour que le test puisse chiffrer un flux "à la main"
    /// avec la même clé que `Decryptor` dérivera.
    fn build_v4_rc4_encrypt_dict(id0: &[u8]) -> (Dictionary, Vec<u8>) {
        let o_entry = vec![0u8; 32];
        let p: i32 = -1;

        let padded_password = pad_password(b"");
        let mut input = Vec::new();
        input.extend_from_slice(&padded_password);
        input.extend_from_slice(&o_entry);
        input.extend_from_slice(&p.to_le_bytes());
        input.extend_from_slice(id0);
        let mut hash: [u8; 16] = Md5::digest(&input).into();
        for _ in 0..50 {
            hash = Md5::digest(&hash[..16]).into();
        }
        let file_key = hash.to_vec();

        // Algorithme 5 (R3/R4), ISO 32000-1 §7.6.4.4 : /U attendu pour que
        // `Decryptor::new` (mot de passe vide) valide sans erreur.
        let mut hash_input = Vec::new();
        hash_input.extend_from_slice(&PAD);
        hash_input.extend_from_slice(id0);
        let mut step: [u8; 16] = Md5::digest(&hash_input).into();
        let mut buf = rc4_apply(&file_key, &step);
        for i in 1u8..=19 {
            let round_key: Vec<u8> = file_key.iter().map(|b| b ^ i).collect();
            buf = rc4_apply(&round_key, &buf);
        }
        step.copy_from_slice(&buf[..16]);
        let mut u_entry = step.to_vec();
        u_entry.extend(std::iter::repeat_n(0u8, 16));

        let mut stdcf = Dictionary::new();
        stdcf.insert("CFM", Object::Name("V2".to_string()));
        let mut cf = Dictionary::new();
        cf.insert("StdCF", Object::Dictionary(stdcf));

        let mut encrypt = Dictionary::new();
        encrypt.insert("Filter", Object::Name("Standard".to_string()));
        encrypt.insert("V", Object::Integer(4));
        encrypt.insert("R", Object::Integer(4));
        encrypt.insert("O", Object::String(o_entry));
        encrypt.insert("U", Object::String(u_entry));
        encrypt.insert("P", Object::Integer(p as i64));
        encrypt.insert("Length", Object::Integer(128));
        encrypt.insert("CF", Object::Dictionary(cf));
        // Le coeur du test : `/StmF` et `/StrF` divergent.
        encrypt.insert("StmF", Object::Name("StdCF".to_string()));
        encrypt.insert("StrF", Object::Name("Identity".to_string()));

        (encrypt, file_key)
    }

    /// AUDIT_FIXES_2026-09-03.md #19 : un mot de passe incorrect doit être
    /// détecté (`WrongPassword`) plutôt que de dériver silencieusement une
    /// clé de fichier erronée — la vérification contre `/U` est le coeur de
    /// la doc de module de `Decryptor::new`, sans test direct jusqu'ici.
    #[test]
    fn new_rejects_an_incorrect_password() {
        let id0 = b"0123456789ABCDEF";
        let (encrypt, _file_key) = build_v4_rc4_encrypt_dict(id0);
        match Decryptor::new(&encrypt, id0, None, b"wrong password") {
            Err(DecryptError::WrongPassword) => {}
            other => panic!("expected WrongPassword, got {}", other.is_ok()),
        }
    }

    /// Audit 2026-10-26 #1 : un `/Encrypt` forgé avec `/Length` ≤ 7 bits
    /// (ici 0) donne `key_len == 0`, donc une clé RC4 vide. Avant le
    /// correctif, `Decryptor::new` descendait jusqu'à `rc4_apply(&[], …)`
    /// qui indexait `key[0]` sur un slice vide → panic « index out of
    /// bounds » dès l'ouverture, avant toute saisie de mot de passe. On
    /// exige désormais une erreur propre (`Unsupported`), pas un panic.
    #[test]
    fn new_rejects_a_zero_length_key_instead_of_panicking() {
        let id0 = b"0123456789ABCDEF";
        let mut encrypt = Dictionary::new();
        encrypt.insert("Filter", Object::Name("Standard".to_string()));
        encrypt.insert("V", Object::Integer(2));
        encrypt.insert("R", Object::Integer(3));
        encrypt.insert("O", Object::String(vec![0u8; 32]));
        encrypt.insert("U", Object::String(vec![0u8; 32]));
        encrypt.insert("P", Object::Integer(-1));
        encrypt.insert("Length", Object::Integer(0));

        match Decryptor::new(&encrypt, id0, None, b"") {
            Err(DecryptError::Unsupported) => {}
            other => panic!("expected Unsupported, got {:?}", other.map(|_| ())),
        }
    }

    /// Même famille : une longueur de clé démesurée (`/Length` > 128 bits)
    /// est aussi hors de la plage valide 40-128 bits (ISO 32000-1
    /// §7.6.3.3) et doit être rejetée proprement.
    #[test]
    fn new_rejects_an_oversized_key_length() {
        let id0 = b"0123456789ABCDEF";
        let mut encrypt = Dictionary::new();
        encrypt.insert("Filter", Object::Name("Standard".to_string()));
        encrypt.insert("V", Object::Integer(2));
        encrypt.insert("R", Object::Integer(3));
        encrypt.insert("O", Object::String(vec![0u8; 32]));
        encrypt.insert("U", Object::String(vec![0u8; 32]));
        encrypt.insert("P", Object::Integer(-1));
        encrypt.insert("Length", Object::Integer(4096));

        assert!(matches!(
            Decryptor::new(&encrypt, id0, None, b""),
            Err(DecryptError::Unsupported)
        ));
    }

    /// Garde défensive sur la primitive elle-même : `rc4_apply` avec une
    /// clé vide ne doit jamais paniquer (elle se comporte comme
    /// l'identité).
    #[test]
    fn rc4_apply_with_empty_key_does_not_panic() {
        assert_eq!(rc4_apply(&[], b"hello"), b"hello".to_vec());
    }

    /// AUDIT_FIXES_2026-09-03.md #19 : round-trip AES-128 (`/V 4`, `CFM
    /// AESV2`) — jusqu'ici seul RC4 avait un test direct de ce module.
    #[test]
    fn decrypt_object_handles_aes128_streams() {
        let id0 = b"FEDCBA9876543210";
        let o_entry = vec![0u8; 32];
        let p: i32 = -1;

        let padded_password = pad_password(b"");
        let mut input = Vec::new();
        input.extend_from_slice(&padded_password);
        input.extend_from_slice(&o_entry);
        input.extend_from_slice(&p.to_le_bytes());
        input.extend_from_slice(id0);
        let mut hash: [u8; 16] = Md5::digest(&input).into();
        for _ in 0..50 {
            hash = Md5::digest(&hash[..16]).into();
        }
        let file_key = hash.to_vec();

        let mut hash_input = Vec::new();
        hash_input.extend_from_slice(&PAD);
        hash_input.extend_from_slice(id0);
        let mut step: [u8; 16] = Md5::digest(&hash_input).into();
        let mut buf = rc4_apply(&file_key, &step);
        for i in 1u8..=19 {
            let round_key: Vec<u8> = file_key.iter().map(|b| b ^ i).collect();
            buf = rc4_apply(&round_key, &buf);
        }
        step.copy_from_slice(&buf[..16]);
        let mut u_entry = step.to_vec();
        u_entry.extend(std::iter::repeat_n(0u8, 16));

        let mut stdcf = Dictionary::new();
        stdcf.insert("CFM", Object::Name("AESV2".to_string()));
        let mut cf = Dictionary::new();
        cf.insert("StdCF", Object::Dictionary(stdcf));

        let mut encrypt = Dictionary::new();
        encrypt.insert("Filter", Object::Name("Standard".to_string()));
        encrypt.insert("V", Object::Integer(4));
        encrypt.insert("R", Object::Integer(4));
        encrypt.insert("O", Object::String(o_entry));
        encrypt.insert("U", Object::String(u_entry));
        encrypt.insert("P", Object::Integer(p as i64));
        encrypt.insert("Length", Object::Integer(128));
        encrypt.insert("CF", Object::Dictionary(cf));
        encrypt.insert("StmF", Object::Name("StdCF".to_string()));
        encrypt.insert("StrF", Object::Name("StdCF".to_string()));

        let decryptor = Decryptor::new(&encrypt, id0, None, b"").unwrap();

        // Clé d'objet (Algorithme 1, avec le suffixe "sAlT" propre à AESV2).
        let object_key = {
            let mut key_input = file_key.clone();
            key_input.extend_from_slice(&9u32.to_le_bytes()[..3]);
            key_input.extend_from_slice(&0u16.to_le_bytes()[..2]);
            key_input.extend_from_slice(b"sAlT");
            let key_hash: [u8; 16] = Md5::digest(&key_input).into();
            let len = (file_key.len() + 5).min(16);
            key_hash[..len].to_vec()
        };
        let key_arr: [u8; 16] = object_key.try_into().unwrap();

        let plain = b"AES-128 round trip".to_vec();
        let iv = [0x42u8; 16];
        let mut buf = plain.clone();
        buf.resize(plain.len() + 16, 0); // marge pour le bourrage PKCS#7.
        let ciphertext = cbc::Encryptor::<Aes128>::new(&key_arr.into(), &iv.into())
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plain.len())
            .unwrap();
        let mut stream_data = iv.to_vec();
        stream_data.extend_from_slice(ciphertext);

        let mut stream_obj = Object::Stream(Stream {
            dict: Dictionary::new(),
            raw_data: stream_data,
        });
        decryptor.decrypt_object(&mut stream_obj, 9, 0);
        let Object::Stream(decrypted) = stream_obj else {
            unreachable!()
        };
        assert_eq!(decrypted.raw_data, plain);
    }

    /// AUDIT_FIXES_2026-09-03.md #15 : `/StrF Identity` avec `/StmF StdCF`
    /// (RC4) doit laisser les chaînes intactes tout en déchiffrant
    /// réellement les flux — avant le correctif, `/StmF` faisait foi pour
    /// les deux, corrompant silencieusement toute chaîne d'un tel document
    /// (métadonnées, `/Contents` d'annotation...).
    #[test]
    fn decrypt_object_honors_strf_independently_from_stmf() {
        let id0 = b"0123456789ABCDEF";
        let (encrypt, file_key) = build_v4_rc4_encrypt_dict(id0);
        let decryptor = Decryptor::new(&encrypt, id0, None, b"").unwrap();

        // `/StrF Identity` : la chaîne est stockée en clair dans le fichier,
        // `decrypt_object` ne doit pas y toucher.
        let plain_string = b"Hello".to_vec();
        let mut string_obj = Object::String(plain_string.clone());
        decryptor.decrypt_object(&mut string_obj, 7, 0);
        assert_eq!(
            string_obj,
            Object::String(plain_string),
            "StrF Identity: string must pass through unchanged"
        );

        // `/StmF StdCF` (RC4) : le flux est réellement chiffré — on le
        // chiffre nous-mêmes avec la clé d'objet (Algorithme 1) pour vérifier
        // que `decrypt_object` le retrouve en clair.
        let object_key = {
            let mut input = file_key.clone();
            input.extend_from_slice(&7u32.to_le_bytes()[..3]);
            input.extend_from_slice(&0u16.to_le_bytes()[..2]);
            let hash: [u8; 16] = Md5::digest(&input).into();
            let len = (file_key.len() + 5).min(16);
            hash[..len].to_vec()
        };
        let plain_stream = b"stream contents".to_vec();
        let cipher_stream = rc4_apply(&object_key, &plain_stream);
        let mut stream_obj = Object::Stream(Stream {
            dict: Dictionary::new(),
            raw_data: cipher_stream,
        });
        decryptor.decrypt_object(&mut stream_obj, 7, 0);
        let Object::Stream(decrypted) = stream_obj else {
            unreachable!()
        };
        assert_eq!(
            decrypted.raw_data, plain_stream,
            "StmF StdCF: stream must be decrypted"
        );
    }
}
