use predictor_cli::pipeline::{atomic_json, hash_file, verified_checkpoint, Stage};
use std::collections::BTreeMap;

#[test]
fn public_checkout_disables_private_host_operations_without_writes() {
    let (temp, config) = admission_fixture();
    let before = tree_snapshot(temp.path());
    let error = predictor_cli::pipeline::host_guard(&config).unwrap_err();
    assert!(error
        .to_string()
        .contains("not available in the public checkout"));
    assert_eq!(before, tree_snapshot(temp.path()));
}

#[test]
fn checkpoint_requires_exact_identity_and_uncorrupted_outputs() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("output");
    std::fs::write(&path, "complete").unwrap();
    let stage = Stage {
        identity: "config-code-tool-input".into(),
        directory: temp.path().into(),
        outputs: BTreeMap::from([("output".into(), hash_file(&path).unwrap())]),
    };
    assert!(verified_checkpoint(&stage, "config-code-tool-input").unwrap());
    assert!(!verified_checkpoint(&stage, "different-tool").unwrap());
    std::fs::write(&path, "corrupt").unwrap();
    assert!(!verified_checkpoint(&stage, "config-code-tool-input").unwrap());
    atomic_json(
        &temp.path().join("state.json"),
        &serde_json::json!({"state":"failed"}),
    )
    .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(temp.path().join("state.json")).unwrap()
        )
        .unwrap()["state"],
        "failed"
    );
}

#[test]
fn unknown_train_receipt_preserves_tracking_even_when_lease_is_free() {
    use predictor_cli::pipeline::{identity, run, Manifest};
    let (temp, config) = admission_fixture();
    let base = temp.path();
    std::fs::create_dir(base.join("run")).unwrap();
    std::fs::create_dir(base.join("old-trainer")).unwrap();
    for prior_identity in [identity(&config).unwrap(), "changed-input-identity".into()] {
        let manifest = Manifest {
            identity: prior_identity,
            state: "running".into(),
            active_stage: Some("train".into()),
            active_directory: Some(base.join("old-trainer")),
            ..Default::default()
        };
        atomic_json(&base.join("run/manifest.json"), &manifest).unwrap();
        let error = run(&base.join("config.json"), &base.join("run"), true).unwrap_err();
        assert!(error.to_string().contains("reconcile_required"), "{error}");
        let saved: Manifest =
            serde_json::from_reader(std::fs::File::open(base.join("run/manifest.json")).unwrap())
                .unwrap();
        assert_eq!(saved.state, "reconcile_required");
        assert_eq!(saved.active_stage.as_deref(), Some("train"));
        assert_eq!(saved.active_directory, manifest.active_directory);
        assert_eq!(saved.identity, manifest.identity);
        assert!(saved.stages.is_empty());
        assert!(!base.join("run/resolved-config.json").exists());
        assert!(!std::fs::read_dir(base.join("run"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("train-")));
    }
}

fn admission_fixture() -> (tempfile::TempDir, predictor_cli::pipeline::Config) {
    use predictor_cli::pipeline::Config;
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path();
    for dir in ["crates", "spec", "runs", "provenance"] {
        std::fs::create_dir(base.join(dir)).unwrap();
    }
    for name in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "holdout"] {
        std::fs::write(base.join(name), "").unwrap();
    }
    std::fs::write(
        base.join("source.tsv"),
        "1\trus\tнужна вода\tfixture\t0\t0\n",
    )
    .unwrap();
    std::fs::write(
        base.join("tool"),
        "non-executable fixture; host guard prevents any trainer spawn",
    )
    .unwrap();
    let config = Config {
        source: base.join("source.tsv"),
        source_sha256: hash_file(&base.join("source.tsv")).unwrap(),
        holdout: base.join("holdout"),
        source_root: base.into(),
        provenance_root: base.join("provenance"),
        kenlm: base.join("tool"),
        kenlm_sha256: hash_file(&base.join("tool")).unwrap(),
        book_root: base.into(),
        host_uuid: "not-book".into(),
        tiny_discount_fallback: true,
        evaluation_limit: 1,
    };
    atomic_json(&base.join("config.json"), &config).unwrap();
    (temp, config)
}

