//! Clipboard subsystem · crypto: the on-disk envelope and the master key.
//!
//! Every persisted byte the clipboard owns goes through this envelope: the history TOML, the
//! original image bytes, the thumbnail preview and the detail preview. Layout:
//!
//! ```text
//! magic     [8]   b"OMTCLIP\x01"   file kind + envelope version
//! kind      [1]   1 history | 2 image data | 3 thumbnail | 4 detail
//! key_id    [16]  identifies the key that sealed the file
//! object_id [16]  SHA-256 of the file's LOGICAL name, first 16 bytes
//! nonce     [12]  random per write
//! ct||tag   [..]  AES-256-GCM(plaintext, AAD = magic || kind || key_id || object_id)
//! ```
//!
//! `object_id` binds the ciphertext to the logical file it belongs to, so two complete
//! same-kind files cannot be swapped. It is always recomputed by the reader from the name it
//! asked for -- the copy carried in the header is a diagnostic field only, never trusted.

use aes_gcm::aead::{Aead, KeyInit, Nonce, Payload};
use aes_gcm::{Aes256Gcm, Key};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

/// The envelope magic: `OMTCLIP` plus the envelope format version byte.
pub(super) const ENVELOPE_MAGIC: [u8; 8] = *b"OMTCLIP\x01";
pub(super) const KEY_LEN: usize = 32;
pub(super) const KEY_ID_LEN: usize = 16;
pub(super) const OBJECT_ID_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
/// magic + kind + key_id + object_id + nonce
pub(super) const HEADER_LEN: usize = 8 + 1 + KEY_ID_LEN + OBJECT_ID_LEN + NONCE_LEN;

/// What a sealed file holds; folded into the AAD so a preview can never be read as image data.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ObjectKind {
    History = 1,
    ImageData = 2,
    Thumbnail = 3,
    Detail = 4,
}

impl ObjectKind {
    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::History),
            2 => Some(Self::ImageData),
            3 => Some(Self::Thumbnail),
            4 => Some(Self::Detail),
            _ => None,
        }
    }
}

/// Why a sealed file could not be opened. The caller's failure classification depends on the
/// difference: `NotEnvelope` means legacy plaintext (migration), `ForeignKey` means the data
/// belongs to another key (never replace the key over this), `Damaged` means authentication or
/// structure failed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum OpenError {
    NotEnvelope,
    ForeignKey { key_id: [u8; KEY_ID_LEN] },
    Damaged,
}

/// The master key: a random 32-byte key plus the random 16-byte id that labels it in every
/// envelope. Zeroized on drop; the key bytes never reach a log. Cloning is intentional (the
/// storage layer hands out copies; each copy zeroizes on its own drop).
#[derive(Clone)]
pub(super) struct MasterKey {
    key_id: [u8; KEY_ID_LEN],
    key: [u8; KEY_LEN],
}

impl Drop for MasterKey {
    fn drop(&mut self) {
        self.key.zeroize();
        self.key_id.zeroize();
    }
}

impl MasterKey {
    /// Generate a fresh key (CSPRNG).
    pub(super) fn generate() -> Option<Self> {
        let mut key = [0u8; KEY_LEN];
        let mut key_id = [0u8; KEY_ID_LEN];
        getrandom::fill(&mut key).ok()?;
        getrandom::fill(&mut key_id).ok()?;
        Some(Self { key_id, key })
    }

    /// The keychain item's value: `key_id || key`.
    pub(super) fn to_item_value(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(KEY_ID_LEN + KEY_LEN);
        out.extend_from_slice(&self.key_id);
        out.extend_from_slice(&self.key);
        out
    }

