// Auto-generated FFI bindings for libsodium 1.0.18
// Generated for OHOS cross-compilation support

#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]
#![allow(dead_code, unused_imports, unused_variables)]

extern crate libc;

// Version functions
extern "C" {
    pub fn sodium_version_string() -> *const libc::c_char;
    pub fn sodium_library_version_string() -> *const libc::c_char;
    pub fn sodium_library_minimal() -> libc::c_int;
    pub fn sodium_init() -> libc::c_int;
}

// Random
extern "C" {
    pub fn randombytes_buf(buf: *mut libc::c_void, len: usize) -> ();
    pub fn randombytes_buf_deterministic(buf: *mut libc::c_void, len: usize, state: *const libc::c_uchar) -> ();
    pub fn randombytes_random() -> libc::c_uint;
    pub fn randombytes_stir() -> ();
    pub fn randombytes_set_implementation(implementation: *const libc::c_void) -> libc::c_int;
}

// Secretbox
pub const crypto_secretbox_KEYBYTES: usize = 32;
pub const crypto_secretbox_NONCEBYTES: usize = 24;
pub const crypto_secretbox_MACBYTES: usize = 16;

extern "C" {
    pub fn crypto_secretbox_easy(c: *mut libc::c_uchar, m: *const libc::c_uchar, mlen: usize, n: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_secretbox_open_easy(m: *mut libc::c_uchar, c: *const libc::c_uchar, clen: usize, nonce: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_secretbox_detached(c: *mut libc::c_uchar, mac: *mut libc::c_uchar, m: *const libc::c_uchar, mlen: usize, n: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_secretbox_open_detached(m: *mut libc::c_uchar, c: *const libc::c_uchar, mac: *const libc::c_uchar, clen: usize, nonce: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
}

// Box (public-key authenticated encryption)
pub const crypto_box_PUBLICKEYBYTES: usize = 32;
pub const crypto_box_SECRETKEYBYTES: usize = 32;
pub const crypto_box_NONCEBYTES: usize = 24;
pub const crypto_box_MACBYTES: usize = 16;
pub const crypto_box_SEALBYTES: usize = 24 + 32;

extern "C" {
    pub fn crypto_box_keypair(pk: *mut libc::c_uchar, sk: *mut libc::c_uchar) -> libc::c_int;
    pub fn crypto_box_seal(c: *mut libc::c_uchar, m: *const libc::c_uchar, mlen: usize, pk: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_box_seal_open(m: *mut libc::c_uchar, c: *const libc::c_uchar, clen: usize, pk: *const libc::c_uchar, sk: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_box_beforenm(k: *mut libc::c_uchar, pk: *const libc::c_uchar, sk: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_box_afternm(c: *mut libc::c_uchar, m: *const libc::c_uchar, mlen: usize, n: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_box_open_afternm(m: *mut libc::c_uchar, c: *const libc::c_uchar, clen: usize, nonce: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
}

// Sign (digital signatures)
pub const crypto_sign_SEEDBYTES: usize = 32;
pub const crypto_sign_PUBLICKEYBYTES: usize = 32;
pub const crypto_sign_SECRETKEYBYTES: usize = 64;
pub const crypto_sign_BYTES: usize = 64;

extern "C" {
    pub fn crypto_sign_seed_keypair(pk: *mut libc::c_uchar, sk: *mut libc::c_uchar, seed: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_sign_keypair(pk: *mut libc::c_uchar, sk: *mut libc::c_uchar) -> libc::c_int;
    pub fn crypto_sign_statebytes() -> usize;
    pub fn crypto_sign(pk: *mut libc::c_uchar, siglen: *mut usize, m: *const libc::c_uchar, mlen: usize, sk: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_sign_open(m: *mut libc::c_uchar, mlen: *mut usize, sm: *const libc::c_uchar, smlen: usize, pk: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_sign_detached(sig: *mut libc::c_uchar, siglen: *mut usize, m: *const libc::c_uchar, mlen: usize, sk: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_sign_verify(sig: *const libc::c_uchar, siglen: usize, m: *const libc::c_uchar, mlen: usize, pk: *const libc::c_uchar) -> libc::c_int;
}

// Auth (message authentication)
pub const crypto_auth_KEYBYTES: usize = 32;
pub const crypto_auth_BYTES: usize = 32;

extern "C" {
    pub fn crypto_auth(out: *mut libc::c_uchar, in_: *const libc::c_uchar, inlen: usize, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_auth_verify(h: *const libc::c_uchar, in_: *const libc::c_uchar, inlen: usize, k: *const libc::c_uchar) -> libc::c_int;
}

// Hash
pub const crypto_hash_BYTES: usize = 64;

extern "C" {
    pub fn crypto_hash(out: *mut libc::c_uchar, in_: *const libc::c_uchar, inlen: usize) -> libc::c_int;
}

// Scalarmult
pub const crypto_scalarmult_BYTES: usize = 32;
pub const crypto_scalarmult_SCALARBYTES: usize = 32;

extern "C" {
    pub fn crypto_scalarmult(n: *const libc::c_uchar, k: *const libc::c_uchar, p: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_scalarmult_base(q: *const libc::c_uchar, n: *const libc::c_uchar) -> libc::c_int;
}

// Verify
extern "C" {
    pub fn crypto_verify_16(a: *const libc::c_uchar, b: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_verify_32(a: *const libc::c_uchar, b: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_verify_64(a: *const libc::c_uchar, b: *const libc::c_uchar) -> libc::c_int;
}

// Stream cipher
pub const crypto_stream_KEYBYTES: usize = 32;
pub const crypto_stream_NONCEBYTES: usize = 24;

extern "C" {
    pub fn crypto_stream(c: *mut libc::c_uchar, n: usize, nonce: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_stream_xor(c: *mut libc::c_uchar, m: *const libc::c_uchar, n: usize, nonce: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
}

// Pwhash (password hashing)
pub const crypto_pwhash_ALG_DEFAULT: libc::c_int = 0;
pub const crypto_pwhash_ALG_ARGON2I13: libc::c_int = 1;
pub const crypto_pwhash_ALG_ARGON2ID13: libc::c_int = 2;
pub const crypto_pwhash_OPSLIMIT_INTERACTIVE: usize = 33554432;
pub const crypto_pwhash_MEMLIMIT_INTERACTIVE: usize = 67108864;
pub const crypto_pwhash_OPSLIMIT_SENSITIVE: usize = 73400320;
pub const crypto_pwhash_MEMLIMIT_SENSITIVE: usize = 268435456;
pub const crypto_pwhash_STRPREFIX: &str = "$argon2id$";
pub const crypto_pwhash_STRBYTES: usize = 128;

extern "C" {
    pub fn crypto_pwhash(out: *mut libc::c_uchar, outlen: usize, passwd: *const libc::c_uchar, passwdlen: usize, salt: *const libc::c_uchar, opslimit: usize, memlimit: usize, alg: libc::c_int) -> libc::c_int;
    pub fn crypto_pwhash_str(out: *mut libc::c_uchar, passwd: *const libc::c_uchar, passwdlen: usize, opslimit: usize, memlimit: usize) -> libc::c_int;
    pub fn crypto_pwhash_str_verify(str_: *const libc::c_uchar, passwd: *const libc::c_uchar, passwdlen: usize) -> libc::c_int;
    pub fn crypto_pwhash_str_needs_rehash(str_: *const libc::c_uchar, opslimit: usize, memlimit: usize) -> libc::c_int;
}

// Pwhash scrypt
pub const crypto_pwhash_scryptsalsa208sha256_STRPREFIX: &str = "$7$";
pub const crypto_pwhash_scryptsalsa208sha256_OPSLIMIT_INTERACTIVE: usize = 32768;
pub const crypto_pwhash_scryptsalsa208sha256_MEMLIMIT_INTERACTIVE: usize = 268435456;
pub const crypto_pwhash_scryptsalsa208sha256_OPSLIMIT_SENSITIVE: usize = 131072;
pub const crypto_pwhash_scryptsalsa208sha256_MEMLIMIT_SENSITIVE: usize = 1073741824;
pub const crypto_pwhash_scryptsalsa208sha256_SALTBYTES: usize = 16;
pub const crypto_pwhash_scryptsalsa208sha256_STRBYTES: usize = 102;

extern "C" {
    pub fn crypto_pwhash_scryptsalsa208sha256(out: *mut libc::c_uchar, outlen: usize, passwd: *const libc::c_uchar, passwdlen: usize, salt: *const libc::c_uchar, opslimit: usize, memlimit: usize) -> libc::c_int;
    pub fn crypto_pwhash_scryptsalsa208sha256_str(out: *mut libc::c_uchar, passwd: *const libc::c_uchar, passwdlen: usize, opslimit: usize, memlimit: usize) -> libc::c_int;
    pub fn crypto_pwhash_scryptsalsa208sha256_str_verify(str_: *const libc::c_uchar, passwd: *const libc::c_uchar, passwdlen: usize) -> libc::c_int;
}

// SHorthash
pub const crypto_shorthash_SIPHASH24_BYTES: usize = 16;
pub const crypto_shorthash_SIPHASH24_KEYBYTES: usize = 32;

extern "C" {
    pub fn crypto_shorthash(out: *mut libc::c_uchar, in_: *const libc::c_uchar, inlen: usize, k: *const libc::c_uchar) -> libc::c_int;
}

// Secretstream
pub const crypto_secretstream_XCHACHA20POLY1305_KEYBYTES: usize = 32;
pub const crypto_secretstream_XCHACHA20POLY1305_NPUBBYTES: usize = 24;
pub const crypto_secretstream_XCHACHA20POLY1305_ABYTES: usize = 17;
pub const crypto_secretstream_XCHACHA20POLY1305_HEADERBYTES: usize = 24;
pub const crypto_secretstream_XCHACHA20POLY1305_TAG_MESSAGE: libc::c_uchar = 0;
pub const crypto_secretstream_XCHACHA20POLY1305_TAG_PUSH: libc::c_uchar = 1;
pub const crypto_secretstream_XCHACHA20POLY1305_TAG_REKEY: libc::c_uchar = 2;
pub const crypto_secretstream_XCHACHA20POLY1305_TAG_FINAL: libc::c_uchar = 3;

extern "C" {
    pub fn crypto_secretstream_xchacha20poly1305_statebytes() -> usize;
    pub fn crypto_secretstream_xchacha20poly1305_init_push(state: *mut libc::c_void, header: *mut libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_secretstream_xchacha20poly1305_push(state: *mut libc::c_void, c: *mut libc::c_uchar, clen: *mut usize, m: *const libc::c_uchar, mlen: usize, ad: *const libc::c_uchar, adlen: usize, tag: libc::c_uchar) -> libc::c_int;
    pub fn crypto_secretstream_xchacha20poly1305_init_pull(state: *mut libc::c_void, header: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_secretstream_xchacha20poly1305_pull(state: *mut libc::c_void, m: *mut libc::c_uchar, mlen: *mut usize, tag: *mut libc::c_uchar, c: *const libc::c_uchar, clen: usize, ad: *const libc::c_uchar, adlen: usize) -> libc::c_int;
    pub fn crypto_secretstream_xchacha20poly1305_rekey(state: *mut libc::c_void) -> ();
}

// AEAD AES-GCM
pub const crypto_aead_aes256gcm_KEYBYTES: usize = 32;
pub const crypto_aead_aes256gcm_NPUBBYTES: usize = 12;
pub const crypto_aead_aes256gcm_ABYTES: usize = 16;
pub const crypto_aead_aes256gcm_MESSAGEBYTES_MAX: usize = ((1 << 61) - 1);

extern "C" {
    pub fn crypto_aead_aes256gcm_init(ctx: *mut libc::c_void, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_aead_aes256gcm_encrypt(ctx: *mut libc::c_void, c: *mut libc::c_uchar, clen: *mut usize, m: *const libc::c_uchar, mlen: usize, ad: *const libc::c_uchar, adlen: usize, nsec: *const libc::c_uchar, npub: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_aead_aes256gcm_decrypt(ctx: *mut libc::c_void, m: *mut libc::c_uchar, mlen: *mut usize, nsec: *mut libc::c_uchar, c: *const libc::c_uchar, clen: usize, ad: *const libc::c_uchar, adlen: usize, npub: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_aead_aes256gcm_encrypt_detached(ctx: *mut libc::c_void, c: *mut libc::c_uchar, mac: *mut libc::c_uchar, maclen: *mut usize, m: *const libc::c_uchar, mlen: usize, ad: *const libc::c_uchar, adlen: usize, nsec: *const libc::c_uchar, npub: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_aead_aes256gcm_decrypt_detached(ctx: *mut libc::c_void, m: *mut libc::c_uchar, mlen: *mut usize, mac: *const libc::c_uchar, c: *const libc::c_uchar, clen: usize, ad: *const libc::c_uchar, adlen: usize, npub: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
}

// AEAD ChaCha20-Poly1305
pub const crypto_aead_chacha20poly1305_KEYBYTES: usize = 32;
pub const crypto_aead_chacha20poly1305_NPUBBYTES: usize = 12;
pub const crypto_aead_chacha20poly1305_ABYTES: usize = 16;
pub const crypto_aead_chacha20poly1305_MESSAGEBYTES_MAX: usize = ((1 << 36) - 36);

extern "C" {
    pub fn crypto_aead_chacha20poly1305_encrypt(c: *mut libc::c_uchar, clen: *mut usize, m: *const libc::c_uchar, mlen: usize, ad: *const libc::c_uchar, adlen: usize, nsec: *const libc::c_uchar, npub: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_aead_chacha20poly1305_decrypt(m: *mut libc::c_uchar, mlen: *mut usize, nsec: *mut libc::c_uchar, c: *const libc::c_uchar, clen: usize, ad: *const libc::c_uchar, adlen: usize, npub: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_aead_chacha20poly1305_encrypt_detached(c: *mut libc::c_uchar, mac: *mut libc::c_uchar, maclen: *mut usize, m: *const libc::c_uchar, mlen: usize, ad: *const libc::c_uchar, adlen: usize, nsec: *const libc::c_uchar, npub: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_aead_chacha20poly1305_decrypt_detached(m: *mut libc::c_uchar, mlen: *mut usize, c: *const libc::c_uchar, mac: *const libc::c_uchar, clen: usize, ad: *const libc::c_uchar, adlen: usize, npub: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
}

// AEAD ChaCha20-Poly1305 IETF
pub const crypto_aead_chacha20poly1305_ietf_KEYBYTES: usize = 32;
pub const crypto_aead_chacha20poly1305_ietf_NPUBBYTES: usize = 12;
pub const crypto_aead_chacha20poly1305_ietf_ABYTES: usize = 16;
pub const crypto_aead_chacha20poly1305_ietf_MESSAGEBYTES_MAX: usize = ((1 << 36) - 68);

extern "C" {
    pub fn crypto_aead_chacha20poly1305_ietf_encrypt(c: *mut libc::c_uchar, clen: *mut usize, m: *const libc::c_uchar, mlen: usize, ad: *const libc::c_uchar, adlen: usize, nsec: *const libc::c_uchar, npub: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_aead_chacha20poly1305_ietf_decrypt(m: *mut libc::c_uchar, mlen: *mut usize, nsec: *mut libc::c_uchar, c: *const libc::c_uchar, clen: usize, ad: *const libc::c_uchar, adlen: usize, npub: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_aead_chacha20poly1305_ietf_encrypt_detached(c: *mut libc::c_uchar, mac: *mut libc::c_uchar, maclen: *mut usize, m: *const libc::c_uchar, mlen: usize, ad: *const libc::c_uchar, adlen: usize, nsec: *const libc::c_uchar, npub: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_aead_chacha20poly1305_ietf_decrypt_detached(m: *mut libc::c_uchar, mlen: *mut usize, c: *const libc::c_uchar, mac: *const libc::c_uchar, clen: usize, ad: *const libc::c_uchar, adlen: usize, npub: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
}

// AEAD XChaCha20-Poly1305 IETF
pub const crypto_aead_xchacha20poly1305_ietf_KEYBYTES: usize = 32;
pub const crypto_aead_xchacha20poly1305_ietf_NPUBBYTES: usize = 24;
pub const crypto_aead_xchacha20poly1305_ietf_ABYTES: usize = 16;
pub const crypto_aead_xchacha20poly1305_ietf_MESSAGEBYTES_MAX: usize = ((1 << 36) - 68);

extern "C" {
    pub fn crypto_aead_xchacha20poly1305_ietf_encrypt(c: *mut libc::c_uchar, clen: *mut usize, m: *const libc::c_uchar, mlen: usize, ad: *const libc::c_uchar, adlen: usize, nsec: *const libc::c_uchar, npub: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_aead_xchacha20poly1305_ietf_decrypt(m: *mut libc::c_uchar, mlen: *mut usize, nsec: *mut libc::c_uchar, c: *const libc::c_uchar, clen: usize, ad: *const libc::c_uchar, adlen: usize, npub: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_aead_xchacha20poly1305_ietf_encrypt_detached(c: *mut libc::c_uchar, mac: *mut libc::c_uchar, maclen: *mut usize, m: *const libc::c_uchar, mlen: usize, ad: *const libc::c_uchar, adlen: usize, nsec: *const libc::c_uchar, npub: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_aead_xchacha20poly1305_ietf_decrypt_detached(m: *mut libc::c_uchar, mlen: *mut usize, c: *const libc::c_uchar, mac: *const libc::c_uchar, clen: usize, ad: *const libc::c_uchar, adlen: usize, npub: *const libc::c_uchar, k: *const libc::c_uchar) -> libc::c_int;
}

// Auth HMAC-SHA256
pub const crypto_auth_hmacsha256_KEYBYTES: usize = 32;
pub const crypto_auth_hmacsha256_BYTES: usize = 32;

extern "C" {
    pub fn crypto_auth_hmacsha256(out: *mut libc::c_uchar, in_: *const libc::c_uchar, inlen: usize, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_auth_hmacsha256_verify(h: *const libc::c_uchar, in_: *const libc::c_uchar, inlen: usize, k: *const libc::c_uchar) -> libc::c_int;
}

// Auth HMAC-SHA512
pub const crypto_auth_hmacsha512_KEYBYTES: usize = 64;
pub const crypto_auth_hmacsha512_BYTES: usize = 64;

extern "C" {
    pub fn crypto_auth_hmacsha512(out: *mut libc::c_uchar, in_: *const libc::c_uchar, inlen: usize, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_auth_hmacsha512_verify(h: *const libc::c_uchar, in_: *const libc::c_uchar, inlen: usize, k: *const libc::c_uchar) -> libc::c_int;
}

// Auth HMAC-SHA512256
pub const crypto_auth_hmacsha512256_KEYBYTES: usize = 64;
pub const crypto_auth_hmacsha512256_BYTES: usize = 64;

extern "C" {
    pub fn crypto_auth_hmacsha512256(out: *mut libc::c_uchar, in_: *const libc::c_uchar, inlen: usize, k: *const libc::c_uchar) -> libc::c_int;
    pub fn crypto_auth_hmacsha512256_verify(h: *const libc::c_uchar, in_: *const libc::c_uchar, inlen: usize, k: *const libc::c_uchar) -> libc::c_int;
}