fn tree_snapshot(root: &std::path::Path) -> BTreeMap<std::path::PathBuf, Option<Vec<u8>>> {
    fn visit(
        root: &std::path::Path,
        path: &std::path::Path,
        result: &mut BTreeMap<std::path::PathBuf, Option<Vec<u8>>>,
    ) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let key = path.strip_prefix(root).unwrap().to_path_buf();
            if entry.file_type().unwrap().is_dir() {
                result.insert(key, None);
                visit(root, &path, result);
            } else {
                result.insert(key, Some(std::fs::read(path).unwrap()));
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

#[test]
fn run_admission_resume_missing_root_leaves_it_absent() {
    let (temp, _) = admission_fixture();
    let root = temp.path().join("absent");
    assert!(predictor_cli::pipeline::run(&temp.path().join("config.json"), &root, true).is_err());
    assert!(!root.exists(), "resume must not initialize a new run");
}

#[test]
fn run_admission_resume_missing_or_invalid_manifest_changes_nothing() {
    use predictor_cli::pipeline::Manifest;
    let (temp, _) = admission_fixture();
    let base = serde_json::json!({"identity":"known","state":"configured","active_stage":null,"active_directory":null,"stages":{}});
    let mut invalids = vec![
        None,
        Some(b"{invalid".to_vec()),
        Some(serde_json::to_vec(&Manifest::default()).unwrap()),
    ];
    for change in [
        serde_json::json!({"state":"unrecognized"}),
        serde_json::json!({"state":"running"}),
        serde_json::json!({"active_stage":"train"}),
        serde_json::json!({"active_directory":""}),
        serde_json::json!({"state":"running","active_stage":"bad-stage","active_directory":"where"}),
        serde_json::json!({"active_stage":"train","active_directory":"where"}),
    ] {
        let mut invalid = base.clone();
        for (key, value) in change.as_object().unwrap() {
            invalid[key] = value.clone();
        }
        invalids.push(Some(serde_json::to_vec(&invalid).unwrap()));
    }
    for (index, manifest) in invalids.into_iter().enumerate() {
        let root = temp.path().join(format!("invalid-{index}"));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join("untracked-UNKNOWN")).unwrap();
        std::fs::write(
            root.join("untracked-UNKNOWN/partial"),
            b"do not ignore/replay",
        )
        .unwrap();
        std::fs::write(root.join("resolved-config.json"), b"unrelated user bytes").unwrap();
        if let Some(bytes) = manifest {
            std::fs::write(root.join("manifest.json"), bytes).unwrap();
        }
        let before = tree_snapshot(&root);
        assert!(
            predictor_cli::pipeline::run(&temp.path().join("config.json"), &root, true).is_err()
        );
        assert_eq!(
            tree_snapshot(&root),
            before,
            "invalid resume case {index} must not even create run.lock"
        );
    }
}

#[test]
fn run_admission_new_rejects_existing_empty_root() {
    let (temp, _) = admission_fixture();
    let root = temp.path().join("existing");
    std::fs::create_dir(&root).unwrap();
    assert!(predictor_cli::pipeline::run(&temp.path().join("config.json"), &root, false).is_err());
    assert!(tree_snapshot(&root).is_empty());
}

#[test]
fn run_admission_new_rejects_existing_nonempty_root_without_overwrite() {
    let (temp, _) = admission_fixture();
    let root = temp.path().join("existing");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("resolved-config.json"),
        b"unrelated configuration",
    )
    .unwrap();
    std::fs::write(root.join("partial"), b"UNKNOWN attempt bytes").unwrap();
    let before = tree_snapshot(&root);
    assert!(predictor_cli::pipeline::run(&temp.path().join("config.json"), &root, false).is_err());
    assert_eq!(tree_snapshot(&root), before);
}

#[test]
fn run_admission_rejects_symlink_roots_and_never_creates_missing_parents() {
    let (temp, _) = admission_fixture();
    let target = temp.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("resolved-config.json"), b"unrelated").unwrap();
    let before = tree_snapshot(&target);
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    for resume in [false, true] {
        assert!(
            predictor_cli::pipeline::run(&temp.path().join("config.json"), &link, resume).is_err()
        );
        assert_eq!(tree_snapshot(&target), before);
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
    }
    let dangling = temp.path().join("dangling");
    std::os::unix::fs::symlink(temp.path().join("missing-target"), &dangling).unwrap();
    assert!(
        predictor_cli::pipeline::run(&temp.path().join("config.json"), &dangling, false).is_err()
    );
    assert!(!temp.path().join("missing-target").exists());
    let parent = temp.path().join("missing-parent");
    assert!(predictor_cli::pipeline::run(
        &temp.path().join("config.json"),
        &parent.join("run"),
        false
    )
    .is_err());
    assert!(!parent.exists());
}

