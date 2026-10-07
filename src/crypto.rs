//! Protocol primitives: hex/base64, SHA-256, X25519 and ChaCha20-Poly1305.
//!
//! Thin, allocation-conscious wrappers over RustCrypto. The bulk `.hax` decrypt
//! uses the in-place AEAD API so each segment slice moves exactly once.

use std::fmt;

use chacha20poly1305::aead::{Aead, AeadInOut, KeyInit, Payload, Tag};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use sha2::{Digest, Sha256};
use x25519_dalek::{EphemeralSecret, PublicKey};

/// Errors from the protocol layer.
#[derive(Debug)]
pub enum Error {
    /// Ciphertext failed authentication, or was too short to contain a tag.
    Auth,
    /// A hex/base64 string was malformed.
    Decode(&'static str),
    /// The page state decrypted but was not the expected shape.
    State(String),
    /// The listen API rejected the request.
    Listen(String),
    /// A required field was missing or unusable.
    Protocol(String),
    /// Transport failure.
    Http(String),
    /// Signing failed.
    Sign(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Auth => write!(f, "decryption failed (bad key or corrupt data)"),
            Error::Decode(what) => write!(f, "malformed {what}"),
            Error::State(m) => write!(f, "page state: {m}"),
            Error::Listen(m) => write!(f, "listen API: {m}"),
            Error::Protocol(m) => write!(f, "protocol: {m}"),
            Error::Http(m) => write!(f, "http: {m}"),
            Error::Sign(m) => write!(f, "signer: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// Shorthand result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Branch keys: tree node index to 32-byte key.
pub type KeyMap = std::collections::HashMap<u32, [u8; 32]>;

/// SHA-256 of `data`.
#[inline]
pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// Decode a lowercase or uppercase hex string.
#[inline]
pub fn hex_decode(s: &str) -> Result<Vec<u8>> {
    if s.len() % 2 != 0 {
        return Err(Error::Decode("hex string of odd length"));
    }
    hex::decode(s).map_err(|_| Error::Decode("hex string"))
}

/// Encode bytes as lowercase hex.
#[inline]
pub fn hex_encode(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

/// Decode standard base64 (the page state is base64 with padding).
pub fn base64_decode(s: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|_| Error::Decode("base64 string"))
}

/// The all-zero 96-bit nonce used for the page state and every HAX0 slice.
#[inline]
fn zero_nonce() -> Nonce {
    Nonce::from([0u8; 12])
}

/// Encrypt `plaintext` with ChaCha20-Poly1305 under `key` and a zero nonce.
pub fn seal_zero(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(&(*key).into());
    cipher
        .encrypt(
            &zero_nonce(),
            Payload {
                msg: plaintext,
                aad: &[],
            },
        )
        .map_err(|_| Error::Auth)
}

/// Decrypt a ChaCha20-Poly1305 ciphertext under `key` and a zero nonce.
pub fn open_zero(key: &[u8; 32], ciphertext: &[u8]) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(&(*key).into());
    cipher
        .decrypt(
            &zero_nonce(),
            Payload {
                msg: ciphertext,
                aad: &[],
            },
        )
        .map_err(|_| Error::Auth)
}

/// Decrypt a zero-nonce ciphertext in place, appending the plaintext to `out`.
///
/// Avoids the intermediate `Vec` that [`open_zero`] allocates, which matters
/// because this runs once per HAX0 segment.
pub fn open_zero_into(key: &[u8; 32], ciphertext: &[u8], out: &mut Vec<u8>) -> Result<()> {
    if ciphertext.len() < 16 {
        return Err(Error::Auth);
    }
    let cipher = ChaCha20Poly1305::new(&(*key).into());
    let start = out.len();
    out.extend_from_slice(ciphertext);
    // Detached: split off the 16-byte Poly1305 tag, authenticate, then keep the
    // ciphertext-only plaintext.
    let split = out.len() - 16;
    let tag = Tag::<ChaCha20Poly1305>::try_from(&out[split..]).expect("16-byte tag");
    if cipher
        .decrypt_inout_detached(&zero_nonce(), b"", (&mut out[start..split]).into(), &tag)
        .is_err()
    {
        out.truncate(start);
        return Err(Error::Auth);
    }
    out.truncate(split);
    Ok(())
}

/// Decrypt with an explicit 12-byte nonce (listen request/response bodies).
pub fn open_with_nonce(key: &[u8; 32], nonce: &[u8; 12], ciphertext: &[u8]) -> Result<Vec<u8>> {
    if ciphertext.len() < 16 {
        return Err(Error::Auth);
    }
    let cipher = ChaCha20Poly1305::new(&(*key).into());
    let mut buf = ciphertext.to_vec();
    let split = buf.len() - 16;
    let tag = Tag::<ChaCha20Poly1305>::try_from(&buf[split..]).expect("16-byte tag");
    cipher
        .decrypt_inout_detached(&Nonce::from(*nonce), b"", (&mut buf[..split]).into(), &tag)
        .map_err(|_| Error::Auth)?;
    buf.truncate(split);
    Ok(buf)
}

/// Encrypt with an explicit 12-byte nonce.
pub fn seal_with_nonce(key: &[u8; 32], nonce: &[u8; 12], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new(&(*key).into());
    cipher
        .encrypt(
            &Nonce::from(*nonce),
            Payload {
                msg: plaintext,
                aad: &[],
            },
        )
        .map_err(|_| Error::Auth)
}

/// An established X25519 session with the server.
pub struct Session {
    /// Client ephemeral public key, hex — sent as `X-Key`.
    pub client_pub_hex: String,
    /// SHA-256 of the X25519 shared secret — the listen payload encryption key.
    pub secret: [u8; 32],
}

/// Generate an ephemeral X25519 keypair and derive the session secret from the
/// server's static public key.
pub fn key_exchange(server_pub_hex: &str) -> Result<Session> {
    let server_pub_bytes = hex_decode(server_pub_hex)?;
    let server_pub_bytes: [u8; 32] = server_pub_bytes
        .try_into()
        .map_err(|_| Error::Protocol("server public key is not 32 bytes".into()))?;
    let server_pub = PublicKey::from(server_pub_bytes);

    // EphemeralSecret::random draws from the OS CSPRNG.
    let secret = EphemeralSecret::random();
    let client_pub = PublicKey::from(&secret);
    let shared = secret.diffie_hellman(&server_pub);

    Ok(Session {
        client_pub_hex: hex::encode(client_pub.as_bytes()),
        secret: sha256(shared.as_bytes()),
    })
}
