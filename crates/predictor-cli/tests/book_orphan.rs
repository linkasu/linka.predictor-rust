use predictor_cli::pipeline::{atomic_json, host_guard, load_config, Lease};
use serde_json::json;
use std::{
    fs,
    io::{Seek, Write},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
#[ignore = "Actual tiny KenLM training: run explicitly on authorized book.local only"]
fn orphan_trainer_retains_heavy_lease_and_durable_receipt() {
    let config_path = std::env::var("LP_BOOK_TEST_CONFIG").unwrap();
    let config = load_config(std::path::Path::new(&config_path)).unwrap();
    host_guard(&config).unwrap();
    assert!(
        config.tiny_discount_fallback,
        "fault test requires an explicit tiny configuration"
    );
    assert!(
        fs::metadata(&config.source).unwrap().len() <= 2_000_000,
        "never run this fault test with the full baseline source"
    );
    let root = std::path::PathBuf::from(std::env::var("LP_BOOK_FAULT_DIR").unwrap());
    fs::create_dir(&root).unwrap();
    let pipeline = root.join("pipeline");
    let mut parent = Command::new(env!("CARGO_BIN_EXE_lp"))
        .arg("run")
        .arg(&config_path)
        .arg(&pipeline)
        .stdout(Stdio::from(
            fs::File::create(root.join("parent.stdout")).unwrap(),
        ))
        .stderr(Stdio::from(
            fs::File::create(root.join("parent.stderr")).unwrap(),
        ))
        .spawn()
        .unwrap();
    let start = Instant::now();
    let out = loop {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "trainer never started"
        );
        assert!(parent.try_wait().unwrap().is_none());
        if let Ok(file) = fs::File::open(pipeline.join("manifest.json")) {
            let manifest: serde_json::Value = serde_json::from_reader(file).unwrap();
            if manifest["active_stage"] == "train" {
                let out = std::path::PathBuf::from(manifest["active_directory"].as_str().unwrap());
                if out.join("trainer.json").exists() {
                    break out;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let trainer: serde_json::Value =
        serde_json::from_reader(fs::File::open(out.join("trainer.json")).unwrap()).unwrap();
    let trainer_pid = trainer["pid"].as_u64().unwrap();
    struct ContinueOnDrop(i32);
    impl Drop for ContinueOnDrop {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.0, libc::SIGCONT);
            }
        }
    }
    assert_eq!(unsafe { libc::kill(trainer_pid as i32, libc::SIGSTOP) }, 0);
    let resume_guard = ContinueOnDrop(trainer_pid as i32);
    let lsof = Command::new("/usr/sbin/lsof")
        .args(["-p", &trainer_pid.to_string()])
        .output()
        .unwrap();
    fs::write(root.join("trainer-open-files.txt"), &lsof.stdout).unwrap();
    assert!(
        String::from_utf8_lossy(&lsof.stdout).contains("heavy.lock"),
        "actual KenLM child did not inherit lease FD"
    );
    parent.kill().unwrap();
    let killed = parent.wait().unwrap();
    assert!(
        Lease::claim(&config.book_root.join("runs/heavy.lock")).is_err(),
        "lease released while orphan trainer still active"
    );
    let second = Command::new(env!("CARGO_BIN_EXE_lp"))
        .arg("train")
        .arg(&config_path)
        .arg(out.join("model.arpa.part"))
        .arg(root.join("second-refused"))
        .output()
        .unwrap();
    fs::write(root.join("second.stderr"), &second.stderr).unwrap();
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("active lease"));
    drop(resume_guard);
    while !out.join("model.arpa").exists() {
        assert!(
            start.elapsed() < Duration::from_secs(90),
            "orphan result not reconciled"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    while Lease::claim(&config.book_root.join("runs/heavy.lock")).is_err() {
        assert!(start.elapsed() < Duration::from_secs(90));
        std::thread::sleep(Duration::from_millis(5));
    }
    let receipt: serde_json::Value =
        serde_json::from_reader(fs::File::open(out.join("exit.json")).unwrap()).unwrap();
    assert_eq!(receipt["state"], "completed");
    let invoke = |verb: &str, directory: &std::path::Path, log: &str| {
        let result = Command::new(env!("CARGO_BIN_EXE_lp"))
            .arg(verb)
            .arg(&config_path)
            .arg(directory)
            .output()
            .unwrap();
        fs::write(root.join(format!("{log}.stdout")), &result.stdout).unwrap();
        fs::write(root.join(format!("{log}.stderr")), &result.stderr).unwrap();
        assert!(
            result.status.success(),
            "{log}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_reader::<_, serde_json::Value>(
            fs::File::open(directory.join("manifest.json")).unwrap(),
        )
        .unwrap()
    };
    let resumed = invoke("resume", &pipeline, "orphan-resume");
    assert_eq!(resumed["state"], "evaluated_dev");
    assert_eq!(
        resumed["stages"]["train"]["directory"],
        out.to_str().unwrap()
    );
    let model =
        std::path::PathBuf::from(resumed["stages"]["compile"]["directory"].as_str().unwrap())
            .join("model.lpk");
    let original_hash = predictor_cli::pipeline::hash_file(&model).unwrap();
    let mut file = fs::OpenOptions::new().write(true).open(&model).unwrap();
    file.seek(std::io::SeekFrom::Start(0)).unwrap();
    file.write_all(&[0]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let repaired = invoke("resume", &pipeline, "corruption-resume");
    assert_eq!(
        repaired["stages"]["train"]["directory"],
        out.to_str().unwrap()
    );
    let repaired_model =
        std::path::PathBuf::from(repaired["stages"]["compile"]["directory"].as_str().unwrap())
            .join("model.lpk");
    assert_eq!(
        predictor_cli::pipeline::hash_file(&repaired_model).unwrap(),
        original_hash
    );
    let fresh = invoke("run", &root.join("fresh-cached"), "fresh-cached");
    let fresh_model =
        std::path::PathBuf::from(fresh["stages"]["compile"]["directory"].as_str().unwrap())
            .join("model.lpk");
    assert_eq!(
        predictor_cli::pipeline::hash_file(&fresh_model).unwrap(),
        original_hash
    );
    atomic_json(&root.join("observation.json"),&json!({"parent_pid":parent.id(),"parent_status":killed.to_string(),"trainer":trainer,"actual_child_fd_inherited":true,"second_trainer_refused":true,"orphan_receipt":receipt,"orphan_result_adopted":true,"corrupt_compile_rebuilt_without_retrain":true,"fresh_cached_model_sha256":original_hash})).unwrap();
}