#[test]
fn run_admission_exclusively_claims_fresh_root_and_reads_genuine_resume() {
    use predictor_cli::pipeline::{admit_run, Manifest};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("new-run");
    let initializing = admit_run(&root, false).unwrap();
    assert!(initializing.identity.is_empty());
    assert!(tree_snapshot(&root).is_empty());
    assert!(admit_run(&root, true).is_err());
    assert!(tree_snapshot(&root).is_empty());
    let manifest = Manifest {
        identity: "known-run-identity".into(),
        state: "configured".into(),
        ..Default::default()
    };
    atomic_json(&root.join("manifest.json"), &manifest).unwrap();
    let before = tree_snapshot(&root);
    let resumed = admit_run(&root, true).unwrap();
    assert_eq!(resumed.identity, manifest.identity);
    assert_eq!(resumed.state, "configured");
    assert_eq!(tree_snapshot(&root), before);
    assert!(admit_run(&root, false).is_err());
    assert_eq!(tree_snapshot(&root), before);
}

#[test]
fn run_admission_rejects_nonregular_resume_manifest_before_lock_creation() {
    let (temp, _) = admission_fixture();
    let root = temp.path().join("bad-marker");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("manifest.json")).unwrap();
    let before = tree_snapshot(&root);
    assert!(predictor_cli::pipeline::run(&temp.path().join("config.json"), &root, true).is_err());
    assert_eq!(tree_snapshot(&root), before);
}

#[test]
fn terminal_classification_never_invents_cancellation_from_a_crash() {
    use predictor_cli::pipeline::terminal_state;
    assert_eq!(terminal_state(None, Some(libc::SIGSEGV), false), "failed");
    assert_eq!(terminal_state(None, Some(libc::SIGTERM), true), "cancelled");
    assert_eq!(terminal_state(Some(0), None, true), "completed");
    assert_eq!(terminal_state(None, None, true), "interrupted");
}

#[test]
fn reconciliation_requires_a_valid_receipt_and_keeps_terminal_outcomes_distinct() {
    use predictor_cli::pipeline::{reconcile_train, Lease, Manifest};
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("trainer");
    std::fs::create_dir(&directory).unwrap();
    let lease = temp.path().join("heavy.lock");
    let fresh = || Manifest {
        identity: "same".into(),
        state: "failed".into(),
        active_stage: Some("train".into()),
        active_directory: Some(directory.clone()),
        ..Default::default()
    };
    for (receipt, state) in [
        (
            serde_json::json!({"state":"failed","exit_code":2,"signal":null}),
            "failed",
        ),
        (
            serde_json::json!({"state":"cancelled","exit_code":null,"signal":15,"cancel_requested":true}),
            "cancelled",
        ),
        (
            serde_json::json!({"state":"running","exit_code":null,"signal":null}),
            "reconcile_required",
        ),
        (
            serde_json::json!({"state":"completed","exit_code":9,"signal":null}),
            "reconcile_required",
        ),
        (
            serde_json::json!({"state":"failed","exit_code":0,"signal":15}),
            "reconcile_required",
        ),
    ] {
        atomic_json(&directory.join("exit.json"), &receipt).unwrap();
        let mut manifest = fresh();
        assert!(reconcile_train(&mut manifest, "same", &lease).is_err());
        assert_eq!(manifest.state, state);
        assert_eq!(manifest.active_directory.as_ref(), Some(&directory));
        assert!(manifest.stages.is_empty());
    }
    atomic_json(
        &directory.join("exit.json"),
        &serde_json::json!({"state":"completed","exit_code":0,"signal":null}),
    )
    .unwrap();
    let mut incomplete = fresh();
    assert!(reconcile_train(&mut incomplete, "same", &lease).is_err());
    assert_eq!(incomplete.state, "reconcile_required");
    let arpa =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/tiny/model.arpa");
    std::fs::copy(arpa, directory.join("model.arpa")).unwrap();
    let held = Lease::claim(&lease).unwrap();
    let mut blocked = fresh();
    assert!(reconcile_train(&mut blocked, "same", &lease).is_err());
    assert!(blocked.stages.is_empty());
    drop(held);
    let mut adopted = fresh();
    reconcile_train(&mut adopted, "same", &lease).unwrap();
    assert!(
        verified_checkpoint(&adopted.stages["train"], &adopted.stages["train"].identity).unwrap()
    );
    let mut changed = fresh();
    reconcile_train(&mut changed, "changed", &lease).unwrap();
    assert!(changed.stages.is_empty());
    std::fs::write(directory.join("model.arpa"), "partial").unwrap();
    let mut corrupt = fresh();
    assert!(reconcile_train(&mut corrupt, "same", &lease).is_err());
    assert_eq!(corrupt.state, "reconcile_required");
    assert!(corrupt.stages.is_empty());
}
