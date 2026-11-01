#![cfg(unix)]

use predictor_core::PersonalModel;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Barrier,
    },
    thread,
};

#[test]
fn personal_snapshots_are_private_in_isolated_process() {
    let result = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "personal_privacy_child",
            "--ignored",
            "--nocapture",
        ])
        .env("LP_PRIVACY_TEST_CHILD", "1")
        .output()
        .unwrap();
    println!("{}", String::from_utf8_lossy(&result.stdout));
    assert!(
        result.status.success(),
        "isolated privacy regression failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
#[ignore = "only launched by personal_snapshots_are_private_in_isolated_process; never changes parent process umask"]
fn personal_privacy_child() {
    assert_eq!(std::env::var("LP_PRIVACY_TEST_CHILD").as_deref(), Ok("1"));
    unsafe extern "C" {
        fn umask(mask: u32) -> u32;
    }
    // The test binary is a dedicated child containing exactly this selected test.
    unsafe {
        umask(0o022);
    }
    let directory = tempfile::tempdir().unwrap();
    let snapshot = directory.path().join("personal.json");
    let temporary = directory
        .path()
        .join(format!(".personal.json.{}.tmp", std::process::id()));
    let alphabet: Vec<_> = "абвгдеёжзийклмнопрстуфхцчшщъыьэюя".chars().collect();
    let mut words = serde_json::Map::new();
    for id in 0..10_000 {
        let word = format!(
            "{}{}{}{}",
            "личное".repeat(12),
            alphabet[id % 33],
            alphabet[id / 33 % 33],
            alphabet[id / 1089 % 33]
        );
        words.insert(
            word,
            serde_json::json!({"confirmed_count":1,"selection_count":0,"last_event":1}),
        );
    }
    let profile: PersonalModel =
        serde_json::from_value(serde_json::json!({"words":words,"events":[1],"actions":[]}))
            .unwrap();
    let mut observed = None;
    let mut new_mode = None;
    for _ in 0..10 {
        let done = Arc::new(AtomicBool::new(false));
        let barrier = Arc::new(Barrier::new(2));
        let watcher = {
            let done = done.clone();
            let barrier = barrier.clone();
            let temporary = temporary.clone();
            thread::spawn(move || {
                barrier.wait();
                loop {
                    if let Ok(metadata) = fs::metadata(&temporary) {
                        return Some(metadata.permissions().mode() & 0o777);
                    }
                    if done.load(Ordering::Acquire) {
                        return None;
                    }
                    std::hint::spin_loop();
                }
            })
        };
        barrier.wait();
        let saved = profile.save(&snapshot);
        done.store(true, Ordering::Release);
        saved.unwrap();
        new_mode.get_or_insert(fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777);
        observed = watcher.join().unwrap();
        if observed.is_some() {
            break;
        }
    }
    let temporary_mode = observed.expect("must observe temporary creation within bounded attempts");
    let new_mode = new_mode.unwrap();
    fs::set_permissions(&snapshot, fs::Permissions::from_mode(0o600)).unwrap();
    profile.save(&snapshot).unwrap();
    let replacement_mode = fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777;
    let arpa = directory.path().join("general.arpa");
    let model = directory.path().join("general.lpk");
    fs::write(
        &arpa,
        "\\data\\\nngram 1=3\n\\1-grams:\n-3 <unk>\n0 <s>\n-1 </s>\n\\end\\\n",
    )
    .unwrap();
    predictor_core::Model::compile_arpa(arpa, &model, &predictor_core::PackageMetadata::default())
        .unwrap();
    let general_mode = fs::metadata(model).unwrap().permissions().mode() & 0o777;
    println!("isolated child pid={} umask=0022 temporary={temporary_mode:04o} new_snapshot={new_mode:04o} replacement_of_0600={replacement_mode:04o}",std::process::id());
    assert_eq!(
        temporary_mode, 0o600,
        "personal temporary must be private at creation"
    );
    assert_eq!(new_mode, 0o600, "new personal snapshot must be private");
    assert_eq!(
        replacement_mode, 0o600,
        "replacement must not broaden private permissions"
    );
    assert_eq!(
        general_mode, 0o644,
        "general model publication mode must remain unchanged under umask 022"
    );
    println!("general model remains {general_mode:04o}");
}
