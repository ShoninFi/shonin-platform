//! The root key: one secret, made inside an attested enclave and held only in
//! enclaves' memory, from which every key that unseals anything is derived.
//!
//! - [`Root`] is the secret itself: 32 bytes from the operating system's
//!   randomness, wiped when dropped, never printed and never serialised.
//! - From it come the **root signer**, a secp256k1 key whose address the
//!   canonical chain's registry records (`rootKey().signer`), and one X25519
//!   key pair per [`Purpose`], for sealing (HPKE, RFC 9180).
//! - A purpose's public key is published as a [`PublishedKey`], signed by the
//!   root signer, so anyone can check it against the chain before sealing to
//!   it ([`seal`]); only a holder of the purpose's private key can [`open`].
//! - The root and the purposes' private keys pass between enclaves only by
//!   the [`exchange`]: the asking enclave shows an attestation token whose
//!   nonce binds a key it just made, and the holder checks the token as the
//!   attestation guest does, checks the image and project against the
//!   canonical chain ([`ChainView`]), decides what that kind of image may
//!   have (a vault's keys only with a key blessed for that vault on the
//!   vault's own chain), and seals it to that key.
//!
//! # Encodings
//!
//! Every label is versioned, so a change of format is a new label.
//!
//! | what | how |
//! | --- | --- |
//! | a derived secret | HKDF-SHA256, salt `shonin:rootkey:v1`, ikm the root, info a label below |
//! | the root signer | info `shonin:rootkey:v1:root-signer` ‖ a counter byte (0, then 1, ... until the 32 bytes are a valid scalar) |
//! | a purpose's key pair | info `shonin:rootkey:v1:purpose` ‖ the purpose's bytes, 32 bytes, then RFC 9180's `DeriveKeyPair` |
//! | a service's seed | info `shonin:rootkey:v1:service` ‖ the image's kind (1 byte, the registry's value) ‖ image digest (32) ‖ project (32), 32 bytes |
//! | a purpose's bytes | see [`Purpose::to_bytes`] |
//! | a published key's digest | `keccak256("shonin:published-key:v1" ‖ chainId (32 bytes, big-endian) ‖ purpose bytes ‖ public key)`, signed as an Ethereum signed message (EIP-191), 65 bytes |
//! | sealed to a purpose | HPKE base mode, info `shonin:sealed:v1` ‖ purpose bytes, aad `len(purpose bytes)` (2 bytes, big-endian) ‖ purpose bytes ‖ the caller's aad |
//! | the exchange's nonce | lowercase hex of `sha256("shonin-root-exchange:v1" ‖ ephemeral public key ‖ purpose bytes)` |
//! | a vault key's proof | `keccak256("shonin:root-exchange-proof:v1" ‖ ephemeral public key ‖ purpose bytes ‖ vaultChain (32 bytes) ‖ vault (20) ‖ strategyHash (32))`, signed as an Ethereum signed message |
//! | a release | HPKE base mode to the ephemeral key, info `shonin:root-exchange:v1:release`, aad `sha256(token)` ‖ purpose bytes |
//!
//! [`Sealed`] and [`exchange::Release`] are JSON with base64 fields; keys,
//! addresses and signatures are `0x` hex.

#![forbid(unsafe_code)]

pub mod body;
pub mod chain_view;
pub mod error;
pub mod exchange;
pub mod http;
pub mod launcher;
pub mod published;
pub mod purpose;
pub mod root;
pub mod sealed;
pub mod service;

pub use chain_view::{ChainView, ImageKind, NetworkView, RegistryView};
pub use error::Error;
pub use exchange::{
    ExchangeRequest, Received, Release, Requester, VaultProof, authorise, authorise_at,
    exchange_nonce,
};
pub use http::{Service, routes};
pub use published::PublishedKey;
pub use purpose::Purpose;
pub use root::{KeyPair, Root, root_leaf};
pub use sealed::{Sealed, open, seal};

/// The purpose the registry's root leaf carries where a key's leaf carries
/// its bundle: `keccak256("shonin:root-key:v1")`.
pub fn root_key_purpose() -> alloy::primitives::B256 {
    chain::attest::root_key_purpose()
}

#[cfg(test)]
mod tests;
