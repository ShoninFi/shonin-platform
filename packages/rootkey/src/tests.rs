use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, B256};
use alloy::signers::local::PrivateKeySigner;
use anyhow::Result;
use serde_json::{Value, json};
use shonin_attestation_core::{Binding, Jwk, fixture};

use crate::chain_view::{ChainView, ImageKind};
use crate::exchange::{
    CLOCK_SKEW, MAX_TOKEN_AGE, Permission, Received, Requester, VaultProof, authorise, permission,
};
use crate::purpose::{DUMMY, Purpose, REVIEWER_MODEL};
use crate::{Error, PublishedKey, Root, open, seal};

const NOW: u64 = 1_800_000_000;

/// A change to a token's claims.
type Edit = Box<dyn FnOnce(&mut Value)>;
const CANONICAL: u64 = 84_532;
const VAULT_CHAIN: u64 = 998;

fn vault() -> Address {
    Address::repeat_byte(0x0A)
}

fn strategy() -> B256 {
    B256::repeat_byte(0x5B)
}

const KINDS: [ImageKind; 5] = [
    ImageKind::Executor,
    ImageKind::Unwinder,
    ImageKind::Builder,
    ImageKind::Reviewer,
    ImageKind::Holder,
];

pub(crate) fn digest_of(kind: ImageKind) -> B256 {
    B256::repeat_byte(match kind {
        ImageKind::Executor => 0xE1,
        ImageKind::Unwinder => 0xE2,
        ImageKind::Builder => 0xE3,
        ImageKind::Reviewer => 0xE4,
        ImageKind::Holder => 0xE5,
    })
}

fn image_claim(digest: B256) -> String {
    format!("sha256:{}", hex::encode(digest))
}

/// The chains, as a test arranges them: the canonical chain's allowlists
/// and root record, and each chain's own registry of blessings, read per
/// chain as the holder reads them.
#[derive(Clone)]
pub(crate) struct StandIn {
    trusted: Vec<[u8; 32]>,
    images: HashMap<B256, ImageKind>,
    projects: HashSet<B256>,
    /// The chains whose registries are read.
    chains: HashSet<u64>,
    /// Each chain's blessings: (the chain whose registry holds it, key,
    /// vault chain, vault, bundle).
    blessed: HashSet<(u64, Address, u64, Address, B256)>,
    /// Each chain's record of the image a key was blessed from.
    key_images: HashMap<(u64, Address), B256>,
    /// The root key's signer while its record is live, shared by every
    /// clone so a test can change it under a service.
    live_root: std::sync::Arc<std::sync::Mutex<Option<Address>>>,
    down: bool,
}

impl StandIn {
    pub(crate) fn new() -> StandIn {
        StandIn {
            trusted: vec![fixture::test_jwk().key_hash()],
            images: KINDS.iter().map(|kind| (digest_of(*kind), *kind)).collect(),
            projects: [B256::from(shonin_attestation_core::project_id(
                fixture::PROJECT,
            ))]
            .into(),
            chains: [CANONICAL, VAULT_CHAIN].into(),
            blessed: HashSet::new(),
            key_images: HashMap::new(),
            live_root: Default::default(),
            down: false,
        }
    }

    /// Bless `key` for the sample vault and bundle, from `kind`'s image, on
    /// the vault's own chain's registry, where a vault key is blessed: the
    /// canonical chain's never hears of it.
    fn bless(&mut self, key: Address, kind: ImageKind) {
        self.blessed
            .insert((VAULT_CHAIN, key, VAULT_CHAIN, vault(), strategy()));
        self.key_images.insert((VAULT_CHAIN, key), digest_of(kind));
    }

    /// The registry on `chain`, if it is read.
    fn read(&self, chain: u64) -> Result<()> {
        self.up()?;
        anyhow::ensure!(self.chains.contains(&chain), "chain {chain} is not read");
        Ok(())
    }

    /// The record names `signer` as the live root key, or nothing.
    pub(crate) fn record_root(&self, signer: Option<Address>) {
        *self.live_root.lock().unwrap() = signer;
    }

    fn up(&self) -> Result<()> {
        anyhow::ensure!(!self.down, "the node is down");
        Ok(())
    }
}

impl ChainView for StandIn {
    fn chain_id(&self) -> u64 {
        CANONICAL
    }

    fn reads_chain(&self, chain: u64) -> bool {
        self.chains.contains(&chain)
    }

    async fn pin(&self, chain: u64) -> Result<u64> {
        self.read(chain)?;
        Ok(1)
    }

    async fn trusted_google_keys(&self, _: u64) -> Result<Vec<[u8; 32]>> {
        self.up()?;
        Ok(self.trusted.clone())
    }

    async fn image_kind(&self, image_digest: B256, _: u64) -> Result<Option<ImageKind>> {
        self.up()?;
        Ok(self.images.get(&image_digest).copied())
    }

    async fn project_allowed(&self, project_id: B256, _: u64) -> Result<bool> {
        self.up()?;
        Ok(self.projects.contains(&project_id))
    }

    async fn is_blessed_for(
        &self,
        key: Address,
        vault_chain: u64,
        vault: Address,
        strategy_hash: B256,
        _: u64,
    ) -> Result<bool> {
        self.read(vault_chain)?;
        Ok(self
            .blessed
            .contains(&(vault_chain, key, vault_chain, vault, strategy_hash)))
    }

    async fn key_image(&self, chain: u64, key: Address, _: u64) -> Result<Option<B256>> {
        self.read(chain)?;
        Ok(self.key_images.get(&(chain, key)).copied())
    }

    async fn live_root_signer(&self, _: u64) -> Result<Option<Address>> {
        self.up()?;
        Ok(*self.live_root.lock().unwrap())
    }
}

/// Google's keys as fetched: the real ones, and the fixture's test key.
fn google_keys() -> Vec<Jwk> {
    let mut keys = Jwk::from_jwks(fixture::GOOGLE_JWKS_2026_10_06).expect("Google's JWKS parses");
    keys.push(fixture::test_jwk());
    keys
}

/// A production token from `kind`'s image, issued 30 seconds ago, its nonce
/// `nonce`, with `edit` applied to its claims.
fn token(nonce: &str, kind: ImageKind, edit: impl FnOnce(&mut Value)) -> String {
    let mut claims = fixture::claims(&Binding::default(), NOW - 30);
    claims["eat_nonce"] = json!([nonce]);
    claims["submods"]["container"]["image_digest"] = json!(image_claim(digest_of(kind)));
    edit(&mut claims);
    fixture::sign(&claims)
}

async fn ask(
    chain: &StandIn,
    root: &Root,
    requester: &Requester,
    token: String,
    proof: Option<VaultProof>,
) -> Result<Received, Error> {
    let request = requester.request(token, proof);
    let release = authorise(root, &google_keys(), &request, NOW, chain).await?;
    requester.open(&request, &release)
}

/// Ask for `purpose` as `kind`, with a good token and, where `vault_key` is
/// given, its proof for the sample vault and bundle.
async fn ask_as(
    chain: &StandIn,
    root: &Root,
    kind: ImageKind,
    purpose: &Purpose,
    vault_key: Option<&PrivateKeySigner>,
) -> Result<Received, Error> {
    let requester = Requester::new(purpose.clone()).unwrap();
    let proof = vault_key.map(|key| {
        requester
            .prove(key, VAULT_CHAIN, vault(), strategy())
            .unwrap()
    });
    let token = token(&requester.nonce(), kind, |_| {});
    ask(chain, root, &requester, token, proof).await
}

/// The fixture's operator project, as the registry lists it.
fn project() -> B256 {
    B256::from(shonin_attestation_core::project_id(fixture::PROJECT))
}

fn venue() -> Purpose {
    Purpose::venue(VAULT_CHAIN, vault(), "hyperliquid").unwrap()
}

fn purposes() -> Vec<Purpose> {
    vec![
        Purpose::Builder,
        Purpose::Bundle(strategy()),
        Purpose::Bundle(B256::repeat_byte(0x01)),
        venue(),
        Purpose::venue(VAULT_CHAIN, vault(), "lighter").unwrap(),
        Purpose::venue(VAULT_CHAIN + 1, vault(), "hyperliquid").unwrap(),
        Purpose::venue(VAULT_CHAIN, Address::repeat_byte(0x0B), "hyperliquid").unwrap(),
        Purpose::platform(REVIEWER_MODEL).unwrap(),
        Purpose::platform(DUMMY).unwrap(),
    ]
}

