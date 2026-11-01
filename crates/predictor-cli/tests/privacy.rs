use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

#[test]
fn replay_privacy_under_umask_022_is_process_isolated() {
    let temp = tempfile::tempdir().unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "privacy_child", "--nocapture"])
        .env("LP_PRIVATE_REPORT_FIXTURE", temp.path())
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
#[ignore = "Only launched in an isolated child; never changes parent process umask"]
fn privacy_child() {
    let root = std::path::PathBuf::from(std::env::var("LP_PRIVATE_REPORT_FIXTURE").unwrap());
    unsafe {
        libc::umask(0o022);
    }
    let model = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/tiny/runtime-v1.lpk");
    let events = root.join("events.jsonl");
    let general = root.join("general.txt");
    let row = serde_json::json!({"query":"личноесек","expected":"личноесекретноеслово","event":predictor_core::ConfirmedEvent{event_id:1,utterance_id:1,origin:predictor_core::EventOrigin::Confirmation,text:"личноесекретноеслово".into()}});
    fs::write(&events, format!("{row}\n")).unwrap();
    fs::write(&general, "я хочу чай\nя хочу пить\n").unwrap();
    use sha2::{Digest, Sha256};
    let hashes =
        ["я хочу чай", "я хочу пить"].map(|text| format!("{:x}", Sha256::digest(text.as_bytes())));
    let component = hashes.iter().min().unwrap();
    let group_map = general.with_extension("groups.tsv");
    fs::write(
        &group_map,
        format!("{}\t{component}\n{}\t{component}\n", hashes[0], hashes[1]),
    )
    .unwrap();
    let out = root.join("replay");
    predictor_cli::diagnostics::personal_replay(&model, &events, &general, &out).unwrap();
    let report = out.join("replay.json");
    let metadata: serde_json::Value =
        serde_json::from_reader(fs::File::open(&report).unwrap()).unwrap();
    assert_eq!(metadata["general_phrase_groups"], 1);
    assert_eq!(metadata["general_phrase_count"], 2);
    assert_eq!(metadata["general_grouping_mode"], "component_sidecar");
    assert_eq!(
        metadata["general_grouping_version"],
        "duplicate-original-components-v1"
    );
    assert_eq!(
        metadata["general_group_map_sha256"],
        predictor_cli::pipeline::hash_file(&group_map).unwrap()
    );
    assert_eq!(
        metadata["general_selection_protocol"],
        predictor_cli::evaluate::SELECTION_PROTOCOL
    );
    assert!(fs::read_to_string(&report).unwrap().contains("личноесек"));
    assert_eq!(
        fs::metadata(&report).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(out.join("personal.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    predictor_cli::pipeline::atomic_json(
        &root.join("public.json"),
        &serde_json::json!({"public":true}),
    )
    .unwrap();
    assert_eq!(
        fs::metadata(root.join("public.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
    struct InspectTemp<'a> {
        path: &'a Path,
        old: Vec<u8>,
    }
    impl serde::Serialize for InspectTemp<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let temporary = self
                .path
                .with_extension(format!("{}.private.part", std::process::id()));
            assert_eq!(
                fs::metadata(temporary).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(fs::read(self.path).unwrap(), self.old);
            serde::Serialize::serialize(
                &serde_json::json!({"query":"private replacement"}),
                serializer,
            )
        }
    }
    let old = fs::read(&report).unwrap();
    fs::set_permissions(&report, fs::Permissions::from_mode(0o644)).unwrap();
    predictor_cli::diagnostics::private_atomic_json(&report, &InspectTemp { path: &report, old })
        .unwrap();
    assert_eq!(
        fs::metadata(&report).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let stable = fs::read(&report).unwrap();
    let collision = report.with_extension(format!("{}.private.part", std::process::id()));
    fs::write(&collision, b"uncertain existing temporary").unwrap();
    assert!(predictor_cli::diagnostics::private_atomic_json(
        &report,
        &serde_json::json!({"query":"must not publish"})
    )
    .is_err());
    assert_eq!(fs::read(&report).unwrap(), stable);
    assert_eq!(
        fs::read(collision).unwrap(),
        b"uncertain existing temporary"
    );
    assert_eq!(
        fs::metadata(root.join("public.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
}
