use std::fs;
use std::path::Path;

fn definitions(prefix: &str) -> Vec<String> {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut found = Vec::new();
    for dir in [tests.clone(), tests.join("common")] {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("read tests entry").path();
            if path.extension().is_none_or(|ext| ext != "rs")
                || path
                    .file_name()
                    .is_some_and(|name| name == "helper_census.rs")
            {
                continue;
            }
            let relative = path
                .strip_prefix(&tests)
                .expect("path under tests")
                .display()
                .to_string();
            let text = fs::read_to_string(&path).expect("read test source");
            for (index, line) in text.lines().enumerate() {
                if line.starts_with(&format!("fn {prefix}"))
                    || line.starts_with(&format!("pub fn {prefix}"))
                {
                    found.push(format!("{relative}:{}", index + 1));
                }
            }
        }
    }
    found.sort();
    found
}

#[test]
fn cli_tests_hash_with_core_sha256_hex() {
    let defined = definitions("sha256_hex(");
    assert!(
        defined.is_empty(),
        "sha256_hex must come from anytopdf_core, but is defined at: {}",
        defined.join(", ")
    );
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    for name in ["manifest.rs", "profiles.rs"] {
        let text = fs::read_to_string(tests.join(name)).expect("read test source");
        for constant in ["0x6a09e667", "0x428a2f98"] {
            assert!(
                !text.contains(constant),
                "{name} holds SHA-256 constant {constant}"
            );
        }
        assert!(text.contains("sha256_hex("), "{name} must call sha256_hex(");
    }
}

#[test]
fn core_sha256_hex_matches_fips_180_4_vectors() {
    assert_eq!(
        anytopdf_core::sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        anytopdf_core::sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        anytopdf_core::sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
}

#[test]
fn png_fixture_helpers_are_defined_once_in_common() {
    let mut offending = Vec::new();
    for name in [
        "crc32(",
        "png_chunk(",
        "encode_png(",
        "write_png(",
        "write_gray_ramp_png(",
        "write_rgb_png(",
    ] {
        let defined = definitions(name);
        if defined.len() != 1 || !defined[0].starts_with("common/") {
            offending.push(format!("{name} -> [{}]", defined.join(", ")));
        }
    }
    let chunk = definitions("chunk(");
    if !chunk.is_empty() {
        offending.push(format!("chunk( -> [{}]", chunk.join(", ")));
    }
    assert!(
        offending.is_empty(),
        "PNG fixture helpers must be defined once under common/:\n{}",
        offending.join("\n")
    );
}

#[test]
fn schema_json_and_repo_helpers_are_defined_once_in_common() {
    let mut offending = Vec::new();
    for name in [
        "load_schema(",
        "validation_errors(",
        "assert_valid(",
        "single_document(",
        "repo_file(",
    ] {
        let defined = definitions(name);
        if defined.len() != 1 || !defined[0].starts_with("common/") {
            offending.push(format!("{name} -> [{}]", defined.join(", ")));
        }
    }
    assert!(
        offending.is_empty(),
        "schema, JSON and repo-file helpers must be defined once under common/:\n{}",
        offending.join("\n")
    );
}
