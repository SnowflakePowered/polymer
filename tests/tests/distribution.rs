use polymer_distribution::{Index, SignedArchive, TrustedComment};

fn archive(id: &str, version: &str) -> Vec<u8> {
    let mut writer = asar::AsarWriter::new();
    writer
        .write_file(
            "plugin.toml",
            format!("[plugin]\nid = \"{id}\"\nversion = \"{version}\"\nclass = \"scraper\"\napi = \"^0.1\"\nmain = \"main.js\"\n"),
            false,
        )
        .unwrap();
    writer.write_file("main.js", b"export {};", false).unwrap();
    let mut bytes = Vec::new();
    writer.finalize(&mut bytes).unwrap();
    bytes
}

fn sign(pair: &minisign::KeyPair, archive: &[u8], id: &str, version: &str) -> Vec<u8> {
    let comment = TrustedComment {
        id: id.to_owned(),
        version: semver::Version::parse(version).unwrap(),
    };
    let signature = minisign::sign(None, &pair.sk, archive, Some(&comment.to_string()), None)
        .unwrap()
        .into_string();
    SignedArchive::seal(archive, &signature)
}

fn key(pair: &minisign::KeyPair) -> minisign_verify::PublicKey {
    minisign_verify::PublicKey::from_base64(&pair.pk.to_base64()).unwrap()
}

fn read_index(entries: &[(&str, String)]) -> (tempfile::TempDir, Index) {
    let root = tempfile::tempdir().unwrap();
    for (repo, body) in entries {
        let path = root.path().join("repos").join(format!("{repo}.toml"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("repo = \"{repo}\"\n{body}")).unwrap();
    }
    let index = Index::read(root.path()).unwrap();
    (root, index)
}

#[test]
fn a_signed_archive_verifies_only_against_a_registered_key_and_its_own_contents() {
    let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let stranger = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let signed = sign(&pair, &archive("fixture", "1.0.0"), "fixture", "1.0.0");

    let header = SignedArchive::split(&signed)
        .unwrap()
        .verify(&[key(&stranger), key(&pair)])
        .unwrap();
    assert_eq!(header.version, semver::Version::new(1, 0, 0));
    assert!(asar::AsarReader::new(&signed, None).is_ok());

    let mut tampered = signed.clone();
    tampered[20] ^= 1;
    assert!(
        SignedArchive::split(&tampered)
            .unwrap()
            .verify(&[key(&pair)])
            .is_err()
    );
    assert!(
        SignedArchive::split(&signed)
            .unwrap()
            .verify(&[key(&stranger)])
            .is_err()
    );
    assert!(SignedArchive::split(&archive("fixture", "1.0.0")).is_err());
}

#[test]
fn a_signature_for_another_version_does_not_verify() {
    let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let signed = sign(&pair, &archive("fixture", "2.0.0"), "fixture", "1.0.0");
    assert!(
        SignedArchive::split(&signed)
            .unwrap()
            .verify(&[key(&pair)])
            .is_err()
    );
}

#[test]
fn only_official_repositories_sign_bare_ids_and_scopes_trust_their_single_claimant() {
    let official = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let author = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let squatter = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let entry = |pair: &minisign::KeyPair, rest: &str| {
        format!("keys = [\"{}\"]\n{rest}", pair.pk.to_base64())
    };
    let (_root, index) = read_index(&[
        ("polymer/plugins", entry(&official, "official = true")),
        ("author/plugins", entry(&author, "scopes = [\"author\"]")),
    ]);
    let signed = sign(
        &author,
        &archive("@author/fixture", "1.0.0"),
        "@author/fixture",
        "1.0.0",
    );
    let archive = SignedArchive::split(&signed).unwrap();
    assert!(archive.verify(&index.keys("@author/fixture")).is_ok());
    assert!(archive.verify(&index.keys("fixture")).is_err());
    assert!(index.keys("@other/fixture").is_empty());

    let (_root, contested) = read_index(&[
        ("author/plugins", entry(&author, "scopes = [\"author\"]")),
        (
            "squatter/plugins",
            entry(&squatter, "scopes = [\"author\"]"),
        ),
    ]);
    assert!(contested.keys("@author/fixture").is_empty());
}