#[test]
fn derivations_are_deterministic_and_domain_separated() {
    let root = Root::for_tests(7);
    let again = Root::for_tests(7);
    let other = Root::for_tests(8);
    assert_eq!(root.signer_address(), again.signer_address());
    assert_ne!(root.signer_address(), other.signer_address());

    let signer_secret = root.signer().to_bytes();
    let mut publics = HashSet::new();
    let mut privates = HashSet::new();
    for purpose in purposes() {
        let pair = root.key_pair(&purpose).unwrap();
        assert_eq!(
            pair.public_key(),
            again.key_pair(&purpose).unwrap().public_key()
        );
        assert_ne!(
            pair.public_key(),
            other.key_pair(&purpose).unwrap().public_key()
        );
        assert!(publics.insert(pair.public_key()), "{purpose} shares a key");
        let private = pair.private_bytes().to_vec();
        assert_ne!(private.as_slice(), signer_secret.as_slice());
        assert_ne!(private.as_slice(), root.expose().as_slice());
        assert!(privates.insert(private));
    }
    assert_eq!(
        root.key_pair(&Purpose::Root).unwrap_err(),
        Error::RootHasNoKeyPair
    );
    assert_eq!(
        root.key_pair(&Purpose::Service).unwrap_err(),
        Error::NoKeyPair(Purpose::Service)
    );
}

#[test]
fn purposes_have_one_string_form_and_distinct_bytes() {
    let mut all = purposes();
    all.push(Purpose::Root);
    all.push(Purpose::Service);
    let mut bytes = HashSet::new();
    for purpose in &all {
        let text = purpose.to_string();
        assert_eq!(&text.parse::<Purpose>().unwrap(), purpose, "{text}");
        let json = serde_json::to_string(purpose).unwrap();
        assert_eq!(&serde_json::from_str::<Purpose>(&json).unwrap(), purpose);
        assert!(bytes.insert(purpose.to_bytes()));
    }
    assert_eq!(
        venue().to_string(),
        "venue:998:0x0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a:hyperliquid"
    );
    for bad in [
        "",
        "root:",
        "bundle:0x12",
        "venue:998:0x0a:hyperliquid",
        "venue:998:0x0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a:",
        "venue:998:0x0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a:Hyper",
        "platform:",
        "platform:a b",
        "other:thing",
    ] {
        assert!(bad.parse::<Purpose>().is_err(), "{bad:?} parsed");
    }
}

#[test]
fn a_published_key_verifies_only_against_its_root_signer_purpose_and_chain() {
    let root = Root::for_tests(7);
    let signer = root.signer_address();
    let purpose = Purpose::platform(DUMMY).unwrap();
    let published = root.publish(&purpose, CANONICAL).unwrap();
    assert_eq!(
        published.public_key.0,
        root.key_pair(&purpose).unwrap().public_key()
    );
    published.verify(signer, CANONICAL, &purpose).unwrap();

    let other = Root::for_tests(8);
    assert_eq!(
        published.verify(other.signer_address(), CANONICAL, &purpose),
        Err(Error::BadPublishedKey)
    );
    assert_eq!(
        published.verify(signer, CANONICAL + 1, &purpose),
        Err(Error::BadPublishedKey)
    );
    assert_eq!(
        published.verify(signer, CANONICAL, &Purpose::Builder),
        Err(Error::BadPublishedKey)
    );
    assert_eq!(
        published.verify(Address::ZERO, CANONICAL, &purpose),
        Err(Error::BadPublishedKey)
    );

    // Relabelled, or another key swapped in under the same signature.
    let relabelled = PublishedKey {
        purpose: Purpose::Builder,
        ..published.clone()
    };
    assert!(
        relabelled
            .verify(signer, CANONICAL, &Purpose::Builder)
            .is_err()
    );
    let rechained = PublishedKey {
        chain_id: CANONICAL + 1,
        ..published.clone()
    };
    assert!(rechained.verify(signer, CANONICAL + 1, &purpose).is_err());
    let swapped = PublishedKey {
        public_key: root
            .publish(&Purpose::Builder, CANONICAL)
            .unwrap()
            .public_key,
        ..published.clone()
    };
    assert!(swapped.verify(signer, CANONICAL, &purpose).is_err());

    // Its JSON reads back to the same key.
    let json = serde_json::to_string(&published).unwrap();
    assert_eq!(
        serde_json::from_str::<PublishedKey>(&json).unwrap(),
        published
    );
}

#[test]
fn a_sealed_message_opens_only_with_its_purposes_key_and_aad() {
    let root = Root::for_tests(7);
    let purpose = Purpose::platform(DUMMY).unwrap();
    let published = root.publish(&purpose, CANONICAL).unwrap();
    let sealed = seal(&published, b"the dummy secret", b"context").unwrap();
    let json = serde_json::to_string(&sealed).unwrap();
    let sealed: crate::Sealed = serde_json::from_str(&json).unwrap();

    let pair = root.key_pair(&purpose).unwrap();
    assert_eq!(
        open(&pair, &sealed, b"context").unwrap().as_slice(),
        b"the dummy secret"
    );
    assert_eq!(open(&pair, &sealed, b"other").unwrap_err(), Error::Open);
    assert_eq!(open(&pair, &sealed, b"").unwrap_err(), Error::Open);

    // Another purpose's key, and another root's key for the same purpose.
    let builder = root.key_pair(&Purpose::Builder).unwrap();
    assert_eq!(
        open(&builder, &sealed, b"context").unwrap_err(),
        Error::Open
    );
    let stranger = Root::for_tests(8).key_pair(&purpose).unwrap();
    assert_eq!(
        open(&stranger, &sealed, b"context").unwrap_err(),
        Error::Open
    );

    // A message relabelled for the key that is tried still doesn't open:
    // the purpose is in the key schedule and the aad.
    let reviewer = Purpose::platform(REVIEWER_MODEL).unwrap();
    let reviewer_pair = root.key_pair(&reviewer).unwrap();
    let relabelled = crate::Sealed {
        purpose: reviewer.clone(),
        public_key: alloy::primitives::FixedBytes(reviewer_pair.public_key()),
        ..sealed.clone()
    };
    assert_eq!(
        open(&reviewer_pair, &relabelled, b"context").unwrap_err(),
        Error::Open
    );
    let mut tampered = sealed.clone();
    tampered.ciphertext[0] ^= 1;
    assert_eq!(open(&pair, &tampered, b"context").unwrap_err(), Error::Open);
}

