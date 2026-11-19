//! The node key is one key for networking (iroh) and grants (Biscuit): the same
//! seed yields the same public key bytes, text forms and signatures in all three.

use biscuit_auth::{Algorithm, KeyPair, PrivateKey, builder::AuthorizerBuilder};
use loa_identity::{MemoryKeyStore, NodeIdentity, PublicKey, Signature};

fn node() -> NodeIdentity {
    NodeIdentity::load_or_create(&MemoryKeyStore::new()).unwrap()
}

#[test]
fn iroh_endpoint_id_is_the_node_public_key() {
    let id = node();
    let iroh_secret = iroh_base::SecretKey::from_bytes(id.secret_bytes());
    let iroh_public = iroh_secret.public();

    assert_eq!(iroh_public.as_bytes(), id.public_key().as_bytes());
    assert_eq!(
        iroh_public.to_string(),
        id.public_key().to_endpoint_id_string()
    );
    assert_eq!(id.public_key().to_string(), iroh_public.to_string());

    let from_iroh_text: PublicKey = iroh_public.to_string().parse().unwrap();
    assert_eq!(from_iroh_text, id.public_key());
    let iroh_from_ours: iroh_base::PublicKey =
        id.public_key().to_endpoint_id_string().parse().unwrap();
    assert_eq!(iroh_from_ours, iroh_public);
    let iroh_from_z32 = iroh_base::PublicKey::from_z32(&iroh_public.to_z32()).unwrap();
    assert_eq!(iroh_from_z32.as_bytes(), id.public_key().as_bytes());
}

#[test]
fn signatures_cross_verify_with_iroh() {
    let id = node();
    let iroh_secret = iroh_base::SecretKey::from_bytes(id.secret_bytes());
    let message = b"dial me";

    let ours = id.sign(message);
    iroh_secret
        .public()
        .verify(message, &iroh_base::Signature::from_bytes(&ours.to_bytes()))
        .unwrap();
    assert!(
        iroh_secret
            .public()
            .verify(
                b"dial you",
                &iroh_base::Signature::from_bytes(&ours.to_bytes())
            )
            .is_err()
    );

    let theirs = iroh_secret.sign(message);
    id.public_key()
        .verify(message, &Signature::from_bytes(&theirs.to_bytes()))
        .unwrap();
    assert_eq!(theirs.to_bytes(), ours.to_bytes());
}

#[test]
fn biscuit_key_is_the_node_public_key() {
    let id = node();
    let private = PrivateKey::from_bytes(id.secret_bytes(), Algorithm::Ed25519).unwrap();
    let keypair = KeyPair::from(&private);
    let biscuit_public = keypair.public();

    assert_eq!(biscuit_public.to_bytes(), id.public_key().as_bytes());
    assert_eq!(
        biscuit_public.to_string(),
        id.public_key().to_biscuit_string()
    );
    assert_eq!(biscuit_public.print(), id.public_key().to_biscuit_string());

    let from_biscuit_text: PublicKey = biscuit_public.to_string().parse().unwrap();
    assert_eq!(from_biscuit_text, id.public_key());
    let biscuit_from_ours: biscuit_auth::PublicKey =
        id.public_key().to_biscuit_string().parse().unwrap();
    assert_eq!(biscuit_from_ours, biscuit_public);
}

#[test]
fn signatures_cross_verify_with_biscuit() {
    let id = node();
    let message = b"grant view";
    let biscuit_public: biscuit_auth::PublicKey =
        id.public_key().to_biscuit_string().parse().unwrap();
    let keypair =
        KeyPair::from(&PrivateKey::from_bytes(id.secret_bytes(), Algorithm::Ed25519).unwrap());

    // Biscuit has no public constructor for a signature from bytes. Ed25519 is
    // deterministic, so a byte-identical signature that Biscuit verifies under
    // the key parsed from our text form is exactly the signature made here.
    let ours = id.sign(message);
    let theirs = keypair.sign(message).unwrap();
    assert_eq!(theirs.to_bytes(), ours.to_bytes());
    biscuit_public.verify_signature(message, &theirs).unwrap();
    assert!(
        biscuit_public
            .verify_signature(b"grant drive", &theirs)
            .is_err()
    );

    let theirs: [u8; Signature::LENGTH] = theirs.to_bytes().try_into().unwrap();
    id.public_key()
        .verify(message, &Signature::from_bytes(&theirs))
        .unwrap();
}

#[test]
fn token_minted_with_node_key_verifies_under_node_public_key_text() {
    let id = node();
    let root =
        KeyPair::from(&PrivateKey::from_bytes(id.secret_bytes(), Algorithm::Ed25519).unwrap());
    let token = biscuit_auth::Biscuit::builder()
        .fact("right(\"view\")")
        .unwrap()
        .build(&root)
        .unwrap()
        .to_vec()
        .unwrap();

    let root_public: biscuit_auth::PublicKey = id.public_key().to_biscuit_string().parse().unwrap();
    let parsed = biscuit_auth::Biscuit::from(&token, root_public).unwrap();
    let mut authorizer = AuthorizerBuilder::new()
        .code("allow if right(\"view\");")
        .unwrap()
        .build(&parsed)
        .unwrap();
    authorizer.authorize().unwrap();

    let stranger: biscuit_auth::PublicKey =
        node().public_key().to_biscuit_string().parse().unwrap();
    assert!(biscuit_auth::Biscuit::from(&token, stranger).is_err());
}