    /// Parse `key_id || key` back out of the keychain item; a wrong length is a foreign item.
    pub(super) fn from_item_value(value: &[u8]) -> Option<Self> {
        if value.len() != KEY_ID_LEN + KEY_LEN {
            return None;
        }
        let mut key_id = [0u8; KEY_ID_LEN];
        let mut key = [0u8; KEY_LEN];
        key_id.copy_from_slice(&value[..KEY_ID_LEN]);
        key.copy_from_slice(&value[KEY_ID_LEN..]);
        Some(Self { key_id, key })
    }

    pub(super) fn key_id(&self) -> &[u8; KEY_ID_LEN] {
        &self.key_id
    }

    fn cipher(&self) -> Option<Aes256Gcm> {
        let key = Key::<Aes256Gcm>::try_from(&self.key[..]).ok()?;
        Some(Aes256Gcm::new(&key))
    }
}

/// The authenticated object id for a logical file name (`history`, `{hash:016x}`,
/// `{hash:016x}.preview`, `{hash:016x}.detail`). Pure.
pub(super) fn object_id_for(logical_name: &str) -> [u8; OBJECT_ID_LEN] {
    let digest = Sha256::digest(logical_name.as_bytes());
    let mut id = [0u8; OBJECT_ID_LEN];
    id.copy_from_slice(&digest[..OBJECT_ID_LEN]);
    id
}

/// The AAD of a sealed file: everything in the header except the nonce.
fn aad(kind: ObjectKind, key_id: &[u8; KEY_ID_LEN], object_id: &[u8; OBJECT_ID_LEN]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(8 + 1 + KEY_ID_LEN + OBJECT_ID_LEN);
    aad.extend_from_slice(&ENVELOPE_MAGIC);
    aad.push(kind as u8);
    aad.extend_from_slice(key_id);
    aad.extend_from_slice(object_id);
    aad
}

/// Whether the bytes carry the envelope magic (a plaintext legacy file does not). Pure.
pub(super) fn looks_like_envelope(bytes: &[u8]) -> bool {
    bytes.len() >= ENVELOPE_MAGIC.len() && bytes[..ENVELOPE_MAGIC.len()] == ENVELOPE_MAGIC
}

/// Seal `plaintext` into an envelope. `object_id` must be the reader-side value computed from
/// the logical name this file will be stored under.
pub(super) fn seal(
    key: &MasterKey,
    kind: ObjectKind,
    object_id: &[u8; OBJECT_ID_LEN],
    plaintext: &[u8],
) -> Option<Vec<u8>> {
    let cipher = key.cipher()?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce_bytes).ok()?;
    let nonce = Nonce::<Aes256Gcm>::try_from(&nonce_bytes[..]).ok()?;
    let aad = aad(kind, &key.key_id, object_id);
    let mut body = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .ok()?;
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&ENVELOPE_MAGIC);
    out.push(kind as u8);
    out.extend_from_slice(&key.key_id);
    out.extend_from_slice(object_id);
    out.extend_from_slice(&nonce_bytes);
    out.append(&mut body);
    Some(out)
}