#[tokio::test]
async fn the_exchange_releases_each_kind_only_what_it_may_have() {
    let root = Root::for_tests(7);
    let mut asked = purposes();
    asked.push(Purpose::Root);
    asked.push(Purpose::Service);
    asked.push(Purpose::platform("something-else").unwrap());

    for kind in KINDS {
        let mut chain = StandIn::new();
        let vault_key = PrivateKeySigner::random();
        chain.bless(vault_key.address(), kind);
        for purpose in &asked {
            let wants_proof = permission(kind, purpose) == Permission::WithVaultProof;
            let got = ask_as(
                &chain,
                &root,
                kind,
                purpose,
                wants_proof.then_some(&vault_key),
            )
            .await;
            let expected = match (kind, purpose) {
                (ImageKind::Holder | ImageKind::Executor, Purpose::Root) => true,
                (ImageKind::Executor | ImageKind::Unwinder, Purpose::Venue { vault: v, .. }) => {
                    *v == vault()
                        && matches!(purpose, Purpose::Venue { vault_chain, .. } if *vault_chain == VAULT_CHAIN)
                }
                (ImageKind::Executor, Purpose::Bundle(hash)) => *hash == strategy(),
                (ImageKind::Builder | ImageKind::Reviewer, Purpose::Builder) => true,
                (ImageKind::Builder | ImageKind::Reviewer, Purpose::Service) => true,
                (ImageKind::Reviewer, Purpose::Platform(name)) => {
                    name == REVIEWER_MODEL || name == DUMMY
                }
                _ => false,
            };
            match got {
                Ok(Received::Root(received)) => {
                    assert!(expected, "{kind} was given {purpose}");
                    assert_eq!(*purpose, Purpose::Root);
                    assert_eq!(received.expose(), root.expose());
                }
                Ok(Received::Key(pair)) => {
                    assert!(expected, "{kind} was given {purpose}");
                    assert_eq!(pair.purpose(), purpose);
                    assert_eq!(
                        pair.public_key(),
                        root.key_pair(purpose).unwrap().public_key()
                    );
                }
                Ok(Received::Seed(seed)) => {
                    assert!(expected, "{kind} was given {purpose}");
                    assert_eq!(*purpose, Purpose::Service);
                    assert_eq!(*seed, *root.service_seed(kind, digest_of(kind), project()));
                }
                Err(error) => {
                    assert!(!expected, "{kind} was refused {purpose}: {error}");
                    match permission(kind, purpose) {
                        Permission::Never => assert_eq!(
                            error,
                            Error::NotForKind {
                                kind,
                                purpose: purpose.clone()
                            }
                        ),
                        // The proof is for the sample vault and bundle only.
                        _ => assert_eq!(error, Error::ProofMismatch, "{kind} {purpose}"),
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn a_key_received_opens_what_was_sealed_to_its_purpose() {
    let root = Root::for_tests(7);
    let chain = StandIn::new();
    let purpose = Purpose::platform(DUMMY).unwrap();
    let published = root.publish(&purpose, CANONICAL).unwrap();
    published
        .verify(root.signer_address(), CANONICAL, &purpose)
        .unwrap();
    let sealed = seal(&published, b"the dummy secret", b"").unwrap();
    let Received::Key(pair) = ask_as(&chain, &root, ImageKind::Reviewer, &purpose, None)
        .await
        .unwrap()
    else {
        panic!("a reviewer asking for a platform secret is given a key");
    };
    assert_eq!(
        open(&pair, &sealed, b"").unwrap().as_slice(),
        b"the dummy secret"
    );

    // The root passed to another holder derives the same keys.
    let Received::Root(passed) = ask_as(&chain, &root, ImageKind::Holder, &Purpose::Root, None)
        .await
        .unwrap()
    else {
        panic!("a holder asking for the root is given it");
    };
    assert_eq!(passed.signer_address(), root.signer_address());
    let pair = passed.key_pair(&purpose).unwrap();
    assert_eq!(
        open(&pair, &sealed, b"").unwrap().as_slice(),
        b"the dummy secret"
    );
}

/// Ask for the dummy secret as a reviewer, with the token's claims edited
/// by `edit`.
async fn reviewer_with(chain: &StandIn, edit: impl FnOnce(&mut Value)) -> Result<Received, Error> {
    let root = Root::for_tests(7);
    let requester = Requester::new(Purpose::platform(DUMMY).unwrap()).unwrap();
    let token = token(&requester.nonce(), ImageKind::Reviewer, edit);
    ask(chain, &root, &requester, token, None).await
}

#[tokio::test]
async fn a_token_failing_any_of_the_guests_checks_is_refused() {
    use shonin_attestation_core::Error as Guest;
    let chain = StandIn::new();
    assert!(reviewer_with(&chain, |_| {}).await.is_ok());

    let cases: Vec<(&str, Edit, Error)> = vec![
        (
            "another audience",
            Box::new(|c| c["aud"] = json!("https://example.com")),
            Error::Token(Guest::Claim("/aud")),
        ),
        (
            "another issuer",
            Box::new(|c| c["iss"] = json!("https://example.com")),
            Error::Token(Guest::Claim("/iss")),
        ),
        (
            "a debug instance",
            Box::new(|c| c["dbgstat"] = json!("enabled")),
            Error::Token(Guest::Claim("/dbgstat")),
        ),
        (
            "not Confidential Space",
            Box::new(|c| c["swname"] = json!("GCE")),
            Error::Token(Guest::Claim("/swname")),
        ),
        (
            "not Intel TDX",
            Box::new(|c| c["hwmodel"] = json!("GCP_AMD_SEV")),
            Error::Token(Guest::Claim("/hwmodel")),
        ),
        (
            "no secure boot",
            Box::new(|c| c["secboot"] = json!(false)),
            Error::Token(Guest::Claim("/secboot")),
        ),
        (
            "not a stable launcher",
            Box::new(|c| {
                c["submods"]["confidential_space"]["support_attributes"] = json!(["LATEST"])
            }),
            Error::Token(Guest::Claim(
                "/submods/confidential_space/support_attributes",
            )),
        ),
        (
            "a command override",
            Box::new(|c| c["submods"]["container"]["cmd_override"] = json!(["--debug"])),
            Error::Token(Guest::Claim("/submods/container/cmd_override")),
        ),
        (
            "an environment override",
            Box::new(|c| c["submods"]["container"]["env_override"] = json!({"A": "b"})),
            Error::Token(Guest::Claim("/submods/container/env_override")),
        ),
        (
            "expired",
            Box::new(|c| {
                c["iat"] = json!(NOW - 4000);
                c["nbf"] = json!(NOW - 4000);
                c["exp"] = json!(NOW - 400);
            }),
            Error::Token(Guest::NotFresh),
        ),
        (
            "issued further in the future than clocks differ",
            Box::new(|c| {
                c["iat"] = json!(NOW + CLOCK_SKEW + 1);
                c["nbf"] = json!(NOW + CLOCK_SKEW + 1);
            }),
            Error::Token(Guest::NotFresh),
        ),
        (
            "older than an exchange accepts",
            Box::new(|c| {
                c["iat"] = json!(NOW - MAX_TOKEN_AGE - 1);
                c["nbf"] = json!(NOW - MAX_TOKEN_AGE - 1);
            }),
            Error::Stale,
        ),
    ];
    for (what, edit, expected) in cases {
        assert_eq!(
            reviewer_with(&chain, edit).await.unwrap_err(),
            expected,
            "{what}"
        );
    }
}

#[tokio::test]
async fn a_token_stamped_by_a_clock_a_little_ahead_is_accepted() {
    // Google stamps a token seconds ahead of the machines' own clocks, and a
    // requester sends it at once: the holder takes it, up to the skew.
    let chain = StandIn::new();
    let received = reviewer_with(&chain, |c| {
        c["iat"] = json!(NOW + CLOCK_SKEW);
        c["nbf"] = json!(NOW + CLOCK_SKEW);
    })
    .await;
    assert!(received.is_ok(), "{received:?}");
}

#[tokio::test]
async fn a_token_for_another_nonce_or_ephemeral_key_is_refused() {
    let root = Root::for_tests(7);
    let chain = StandIn::new();
    let purpose = Purpose::platform(DUMMY).unwrap();
    let requester = Requester::new(purpose.clone()).unwrap();

    // Another enclave's ephemeral key: its token doesn't name ours.
    let other = Requester::new(purpose.clone()).unwrap();
    let token_ = token(&other.nonce(), ImageKind::Reviewer, |_| {});
    assert_eq!(
        ask(&chain, &root, &requester, token_, None)
            .await
            .unwrap_err(),
        Error::Nonce
    );
    // A blessing's nonce.
    let token_ = token(&fixture::binding().nonce(), ImageKind::Reviewer, |_| {});
    assert_eq!(
        ask(&chain, &root, &requester, token_, None)
            .await
            .unwrap_err(),
        Error::Nonce
    );
    // Our key, another purpose.
    let reviewer = Requester::new(Purpose::platform(REVIEWER_MODEL).unwrap()).unwrap();
    let request = reviewer.request(token(&requester.nonce(), ImageKind::Reviewer, |_| {}), None);
    assert_eq!(
        authorise(&root, &google_keys(), &request, NOW, &chain)
            .await
            .unwrap_err(),
        Error::Nonce
    );
    // A token whose nonce was ours, sent with another ephemeral key: the
    // release would be to a key the token doesn't bind.
    let mut request =
        requester.request(token(&requester.nonce(), ImageKind::Reviewer, |_| {}), None);
    request.ephemeral_public = alloy::primitives::FixedBytes(other.ephemeral_public());
    assert_eq!(
        authorise(&root, &google_keys(), &request, NOW, &chain)
            .await
            .unwrap_err(),
        Error::Nonce
    );
}

#[tokio::test]
async fn a_replayed_request_gives_nothing_its_sender_can_open() {
    let root = Root::for_tests(7);
    let chain = StandIn::new();
    let purpose = Purpose::platform(DUMMY).unwrap();
    let requester = Requester::new(purpose.clone()).unwrap();
    let request = requester.request(token(&requester.nonce(), ImageKind::Reviewer, |_| {}), None);
    let first = authorise(&root, &google_keys(), &request, NOW, &chain)
        .await
        .unwrap();
    let replayed = authorise(&root, &google_keys(), &request, NOW, &chain)
        .await
        .unwrap();
    assert_ne!(first.enc, replayed.enc);

    // Whoever replays it holds another ephemeral key, which opens nothing.
    let thief = Requester::new(purpose.clone()).unwrap();
    assert_eq!(thief.open(&request, &replayed).unwrap_err(), Error::Open);
    let mut theirs = request.clone();
    theirs.ephemeral_public = alloy::primitives::FixedBytes(thief.ephemeral_public());
    assert_eq!(thief.open(&theirs, &replayed).unwrap_err(), Error::Open);
    assert!(requester.open(&request, &replayed).is_ok());

    // And once the token is older than the exchange accepts, it is refused.
    assert_eq!(
        authorise(&root, &google_keys(), &request, NOW + MAX_TOKEN_AGE, &chain)
            .await
            .unwrap_err(),
        Error::Stale
    );
}

#[tokio::test]
async fn an_image_or_project_the_chain_does_not_allow_is_refused() {
    let root = Root::for_tests(7);

    let mut chain = StandIn::new();
    chain.images.remove(&digest_of(ImageKind::Reviewer));
    let dummy = Purpose::platform(DUMMY).unwrap();
    assert_eq!(
        ask_as(&chain, &root, ImageKind::Reviewer, &dummy, None)
            .await
            .unwrap_err(),
        Error::ImageNotAllowed
    );

    // The reviewer's image allowed as a builder gets a builder's keys only.
    chain
        .images
        .insert(digest_of(ImageKind::Reviewer), ImageKind::Builder);
    assert_eq!(
        ask_as(&chain, &root, ImageKind::Reviewer, &dummy, None)
            .await
            .unwrap_err(),
        Error::NotForKind {
            kind: ImageKind::Builder,
            purpose: dummy.clone()
        }
    );

    let mut chain = StandIn::new();
    chain.projects.clear();
    assert_eq!(
        ask_as(&chain, &root, ImageKind::Reviewer, &dummy, None)
            .await
            .unwrap_err(),
        Error::ProjectNotAllowed
    );

    let mut chain = StandIn::new();
    chain.down = true;
    assert_eq!(
        ask_as(&chain, &root, ImageKind::Reviewer, &dummy, None)
            .await
            .unwrap_err(),
        Error::ChainUnavailable
    );
}

#[tokio::test]
async fn a_google_key_the_chains_checkers_do_not_trust_signs_nothing() {
    let root = Root::for_tests(7);
    let dummy = Purpose::platform(DUMMY).unwrap();

    // The checkers trust Google's real keys, not the one that signed.
    let mut chain = StandIn::new();
    chain.trusted = Jwk::from_jwks(fixture::GOOGLE_JWKS_2026_10_06)
        .unwrap()
        .iter()
        .map(Jwk::key_hash)
        .collect();
    assert_eq!(
        ask_as(&chain, &root, ImageKind::Reviewer, &dummy, None)
            .await
            .unwrap_err(),
        Error::UntrustedGoogleKey
    );

    // A key nobody published at all.
    let requester = Requester::new(dummy.clone()).unwrap();
    let mut claims = fixture::claims(&Binding::default(), NOW - 30);
    claims["eat_nonce"] = json!([requester.nonce()]);
    claims["submods"]["container"]["image_digest"] =
        json!(image_claim(digest_of(ImageKind::Reviewer)));
    let unknown = fixture::sign_with(
        &fixture::test_private_key(),
        &fixture::header("nobody-published-this"),
        &claims,
    );
    assert_eq!(
        ask(&StandIn::new(), &root, &requester, unknown, None)
            .await
            .unwrap_err(),
        Error::Token(shonin_attestation_core::Error::UnknownKey)
    );
}

/// A PKI token from `kind`'s image under `chain` (the test chain's shape,
/// issued 30 seconds ago), its nonce `nonce`, signed by the leaf's key.
fn pki_token(nonce: &str, kind: ImageKind, chain: &[Vec<u8>]) -> String {
    let mut claims = fixture::claims(&Binding::default(), NOW - 30);
    claims["eat_nonce"] = json!([nonce]);
    claims["submods"]["container"]["image_digest"] = json!(image_claim(digest_of(kind)));
    fixture::chain::token(
        chain,
        &fixture::chain::key(fixture::chain::LEAF_SEED),
        &claims,
    )
}

/// The checkers as they stand after a rotation to the root: Google's keys
/// as pinned, and the test chain's root.
fn trusting_the_root() -> StandIn {
    let mut chain = StandIn::new();
    chain.trusted = Jwk::from_jwks(fixture::GOOGLE_JWKS_2026_10_06)
        .unwrap()
        .iter()
        .map(Jwk::key_hash)
        .chain([fixture::chain::root_hash(NOW - 30)])
        .collect();
    chain
}

#[test]
fn the_genuine_pki_token_is_accepted_by_the_exchange_s_check_under_google_s_root() {
    use shonin_attestation_core::confidential_space_root_hash;
    let token = fixture::REAL_PKI_TOKEN.trim();
    let nonce = fixture::real_binding().nonce();
    let now = fixture::REAL_PKI_IAT + 60;
    // No published Google key is needed: the chain is the token's own.
    let attested =
        crate::exchange::check_token(token, &[], &[confidential_space_root_hash()], now, &nonce)
            .unwrap();
    assert_eq!(attested.key_hash, confidential_space_root_hash());
    assert_eq!(attested.issued_at, fixture::REAL_PKI_IAT);
    assert_eq!(
        hex::encode(attested.image_digest),
        fixture::REAL_IMAGE_DIGEST.trim_start_matches("sha256:")
    );
    assert_eq!(
        attested.project_id,
        B256::from(shonin_attestation_core::project_id(fixture::REAL_PROJECT))
    );
    // Checkers that trust only Google's signing keys do not vouch for it.
    let keys_only: Vec<[u8; 32]> = Jwk::from_jwks(fixture::GOOGLE_JWKS_2026_10_06)
        .unwrap()
        .iter()
        .map(Jwk::key_hash)
        .collect();
    assert_eq!(
        crate::exchange::check_token(token, &google_keys(), &keys_only, now, &nonce).unwrap_err(),
        Error::UntrustedRoot
    );
    // Nor for a request whose nonce it does not bind.
    assert_eq!(
        crate::exchange::check_token(
            token,
            &[],
            &[confidential_space_root_hash()],
            now,
            &"00".repeat(32)
        )
        .unwrap_err(),
        Error::Nonce
    );
}

#[tokio::test]
async fn a_pki_token_under_a_trusted_root_obtains_what_its_image_may_have() {
    let root = Root::for_tests(7);
    let requester = Requester::new(Purpose::platform(DUMMY).unwrap()).unwrap();
    let token = pki_token(
        &requester.nonce(),
        ImageKind::Reviewer,
        &fixture::chain::chain(NOW - 30),
    );
    assert!(
        ask(&trusting_the_root(), &root, &requester, token, None)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_certificate_chain_the_guest_refuses_is_refused_by_the_exchange() {
    use fixture::chain::{INTERMEDIATE_SEED, LEAF_SEED, OTHER_ROOT_SEED, ROOT_SEED, Spec, key};
    use shonin_attestation_core::Error as Guest;
    let root = Root::for_tests(7);
    let iat = NOW - 30;
    let (leaf, intermediate, top) = (Spec::leaf(iat), Spec::intermediate(iat), Spec::root(iat));
    let refused = [
        (
            fixture::chain::chain_of(
                &leaf,
                &Spec {
                    not_after: iat - 1,
                    ..intermediate.clone()
                },
                &top,
            ),
            Error::Token(Guest::Chain("validity")),
        ),
        (
            fixture::chain::chain_of(
                &Spec {
                    not_before: iat + 1,
                    ..leaf.clone()
                },
                &intermediate,
                &top,
            ),
            Error::Token(Guest::Chain("validity")),
        ),
        (
            fixture::chain::chain_of(
                &leaf,
                &Spec {
                    basic: Some((false, None)),
                    ..intermediate.clone()
                },
                &top,
            ),
            Error::Token(Guest::Chain("not a CA")),
        ),
        (
            {
                // The leaf signed by the root's key, not the intermediate's.
                let mut chain = fixture::chain::chain(iat);
                chain[0] = fixture::chain::certificate(
                    &leaf,
                    &key(LEAF_SEED).to_public_key(),
                    &key(ROOT_SEED),
                );
                chain
            },
            Error::Token(Guest::Chain("signature")),
        ),
        (
            {
                // A self-consistent chain under a root no checker pins.
                let other = key(OTHER_ROOT_SEED);
                let intermediate_key = key(INTERMEDIATE_SEED);
                vec![
                    fixture::chain::certificate(
                        &leaf,
                        &key(LEAF_SEED).to_public_key(),
                        &intermediate_key,
                    ),
                    fixture::chain::certificate(
                        &intermediate,
                        &intermediate_key.to_public_key(),
                        &other,
                    ),
                    fixture::chain::certificate(&top, &other.to_public_key(), &other),
                ]
            },
            Error::UntrustedRoot,
        ),
    ];
    for (chain, refusal) in refused {
        let requester = Requester::new(Purpose::platform(DUMMY).unwrap()).unwrap();
        let token = pki_token(&requester.nonce(), ImageKind::Reviewer, &chain);
        assert_eq!(
            ask(&trusting_the_root(), &root, &requester, token, None)
                .await
                .unwrap_err(),
            refusal
        );
    }
    // A token signed by a key other than its leaf's.
    let requester = Requester::new(Purpose::platform(DUMMY).unwrap()).unwrap();
    let mut claims = fixture::claims(&Binding::default(), iat);
    claims["eat_nonce"] = json!([requester.nonce()]);
    claims["submods"]["container"]["image_digest"] =
        json!(image_claim(digest_of(ImageKind::Reviewer)));
    let forged = fixture::chain::token(
        &fixture::chain::chain(iat),
        &key(INTERMEDIATE_SEED),
        &claims,
    );
    assert_eq!(
        ask(&trusting_the_root(), &root, &requester, forged, None)
            .await
            .unwrap_err(),
        Error::Token(Guest::BadSignature)
    );
}

#[tokio::test]
async fn after_google_rotates_its_key_an_oidc_token_is_refused_and_a_pki_token_accepted() {
    // Google now signs OIDC tokens with a key it publishes (the test key,
    // among the keys fetched) that no checker pins; the checkers pin the
    // earlier keys and the root.
    let root = Root::for_tests(7);
    let chain = trusting_the_root();
    let dummy = Purpose::platform(DUMMY).unwrap();
    assert_eq!(
        ask_as(&chain, &root, ImageKind::Reviewer, &dummy, None)
            .await
            .unwrap_err(),
        Error::UntrustedGoogleKey
    );
    let requester = Requester::new(dummy).unwrap();
    let token = pki_token(
        &requester.nonce(),
        ImageKind::Reviewer,
        &fixture::chain::chain(NOW - 30),
    );
    assert!(ask(&chain, &root, &requester, token, None).await.is_ok());
}

#[tokio::test]
async fn a_vaults_key_needs_a_proof_from_a_key_blessed_for_that_vault() {
    let root = Root::for_tests(7);
    let mut chain = StandIn::new();
    let vault_key = PrivateKeySigner::random();
    chain.bless(vault_key.address(), ImageKind::Executor);
    let venue = venue();
    let bundle = Purpose::Bundle(strategy());

    for purpose in [&venue, &bundle] {
        assert!(
            ask_as(
                &chain,
                &root,
                ImageKind::Executor,
                purpose,
                Some(&vault_key)
            )
            .await
            .is_ok()
        );
        assert_eq!(
            ask_as(&chain, &root, ImageKind::Executor, purpose, None)
                .await
                .unwrap_err(),
            Error::ProofMissing(purpose.clone())
        );
        // A key that isn't blessed for the vault.
        let stranger = PrivateKeySigner::random();
        assert_eq!(
            ask_as(&chain, &root, ImageKind::Executor, purpose, Some(&stranger))
                .await
                .unwrap_err(),
            Error::NotBlessed
        );
    }

    // A key blessed from another image than the asker's.
    let mut elsewhere = chain.clone();
    elsewhere.bless(vault_key.address(), ImageKind::Unwinder);
    assert_eq!(
        ask_as(
            &elsewhere,
            &root,
            ImageKind::Executor,
            &venue,
            Some(&vault_key)
        )
        .await
        .unwrap_err(),
        Error::NotBlessed
    );

    // A proof signed for another request (another ephemeral key).
    let requester = Requester::new(venue.clone()).unwrap();
    let other = Requester::new(venue.clone()).unwrap();
    let proof = other
        .prove(&vault_key, VAULT_CHAIN, vault(), strategy())
        .unwrap();
    let token_ = token(&requester.nonce(), ImageKind::Executor, |_| {});
    assert_eq!(
        ask(&chain, &root, &requester, token_, Some(proof))
            .await
            .unwrap_err(),
        Error::NotBlessed,
        "a proof over another request recovers to another key"
    );

    // A proof whose fields were changed after signing.
    let mut proof = requester
        .prove(&vault_key, VAULT_CHAIN, vault(), strategy())
        .unwrap();
    proof.signature = alloy::primitives::Bytes::from(vec![0u8; 3]);
    let token_ = token(&requester.nonce(), ImageKind::Executor, |_| {});
    assert_eq!(
        ask(&chain, &root, &requester, token_, Some(proof))
            .await
            .unwrap_err(),
        Error::ProofInvalid
    );

    // A proof for another vault than the venue key's.
    let elsewhere_venue =
        Purpose::venue(VAULT_CHAIN, Address::repeat_byte(0x0B), "hyperliquid").unwrap();
    assert_eq!(
        ask_as(
            &chain,
            &root,
            ImageKind::Executor,
            &elsewhere_venue,
            Some(&vault_key)
        )
        .await
        .unwrap_err(),
        Error::ProofMismatch
    );
    // A bundle the vault isn't running.
    assert_eq!(
        ask_as(
            &chain,
            &root,
            ImageKind::Executor,
            &Purpose::Bundle(B256::repeat_byte(0x01)),
            Some(&vault_key)
        )
        .await
        .unwrap_err(),
        Error::ProofMismatch
    );
}

/// The exchange relies on the guest checking the nonce after everything
/// else: a token failing any other check, with a nonce that matches
/// nothing, must be refused for that check, never for its nonce.
#[test]
fn the_guest_checks_its_nonce_after_everything_else() {
    use shonin_attestation_core::{Error as Guest, Input, verify};
    let edits: Vec<Edit> = vec![
        Box::new(|c| c["iss"] = json!("x")),
        Box::new(|c| c["aud"] = json!("x")),
        Box::new(|c| c["exp"] = json!(NOW - 1)),
        Box::new(|c| c["nbf"] = json!(NOW + 1)),
        Box::new(|c| c["dbgstat"] = json!("x")),
        Box::new(|c| c["swname"] = json!("x")),
        Box::new(|c| c["hwmodel"] = json!("x")),
        Box::new(|c| c["secboot"] = json!(false)),
        Box::new(|c| c["submods"]["confidential_space"]["support_attributes"] = json!([])),
        Box::new(|c| c["submods"]["container"]["cmd_override"] = json!(["x"])),
        Box::new(|c| c["submods"]["container"]["env_override"] = json!({"x": "y"})),
        Box::new(|c| c["submods"]["container"]["image_digest"] = json!("x")),
        Box::new(|c| c["submods"]["gce"]["project_id"] = json!("")),
    ];
    for edit in edits {
        let mut claims = fixture::claims(&Binding::default(), NOW - 30);
        claims["eat_nonce"] = json!(["matches-nothing"]);
        edit(&mut claims);
        let input = Input {
            token: fixture::sign(&claims),
            keys: vec![fixture::test_jwk()],
            now: NOW,
            binding: Binding::default(),
        };
        let error = verify(&input).unwrap_err();
        assert_ne!(error, Guest::Nonce, "{claims}");
    }
    let mut claims = fixture::claims(&Binding::default(), NOW - 30);
    claims["eat_nonce"] = json!(["matches-nothing"]);
    let input = Input {
        token: fixture::sign(&claims),
        keys: vec![fixture::test_jwk()],
        now: NOW,
        binding: Binding::default(),
    };
    assert_eq!(verify(&input).unwrap_err(), Guest::Nonce);
}

#[tokio::test]
async fn the_roots_bytes_never_appear_in_debug_output_or_errors() {
    let root = Root::generate().unwrap();
    let secrets: Vec<String> = {
        let mut out = vec![hex::encode(root.expose())];
        for purpose in purposes() {
            out.push(hex::encode(
                root.key_pair(&purpose).unwrap().private_bytes().as_slice(),
            ));
        }
        out.push(hex::encode(root.signer().to_bytes()));
        out
    };
    let mut said = vec![format!("{root:?}"), format!("{root:#?}")];
    for purpose in purposes() {
        let pair = root.key_pair(&purpose).unwrap();
        said.push(format!("{pair:?}"));
        said.push(format!("{:?}", Received::Key(pair)));
    }
    let requester = Requester::new(Purpose::Root).unwrap();
    said.push(format!("{requester:?}"));
    let mut secrets = secrets;
    for kind in [ImageKind::Builder, ImageKind::Reviewer] {
        let seed = root.service_seed(kind, digest_of(kind), project());
        secrets.push(hex::encode(seed.as_slice()));
        said.push(format!("{:?}", Received::Seed(seed.clone())));
        said.push(format!("{:#?}", Received::Seed(seed)));
        let Received::Seed(given) = ask_as(&StandIn::new(), &root, kind, &Purpose::Service, None)
            .await
            .unwrap()
        else {
            panic!("{kind} is given its seed");
        };
        said.push(format!("{:?}", Received::Seed(given)));
    }

    let mut chain = StandIn::new();
    let errors = [
        ask_as(&chain, &root, ImageKind::Builder, &Purpose::Root, None).await,
        ask_as(&chain, &root, ImageKind::Executor, &venue(), None).await,
        reviewer_with(&chain, |c| c["dbgstat"] = json!("enabled")).await,
    ];
    chain.down = true;
    let more = [ask_as(&chain, &root, ImageKind::Holder, &Purpose::Root, None).await];
    for result in errors.into_iter().chain(more) {
        let error = result.unwrap_err();
        said.push(error.to_string());
        said.push(format!("{error:?}"));
    }
    let Received::Root(passed) = ask_as(
        &StandIn::new(),
        &root,
        ImageKind::Holder,
        &Purpose::Root,
        None,
    )
    .await
    .unwrap() else {
        panic!("a holder is given the root");
    };
    said.push(format!("{:?}", Received::Root(passed)));

    for text in said {
        let lower = text.to_lowercase();
        for secret in &secrets {
            assert!(!lower.contains(secret), "a secret in {text}");
        }
        // Nor as a list of bytes.
        assert!(!text.contains(&format!("{:?}", root.expose())));
    }
}

#[tokio::test]
async fn the_routes_publish_release_and_say_whether_the_root_is_held() {
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let chain = StandIn::new();
    let service = crate::Service::new(chain.clone());
    service.set_google_keys(google_keys());
    let app = crate::routes(service.clone());
    let get = |uri: &str| {
        axum::http::Request::get(uri.to_string())
            .body(Body::empty())
            .unwrap()
    };

    let answer = app.clone().oneshot(get("/rootkey")).await.unwrap();
    let body = answer.into_body().collect().await.unwrap().to_bytes();
    let status: crate::http::RootStatus = serde_json::from_slice(&body).unwrap();
    assert!(!status.holds_root && status.signer.is_none());
    let answer = app
        .clone()
        .oneshot(get("/rootkey/published/platform:dummy"))
        .await
        .unwrap();
    assert_eq!(answer.status(), 503);

    let root = Root::generate().unwrap();
    let signer = root.signer_address();
    service.hold(root);
    chain.record_root(Some(signer));
    // Held, and not yet recorded by the chain: nothing derived is served.
    let answer = app.clone().oneshot(get("/rootkey")).await.unwrap();
    let body = answer.into_body().collect().await.unwrap().to_bytes();
    let status: crate::http::RootStatus = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        status,
        crate::http::RootStatus {
            chain_id: CANONICAL,
            holds_root: true,
            signer: Some(signer),
            serving: false,
        }
    );
    let answer = app
        .clone()
        .oneshot(get("/rootkey/published/platform:dummy"))
        .await
        .unwrap();
    assert_eq!(answer.status(), 503);

    service.set_serving(true);
    let answer = app.clone().oneshot(get("/rootkey")).await.unwrap();
    let body = answer.into_body().collect().await.unwrap().to_bytes();
    let status: crate::http::RootStatus = serde_json::from_slice(&body).unwrap();
    assert!(status.serving);

    let purpose = Purpose::platform(DUMMY).unwrap();
    let answer = app
        .clone()
        .oneshot(get("/rootkey/published/platform:dummy"))
        .await
        .unwrap();
    assert_eq!(answer.status(), 200);
    let body = answer.into_body().collect().await.unwrap().to_bytes();
    let published: PublishedKey = serde_json::from_slice(&body).unwrap();
    published.verify(signer, CANONICAL, &purpose).unwrap();
    for bad in ["/rootkey/published/root", "/rootkey/published/nonsense"] {
        assert_eq!(app.clone().oneshot(get(bad)).await.unwrap().status(), 400);
    }

    // The exchange, with the holder's own clock: a token issued now.
    let requester = Requester::new(purpose.clone()).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut claims = fixture::claims(&Binding::default(), now - 5);
    claims["eat_nonce"] = json!([requester.nonce()]);
    claims["submods"]["container"]["image_digest"] =
        json!(image_claim(digest_of(ImageKind::Reviewer)));
    let request = requester.request(fixture::sign(&claims), None);
    let post = |request: &crate::ExchangeRequest| {
        axum::http::Request::post("/rootkey/exchange")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(request).unwrap()))
            .unwrap()
    };
    let answer = app.clone().oneshot(post(&request)).await.unwrap();
    assert_eq!(answer.status(), 200);
    let body = answer.into_body().collect().await.unwrap().to_bytes();
    let release: crate::Release = serde_json::from_slice(&body).unwrap();
    let Received::Key(pair) = requester.open(&request, &release).unwrap() else {
        panic!("a key is released");
    };
    let sealed = seal(&published, b"the dummy secret", b"").unwrap();
    assert_eq!(
        open(&pair, &sealed, b"").unwrap().as_slice(),
        b"the dummy secret"
    );

    // Refused, with the reason and nothing else.
    let mut refused = request.clone();
    refused.purpose = Purpose::Root;
    let answer = app.clone().oneshot(post(&refused)).await.unwrap();
    assert_eq!(answer.status(), 403);
    let body = answer.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        String::from_utf8(body.to_vec()).unwrap(),
        Error::Nonce.to_string()
    );

    // The record stops being live: the same request releases nothing.
    service.set_serving(false);
    assert_eq!(
        app.clone().oneshot(post(&request)).await.unwrap().status(),
        503
    );
    assert_eq!(
        app.clone()
            .oneshot(get("/rootkey/published/platform:dummy"))
            .await
            .unwrap()
            .status(),
        503
    );
    // And a root let go is neither held nor served, whatever was said.
    service.set_serving(true);
    service.forget();
    service.set_serving(true);
    assert!(!service.holds_root() && !service.is_serving());
}

/// A token signed by a Google key the holder hasn't fetched wakes the
/// holder's fetch of Google's keys.
#[tokio::test]
async fn a_token_under_an_unknown_google_key_asks_for_the_keys_again() {
    let chain = StandIn::new();
    let service = crate::Service::new(chain.clone());
    let root = Root::for_tests(7);
    chain.record_root(Some(root.signer_address()));
    service.hold(root);
    service.set_serving(true);
    // No keys fetched at all: every key id is unknown.
    let purpose = Purpose::platform(DUMMY).unwrap();
    let requester = Requester::new(purpose).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut claims = fixture::claims(&Binding::default(), now - 5);
    claims["eat_nonce"] = json!([requester.nonce()]);
    let request = requester.request(fixture::sign(&claims), None);
    let woken = service.clone();
    let waiting = tokio::spawn(async move { woken.keys_wanted().await });
    tokio::task::yield_now().await;
    assert!(matches!(
        service.exchange(&request).await,
        Err(Error::Token(_))
    ));
    tokio::time::timeout(std::time::Duration::from_secs(5), waiting)
        .await
        .expect("the holder was not asked to fetch the keys")
        .unwrap();
}

/// The holder may not have looked at the record since it was cleared or
/// revoked: every request reads it again, and the next one is refused.
#[tokio::test]
async fn a_cleared_or_revoked_root_releases_nothing_from_the_next_request() {
    let chain = StandIn::new();
    let service = crate::Service::new(chain.clone());
    service.set_google_keys(google_keys());
    let root = Root::for_tests(7);
    let signer = root.signer_address();
    service.hold(root);
    service.set_serving(true);
    chain.record_root(Some(signer));
    let purpose = Purpose::platform(DUMMY).unwrap();
    let request = |purpose: &Purpose| {
        let requester = Requester::new(purpose.clone()).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut claims = fixture::claims(&Binding::default(), now - 5);
        claims["eat_nonce"] = json!([requester.nonce()]);
        claims["submods"]["container"]["image_digest"] =
            json!(image_claim(digest_of(ImageKind::Reviewer)));
        requester.request(fixture::sign(&claims), None)
    };
    assert!(service.published(&purpose).await.is_ok());
    assert!(service.exchange(&request(&purpose)).await.is_ok());

    // Revoked (the record is no longer live), or cleared, or another
    // signer recorded since: the holder still says serve, and nothing is.
    for record in [None, Some(Address::repeat_byte(0x0e))] {
        chain.record_root(record);
        assert!(service.is_serving());
        assert_eq!(service.published(&purpose).await, Err(Error::NotServing));
        assert_eq!(
            service.exchange(&request(&purpose)).await.err(),
            Some(Error::NotServing)
        );
    }

    // A chain that can't be read decides nothing.
    let mut down = chain.clone();
    down.down = true;
    let service = crate::Service::new(down);
    service.hold(Root::for_tests(7));
    service.set_serving(true);
    chain.record_root(Some(signer));
    assert_eq!(
        service.published(&purpose).await,
        Err(Error::ChainUnavailable)
    );

    // Recorded and live again: served again.
    let service = crate::Service::new(chain.clone());
    service.set_google_keys(google_keys());
    service.hold(Root::for_tests(7));
    service.set_serving(true);
    assert!(service.published(&purpose).await.is_ok());
}

#[test]
fn the_root_leaf_is_the_registrys() {
    use alloy::sol_types::SolValue;
    let signer = Address::repeat_byte(0x42);
    let purpose = alloy::primitives::keccak256("shonin:root-key:v1");
    let expected = alloy::primitives::keccak256(alloy::primitives::keccak256(
        (
            alloy::primitives::U256::ZERO,
            Address::ZERO,
            purpose,
            alloy::primitives::U256::from(CANONICAL),
            signer,
        )
            .abi_encode(),
    ));
    assert_eq!(crate::root_leaf(CANONICAL, signer), expected);
}

/// The routes mount over the real registry as well as the stand-in: the
/// futures it makes are `Send`.
#[allow(dead_code)]
fn the_routes_mount_over_the_registry(
    service: crate::Service<crate::RegistryView>,
) -> axum::Router {
    crate::routes(service)
}

/// The canonical chain's node, as far as the root key's record goes: it
/// answers `isRootKeyLive()` and `rootKey()` from what a test sets, and
/// counts how often each was asked.
#[derive(Default)]
struct RecordNode {
    live: std::sync::atomic::AtomicBool,
    signer: std::sync::Mutex<Address>,
    asked_live: std::sync::atomic::AtomicUsize,
    asked_record: std::sync::atomic::AtomicUsize,
}

async fn record_node(node: std::sync::Arc<RecordNode>) -> String {
    use alloy::sol_types::SolCall;
    use chain::bindings::attestation::{IAttestationRegistry, RootKeyRecord};
    use std::sync::atomic::Ordering;
    let app = axum::Router::new().route(
        "/",
        axum::routing::post(move |axum::Json(request): axum::Json<Value>| {
            let node = node.clone();
            async move {
                let id = request["id"].clone();
                let answer = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
                let refuse = || {
                    json!({"jsonrpc": "2.0", "id": request["id"].clone(),
                        "error": {"code": -32601, "message": "not answered here"}})
                };
                axum::Json(match request["method"].as_str().unwrap_or_default() {
                    "eth_chainId" => answer(json!(format!("0x{CANONICAL:x}"))),
                    "eth_blockNumber" => answer(json!("0x10")),
                    "eth_call" => {
                        let call = &request["params"][0];
                        let data = call["input"]
                            .as_str()
                            .or_else(|| call["data"].as_str())
                            .unwrap_or_default();
                        let input = alloy::hex::decode(data).unwrap_or_default();
                        let selector: [u8; 4] = input
                            .get(..4)
                            .and_then(|head| head.try_into().ok())
                            .unwrap_or_default();
                        let out = match selector {
                            IAttestationRegistry::isRootKeyLiveCall::SELECTOR => {
                                node.asked_live.fetch_add(1, Ordering::SeqCst);
                                IAttestationRegistry::isRootKeyLiveCall::abi_encode_returns(
                                    &node.live.load(Ordering::SeqCst),
                                )
                            }
                            IAttestationRegistry::rootKeyCall::SELECTOR => {
                                node.asked_record.fetch_add(1, Ordering::SeqCst);
                                IAttestationRegistry::rootKeyCall::abi_encode_returns(
                                    &RootKeyRecord {
                                        signer: *node.signer.lock().unwrap(),
                                        issuedAt: NOW,
                                        verifier: Address::repeat_byte(0xcc),
                                        setAt: NOW,
                                        imageDigest: digest_of(ImageKind::Executor),
                                        projectId: B256::repeat_byte(0x9a),
                                    },
                                )
                            }
                            _ => return axum::Json(refuse()),
                        };
                        answer(json!(alloy::hex::encode_prefixed(out)))
                    }
                    _ => refuse(),
                })
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    url
}

/// Read through the registry reader the holder uses, every published key
/// and every exchange asks the chain for the record's liveness and signer
/// again: nothing is kept from one request to the next, so a root revoked or
/// cleared on the chain releases nothing from the very next request, with
/// no look by the holder in between.
#[tokio::test]
async fn every_request_reads_the_root_key_s_liveness_from_the_chain_uncached() {
    use std::sync::atomic::Ordering;
    let node = std::sync::Arc::new(RecordNode::default());
    let url = record_node(node.clone()).await;
    let config = chain::ChainConfig {
        label: "canonical".to_string(),
        rpc: url.clone(),
        ws: None,
        dollar: "USDC".to_string(),
        native_usd: None,
        public_rpc: url,
        logs_rpc: None,
        log_range: None,
        confirmations: None,
    };
    let client = chain::Client::connect(&config, chain::Signing::ReadOnly)
        .await
        .unwrap();
    let view = crate::RegistryView::new(client, Address::repeat_byte(0xa7), CANONICAL);
    let service = crate::Service::new(view);
    service.set_google_keys(google_keys());
    let root = Root::for_tests(7);
    *node.signer.lock().unwrap() = root.signer_address();
    node.live.store(true, Ordering::SeqCst);
    service.hold(root);
    service.set_serving(true);
    let purpose = Purpose::platform(DUMMY).unwrap();

    for asked in 1..=3 {
        assert!(service.published(&purpose).await.is_ok());
        assert_eq!(node.asked_live.load(Ordering::SeqCst), asked);
        assert_eq!(node.asked_record.load(Ordering::SeqCst), asked);
    }

    // Revoked on the chain: the holder still says serve, and the next
    // request is refused, published key and exchange alike.
    node.live.store(false, Ordering::SeqCst);
    assert!(service.is_serving());
    assert_eq!(service.published(&purpose).await, Err(Error::NotServing));
    assert_eq!(node.asked_live.load(Ordering::SeqCst), 4);
    let requester = Requester::new(purpose.clone()).unwrap();
    let mut claims = fixture::claims(&Binding::default(), NOW - 5);
    claims["eat_nonce"] = json!([requester.nonce()]);
    let request = requester.request(fixture::sign(&claims), None);
    assert_eq!(
        service.exchange_at(&request, NOW).await.err(),
        Some(Error::NotServing)
    );
    assert_eq!(node.asked_live.load(Ordering::SeqCst), 5);

    // Live again but naming another signer (cleared and remade elsewhere):
    // refused too, read afresh.
    node.live.store(true, Ordering::SeqCst);
    *node.signer.lock().unwrap() = Address::repeat_byte(0x0e);
    assert_eq!(service.published(&purpose).await, Err(Error::NotServing));
    assert_eq!(node.asked_live.load(Ordering::SeqCst), 6);
    assert_eq!(node.asked_record.load(Ordering::SeqCst), 4);
}

/// Only the platform secrets an image may ever be given have their keys
/// published: a key for any other name would have something sealed to it
/// that nobody can ever open.
#[tokio::test]
async fn only_the_platform_secrets_an_image_may_be_given_are_published() {
    let chain = StandIn::new();
    let service = crate::Service::new(chain.clone());
    let root = Root::for_tests(7);
    chain.record_root(Some(root.signer_address()));
    service.hold(root);
    service.set_serving(true);
    for name in [REVIEWER_MODEL, DUMMY] {
        let purpose = Purpose::platform(name).unwrap();
        assert!(service.published(&purpose).await.is_ok(), "{name}");
    }
    let unknown = Purpose::platform("anything-else").unwrap();
    assert_eq!(
        service.published(&unknown).await,
        Err(Error::UnknownPlatformSecret(unknown.clone()))
    );
    let app = crate::routes(service.clone());
    let answer = tower::ServiceExt::oneshot(
        app,
        axum::http::Request::get("/rootkey/published/platform:anything-else")
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(answer.status(), 404);
    // Every other kind of purpose is published as before.
    assert!(service.published(&Purpose::Builder).await.is_ok());
}

/// A service's seed is its own: another image, project or kind (or another
/// root) derives another, and the same four always derive the same one.
#[test]
fn a_service_seed_differs_by_image_project_and_kind() {
    let root = Root::for_tests(7);
    let image = B256::repeat_byte(0x11);
    let project = B256::repeat_byte(0x22);
    let seed = root.service_seed(ImageKind::Builder, image, project);
    assert_eq!(
        *seed,
        *Root::for_tests(7).service_seed(ImageKind::Builder, image, project)
    );
    let others = [
        root.service_seed(ImageKind::Reviewer, image, project),
        root.service_seed(ImageKind::Builder, B256::repeat_byte(0x12), project),
        root.service_seed(ImageKind::Builder, image, B256::repeat_byte(0x23)),
        Root::for_tests(8).service_seed(ImageKind::Builder, image, project),
    ];
    let mut seen = HashSet::from([*seed]);
    for other in others {
        assert!(seen.insert(*other), "two services share a seed");
    }
    // Nor is it the root, the root signer or any purpose's key.
    assert_ne!(*seed, *root.expose());
    assert_ne!(seed.as_slice(), root.signer().to_bytes().as_slice());
    for purpose in purposes() {
        let pair = root.key_pair(&purpose).unwrap();
        assert_ne!(seed.as_slice(), pair.private_bytes().as_slice());
    }
}

#[test]
fn an_image_kind_s_byte_is_the_registry_s_value() {
    use chain::bindings::attestation::ImageKind as Chain;
    for (kind, chain) in [
        (ImageKind::Executor, Chain::Executor),
        (ImageKind::Unwinder, Chain::Unwinder),
        (ImageKind::Builder, Chain::Builder),
        (ImageKind::Reviewer, Chain::Reviewer),
        (ImageKind::Holder, Chain::Holder),
    ] {
        assert_eq!(kind.to_byte(), chain as u8, "{kind}");
    }
}

#[tokio::test]
async fn a_service_seed_is_never_given_to_an_executor_holder_or_unwinder() {
    let root = Root::for_tests(7);
    let chain = StandIn::new();
    for kind in [ImageKind::Executor, ImageKind::Holder, ImageKind::Unwinder] {
        assert_eq!(permission(kind, &Purpose::Service), Permission::Never);
        assert_eq!(
            ask_as(&chain, &root, kind, &Purpose::Service, None)
                .await
                .unwrap_err(),
            Error::NotForKind {
                kind,
                purpose: Purpose::Service
            }
        );
    }
}

/// The holder derives the seed from what the token attests, so a builder
/// is given its own image's seed, the reviewer its own.
#[tokio::test]
async fn a_service_is_given_the_seed_for_its_attested_image_project_and_kind() {
    let root = Root::for_tests(7);
    let chain = StandIn::new();
    for kind in [ImageKind::Builder, ImageKind::Reviewer] {
        let Received::Seed(seed) = ask_as(&chain, &root, kind, &Purpose::Service, None)
            .await
            .unwrap()
        else {
            panic!("{kind} is given a seed");
        };
        assert_eq!(*seed, *root.service_seed(kind, digest_of(kind), project()));
    }
    // Under another allowed project, another seed.
    let mut chain = StandIn::new();
    let elsewhere = "another-project";
    chain
        .projects
        .insert(B256::from(shonin_attestation_core::project_id(elsewhere)));
    let requester = Requester::new(Purpose::Service).unwrap();
    let token = token(&requester.nonce(), ImageKind::Builder, |claims| {
        claims["submods"]["gce"]["project_id"] = json!(elsewhere);
    });
    let Received::Seed(seed) = ask(&chain, &root, &requester, token, None).await.unwrap() else {
        panic!("a builder is given a seed");
    };
    assert_ne!(
        *seed,
        *root.service_seed(ImageKind::Builder, digest_of(ImageKind::Builder), project())
    );
}

/// The reviewer is given the builder key, so it can open sealed source it
/// reviews.
#[tokio::test]
async fn a_reviewer_is_given_the_builder_key() {
    let root = Root::for_tests(7);
    let chain = StandIn::new();
    let published = root.publish(&Purpose::Builder, CANONICAL).unwrap();
    let sealed = seal(&published, b"sealed source", b"").unwrap();
    let Received::Key(pair) = ask_as(&chain, &root, ImageKind::Reviewer, &Purpose::Builder, None)
        .await
        .unwrap()
    else {
        panic!("a reviewer asking for the builder key is given it");
    };
    assert_eq!(
        open(&pair, &sealed, b"").unwrap().as_slice(),
        b"sealed source"
    );
}

/// A seed's release opens only with the ephemeral key its token binds:
/// another requester, even one asking for the same purpose, gets nothing.
#[tokio::test]
async fn a_service_seed_opens_only_for_its_ephemeral_key() {
    let root = Root::for_tests(7);
    let chain = StandIn::new();
    let requester = Requester::new(Purpose::Service).unwrap();
    let token = token(&requester.nonce(), ImageKind::Builder, |_| {});
    let request = requester.request(token, None);
    let release = authorise(&root, &google_keys(), &request, NOW, &chain)
        .await
        .unwrap();
    assert!(matches!(
        requester.open(&request, &release),
        Ok(Received::Seed(_))
    ));

    let thief = Requester::new(Purpose::Service).unwrap();
    assert_eq!(
        thief.open(&request, &release).unwrap_err(),
        Error::Open,
        "another ephemeral key opened the seed"
    );
    // Its own request relabelled with the stolen release's token and key.
    let mut relabelled = thief.request(request.token.clone(), None);
    relabelled.ephemeral_public = request.ephemeral_public;
    assert_eq!(thief.open(&relabelled, &release).unwrap_err(), Error::Open);
    let forged = thief.request(request.token.clone(), None);
    assert_eq!(thief.open(&forged, &release).unwrap_err(), Error::Open);
}

/// A service's seed has no key pair: nothing is published for it, and
/// nothing can be sealed to it.
#[tokio::test]
async fn a_service_seed_has_no_published_key() {
    let chain = StandIn::new();
    let service = crate::Service::new(chain.clone());
    let root = Root::for_tests(7);
    chain.record_root(Some(root.signer_address()));
    let builder = root.publish(&Purpose::Builder, CANONICAL).unwrap();
    service.hold(root);
    service.set_serving(true);
    assert_eq!(
        service.published(&Purpose::Service).await,
        Err(Error::NoKeyPair(Purpose::Service))
    );
    let app = crate::routes(service.clone());
    let answer = tower::ServiceExt::oneshot(
        app,
        axum::http::Request::get("/rootkey/published/service")
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(answer.status(), 404);
    let relabelled = PublishedKey {
        purpose: Purpose::Service,
        ..builder
    };
    assert_eq!(
        seal(&relabelled, b"x", b"").unwrap_err(),
        Error::NoKeyPair(Purpose::Service)
    );
}