/// Open an envelope. `object_id` is recomputed by the caller from the logical name it asked
/// for; the header's copy is compared for diagnostics only.
pub(super) fn open(
    key: &MasterKey,
    kind: ObjectKind,
    object_id: &[u8; OBJECT_ID_LEN],
    bytes: &[u8],
) -> Result<Vec<u8>, OpenError> {
    if !looks_like_envelope(bytes) {
        return Err(OpenError::NotEnvelope);
    }
    if bytes.len() < HEADER_LEN + TAG_LEN {
        return Err(OpenError::Damaged);
    }
    let Some(file_kind) = ObjectKind::from_byte(bytes[8]) else {
        return Err(OpenError::Damaged);
    };
    let mut file_key_id = [0u8; KEY_ID_LEN];
    file_key_id.copy_from_slice(&bytes[9..9 + KEY_ID_LEN]);
    if file_key_id != *key.key_id() {
        return Err(OpenError::ForeignKey {
            key_id: file_key_id,
        });
    }
    if file_kind != kind {
        return Err(OpenError::Damaged);
    }
    // The header's object_id copy is never trusted (the AAD uses the value recomputed from the
    // logical name), but a file whose header disagrees with its own name is inconsistent: reject
    // it instead of reading it with a silent discrepancy.
    if bytes[9 + KEY_ID_LEN..9 + KEY_ID_LEN + OBJECT_ID_LEN] != object_id[..] {
        return Err(OpenError::Damaged);
    }
    let mut nonce_bytes = [0u8; NONCE_LEN];
    let nonce_start = 9 + KEY_ID_LEN + OBJECT_ID_LEN;
    nonce_bytes.copy_from_slice(&bytes[nonce_start..nonce_start + NONCE_LEN]);
    let Some(nonce) = Nonce::<Aes256Gcm>::try_from(&nonce_bytes[..]).ok() else {
        return Err(OpenError::Damaged);
    };
    let Some(cipher) = key.cipher() else {
        return Err(OpenError::Damaged);
    };
    let aad = aad(kind, &key.key_id, object_id);
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: &bytes[HEADER_LEN..],
                aad: &aad,
            },
        )
        .map_err(|_| OpenError::Damaged)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> MasterKey {
        MasterKey::generate().expect("rng")
    }

    fn key_with_same_id(other: &MasterKey) -> MasterKey {
        let mut clone = MasterKey {
            key_id: *other.key_id(),
            key: [7u8; KEY_LEN],
        };
        clone.key[0] = other.key[0].wrapping_add(1);
        clone
    }

    fn history_id() -> [u8; OBJECT_ID_LEN] {
        object_id_for("history")
    }

    #[test]
    fn round_trips_every_kind() {
        let key = key();
        for kind in [
            ObjectKind::History,
            ObjectKind::ImageData,
            ObjectKind::Thumbnail,
            ObjectKind::Detail,
        ] {
            let plaintext = b"the quick brown fox";
            let sealed = seal(&key, kind, &history_id(), plaintext).expect("seal");
            assert!(looks_like_envelope(&sealed));
            assert_eq!(
                open(&key, kind, &history_id(), &sealed).expect("open"),
                plaintext
            );
        }
    }

    #[test]
    fn every_write_uses_a_fresh_nonce() {
        let key = key();
        let first = seal(&key, ObjectKind::History, &history_id(), b"x").expect("seal");
        let second = seal(&key, ObjectKind::History, &history_id(), b"x").expect("seal");
        assert_ne!(
            first, second,
            "nonce reuse would leak equality of the plaintext"
        );
    }

    #[test]
    fn plaintext_is_not_detectable_in_the_envelope() {
        let key = key();
        let secret = b"hunter2-copied-secret";
        let sealed = seal(&key, ObjectKind::History, &history_id(), secret).expect("seal");
        assert!(
            !sealed.windows(secret.len()).any(|w| w == secret),
            "the plaintext must not survive verbatim in the file"
        );
    }

    #[test]
    fn a_foreign_key_is_reported_as_such_and_never_as_damage() {
        let creator = key();
        let sealed = seal(&creator, ObjectKind::History, &history_id(), b"x").expect("seal");
        let other = key();
        match open(&other, ObjectKind::History, &history_id(), &sealed) {
            Err(OpenError::ForeignKey { key_id }) => assert_eq!(&key_id, creator.key_id()),
            other => panic!("expected ForeignKey, got {other:?}"),
        }
    }

    #[test]
    fn a_wrong_key_with_the_same_id_is_damage() {
        let creator = key();
        let sealed = seal(&creator, ObjectKind::History, &history_id(), b"x").expect("seal");
        let impostor = key_with_same_id(&creator);
        assert_eq!(
            open(&impostor, ObjectKind::History, &history_id(), &sealed),
            Err(OpenError::Damaged)
        );
    }

    #[test]
    fn swapping_the_kind_is_rejected() {
        let key = key();
        let sealed = seal(&key, ObjectKind::Thumbnail, &history_id(), b"x").expect("seal");
        assert_eq!(
            open(&key, ObjectKind::ImageData, &history_id(), &sealed),
            Err(OpenError::Damaged)
        );
    }

    #[test]
    fn swapping_the_object_id_is_rejected() {
        let key = key();
        let sealed = seal(&key, ObjectKind::ImageData, &object_id_for("aaaa"), b"x").expect("seal");
        // A complete, self-consistent file moved to another object's name must not open: the
        // reader recomputes the id from the name it asked for.
        assert_eq!(
            open(&key, ObjectKind::ImageData, &object_id_for("bbbb"), &sealed),
            Err(OpenError::Damaged)
        );
    }

    #[test]
    fn tampering_with_any_header_field_is_rejected() {
        let key = key();
        let sealed = seal(&key, ObjectKind::History, &history_id(), b"x").expect("seal");
        for index in 0..HEADER_LEN {
            let mut tampered = sealed.clone();
            tampered[index] ^= 0x01;
            let result = open(&key, ObjectKind::History, &history_id(), &tampered);
            // Flipping a magic byte makes it stop looking like an envelope (which the load path
            // treats as damage, never as legacy plaintext); every other field is authenticated.
            assert!(
                matches!(
                    result,
                    Err(OpenError::Damaged)
                        | Err(OpenError::ForeignKey { .. })
                        | Err(OpenError::NotEnvelope)
                ),
                "flipping header byte {index} must be rejected, got {result:?}"
            );
            assert!(result.is_err(), "byte {index} must never open");
        }
    }

    #[test]
    fn tampering_with_the_ciphertext_or_the_tag_is_rejected() {
        let key = key();
        let sealed = seal(&key, ObjectKind::History, &history_id(), b"payload").expect("seal");
        for index in [HEADER_LEN, sealed.len() - 1] {
            let mut tampered = sealed.clone();
            tampered[index] ^= 0x80;
            assert_eq!(
                open(&key, ObjectKind::History, &history_id(), &tampered),
                Err(OpenError::Damaged),
                "byte {index} must be authenticated"
            );
        }
    }

    #[test]
    fn truncation_is_damage_and_plaintext_is_not_an_envelope() {
        let key = key();
        let sealed = seal(&key, ObjectKind::History, &history_id(), b"payload").expect("seal");
        assert_eq!(
            open(
                &key,
                ObjectKind::History,
                &history_id(),
                &sealed[..HEADER_LEN]
            ),
            Err(OpenError::Damaged)
        );
        assert_eq!(
            open(&key, ObjectKind::History, &history_id(), b"version = 1\n"),
            Err(OpenError::NotEnvelope)
        );
        assert!(!looks_like_envelope(b"version = 1\n"));
    }

    #[test]
    fn the_item_value_round_trips_and_rejects_a_wrong_length() {
        let key = key();
        let value = key.to_item_value();
        assert_eq!(value.len(), KEY_ID_LEN + KEY_LEN);
        let parsed = MasterKey::from_item_value(&value).expect("parse");
        assert_eq!(parsed.key_id(), key.key_id());
        let sealed = seal(&key, ObjectKind::History, &history_id(), b"x").expect("seal");
        assert_eq!(
            open(&parsed, ObjectKind::History, &history_id(), &sealed).expect("open"),
            b"x"
        );
        assert!(MasterKey::from_item_value(&value[..value.len() - 1]).is_none());
        assert!(MasterKey::from_item_value(&[]).is_none());
    }

    #[test]
    fn object_ids_are_stable_and_name_dependent() {
        assert_eq!(object_id_for("history"), object_id_for("history"));
        assert_ne!(object_id_for("history"), object_id_for("history2"));
        assert_ne!(
            object_id_for("0000000000000001"),
            object_id_for("0000000000000001.preview")
        );
    }
}
