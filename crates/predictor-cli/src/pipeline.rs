use crate::{evaluate, prepare};
use predictor_core::{Model, PackageMetadata, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub source: PathBuf,
    pub source_sha256: String,
    pub holdout: PathBuf,
    pub source_root: PathBuf,
    pub provenance_root: PathBuf,
    pub kenlm: PathBuf,
    pub kenlm_sha256: String,
    pub book_root: PathBuf,
    pub host_uuid: String,
    pub tiny_discount_fallback: bool,
    pub evaluation_limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stage {
    pub identity: String,
    pub directory: PathBuf,
    pub outputs: BTreeMap<String, String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub identity: String,
    pub state: String,
    pub active_stage: Option<String>,
    pub active_directory: Option<PathBuf>,
    pub stages: BTreeMap<String, Stage>,
    pub error: Option<String>,
}

pub fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
pub const COMMON_PACK_LIMIT: u64 = 500_000_000;
#[derive(Debug, Default, Serialize)]
pub struct BundleSizes {
    pub physical_bytes: u64,
    pub unpacked_bytes: u64,
}

pub fn directory_bytes(directory: &Path) -> Result<u64> {
    Ok(bundle_sizes(directory, COMMON_PACK_LIMIT)?.unpacked_bytes)
}

pub fn bundle_sizes(directory: &Path, limit: u64) -> Result<BundleSizes> {
    fn visit(directory: &Path, limit: u64, sizes: &mut BundleSizes) -> Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_symlink() || (!kind.is_dir() && !kind.is_file()) {
                return Err("bundle non-regular entry rejected".into());
            }
            if kind.is_dir() {
                visit(&path, limit, sizes)?;
                continue;
            }
            let physical = entry.metadata()?.len();
            sizes.physical_bytes = sizes
                .physical_bytes
                .checked_add(physical)
                .ok_or("physical bundle size overflow")?;
            if path.extension().is_some_and(|extension| extension == "bz2") {
                let mut decoder = bzip2::read::MultiBzDecoder::new(File::open(&path)?);
                let mut buffer = [0u8; 65536];
                loop {
                    let remaining = limit
                        .checked_sub(sizes.unpacked_bytes)
                        .ok_or("unpacked bundle exceeds limit")?;
                    let capacity = remaining.min(65535) as usize + 1;
                    let n = decoder.read(&mut buffer[..capacity])?;
                    if n == 0 {
                        break;
                    }
                    sizes.unpacked_bytes = sizes
                        .unpacked_bytes
                        .checked_add(n as u64)
                        .ok_or("unpacked bundle size overflow")?;
                    if sizes.unpacked_bytes > limit {
                        return Err("unpacked bundle exceeds limit".into());
                    }
                }
            } else {
                sizes.unpacked_bytes = sizes
                    .unpacked_bytes
                    .checked_add(physical)
                    .ok_or("unpacked bundle size overflow")?;
                if sizes.unpacked_bytes > limit {
                    return Err("unpacked bundle exceeds limit".into());
                }
            }
        }
        Ok(())
    }
    let mut sizes = BundleSizes::default();
    visit(directory, limit, &mut sizes)?;
    Ok(sizes)
}
fn hash_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let temp = path.with_extension(format!("{}.part", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    File::open(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?
    .sync_all()?;
    Ok(())
}

fn tree_files(root: &Path, directory: &Path, files: &mut BTreeMap<String, String>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err("symlinks are not accepted in hashed stage/source trees".into());
        }
        let path = entry.path();
        if kind.is_dir() {
            tree_files(root, &path, files)?;
        } else if kind.is_file() {
            files.insert(
                path.strip_prefix(root)?
                    .to_str()
                    .ok_or("non-UTF8 path")?
                    .into(),
                hash_file(&path)?,
            );
        }
    }
    Ok(())
}
fn outputs(directory: &Path) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    tree_files(directory, directory, &mut result)?;
    Ok(result)
}
pub fn verified_checkpoint(stage: &Stage, identity: &str) -> Result<bool> {
    if stage.identity != identity || !stage.directory.is_dir() || stage.outputs.is_empty() {
        return Ok(false);
    }
    Ok(outputs(&stage.directory)? == stage.outputs)
}

pub fn load_config(path: &Path) -> Result<Config> {
    Ok(serde_json::from_reader(File::open(path)?)?)
}

pub fn source_tree_hash(root: &Path) -> Result<String> {
    let mut sources = BTreeMap::new();
    tree_files(root, &root.join("crates"), &mut sources)?;
    tree_files(root, &root.join("spec"), &mut sources)?;
    for name in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"] {
        sources.insert(name.into(), hash_file(&root.join(name))?);
    }
    Ok(hash_bytes(&serde_json::to_vec(&sources)?))
}

pub fn identity(config: &Config) -> Result<String> {
    if hash_file(&config.source)? != config.source_sha256 {
        return Err("source SHA-256 mismatch".into());
    }
    if hash_file(&config.kenlm)? != config.kenlm_sha256 {
        return Err("KenLM SHA-256 mismatch".into());
    }
    Ok(hash_bytes(&serde_json::to_vec(
        &serde_json::json!({"config":config,"binary":hash_file(&std::env::current_exe()?)?,
        "source_tree":source_tree_hash(&config.source_root)?,"source":config.source_sha256,"holdout":hash_file(&config.holdout)?,
        "tools":outputs(&config.provenance_root)?}),
    )?))
}

pub struct Lease(File);
pub fn terminal_state(
    exit_code: Option<i32>,
    signal: Option<i32>,
    cancel_requested: bool,
) -> &'static str {
    if exit_code.is_none() && signal.is_none() {
        "interrupted"
    } else if exit_code == Some(0) && signal.is_none() {
        "completed"
    } else if cancel_requested {
        "cancelled"
    } else {
        "failed"
    }
}

impl Lease {
    pub fn claim(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        // flock is an open-file-description lock: an inherited descriptor keeps
        // the lease alive after any ancestor dies; PID files are not the lock.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(format!(
                "active lease {}; reconcile existing process before retry",
                path.display()
            )
            .into());
        }
        Ok(Self(file))
    }
    fn fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

fn inherit_lease(command: &mut Command, fd: RawFd) {
    // Only async-signal-safe fcntl runs after fork. KenLM itself retains this FD;
    // killing lp or its worker cannot make a live orphan trainer lose the lease.
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

pub fn host_guard(_config: &Config) -> Result<()> {
    // Private hardware identity is not published; never weaken the training gate.
    Err(
        "hostguard: private-host training/qualification is not available in the public checkout"
            .into(),
    )
}

pub fn cached_acquire(source: &Path, destination: &Path, expected: &str) -> Result<()> {
    if hash_file(source)? != expected {
        return Err("cached source checksum mismatch".into());
    }
    if destination.exists() {
        return if hash_file(destination)? == expected {
            Ok(())
        } else {
            Err("existing cache output is corrupt; choose/reconcile a new path".into())
        };
    }
    let part = destination.with_extension("part");
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&part)?;
    let offset = output.metadata()?.len();
    if offset > input.metadata()?.len() {
        return Err("partial source exceeds input size".into());
    }
    // Verify every existing partial byte against pinned input before resuming.
    let mut left = offset;
    let (mut a, mut b) = ([0u8; 65536], [0u8; 65536]);
    while left > 0 {
        let n = left.min(65536) as usize;
        input.read_exact(&mut a[..n])?;
        output.read_exact(&mut b[..n])?;
        if a[..n] != b[..n] {
            return Err("partial source prefix corrupted".into());
        }
        left -= n as u64;
    }
    output.seek(SeekFrom::Start(offset))?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    drop(output);
    if hash_file(&part)? != expected {
        return Err("acquired checksum mismatch".into());
    }
    fs::rename(part, destination)?;
    Ok(())
}

fn arpa_valid(path: &Path) -> Result<()> {
    use std::io::BufRead;
    let reader = std::io::BufReader::new(File::open(path)?);
    let mut data = false;
    let mut end = false;
    let mut trigram = false;
    for line in reader.lines() {
        let line = line?;
        data |= line == "\\data\\";
        end |= line == "\\end\\";
        trigram |= line == "\\3-grams:";
    }
    if !data || !end || !trigram {
        return Err("incomplete/non-trigram ARPA".into());
    }
    Ok(())
}

pub fn train(config: &Config, config_path: &Path, input: &Path, output: &Path) -> Result<()> {
    host_guard(config)?;
    if hash_file(&config.kenlm)? != config.kenlm_sha256 {
        return Err("trainer binary changed".into());
    }
    fs::create_dir_all(output)?;
    let lease = Lease::claim(&config.book_root.join("runs/heavy.lock"))?;
    atomic_json(&output.join("worker-config.json"), config)?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("__train-worker")
        .arg(output.join("worker-config.json"))
        .arg(input)
        .arg(output)
        .env("LP_LEASE_FD", lease.fd().to_string());
    inherit_lease(&mut command, lease.fd());
    let mut worker = command.spawn()?;
    atomic_json(
        &output.join("worker.json"),
        &serde_json::json!({"pid":worker.id(),"started_unix_ms":now(),"parent":std::process::id(),"input_sha256":hash_file(input)?,"original_config_sha256":hash_file(config_path)?}),
    )?;
    let status = worker.wait()?;
    if !status.success() {
        return Err(format!(
            "trainer worker failed: {status}; inspect {}",
            output.display()
        )
        .into());
    }
    arpa_valid(&output.join("model.arpa"))?;
    Ok(())
}

pub fn train_worker(config: &Config, input: &Path, output: &Path) -> Result<()> {
    host_guard(config)?;
    if hash_file(&config.kenlm)? != config.kenlm_sha256 {
        return Err("worker trainer SHA-256 mismatch".into());
    }
    let input_hash = hash_file(input)?;
    let fd: RawFd = std::env::var("LP_LEASE_FD")?.parse()?;
    if fd < 3 {
        return Err("invalid lease FD".into());
    }
    // SAFETY: fd is inherited from the spawning lp process; validate its inode
    // before use, then own exactly one File for this worker's descriptor.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return Err("lease FD not inherited".into());
    }
    let lease = unsafe { File::from_raw_fd(fd) };
    let expected = fs::metadata(config.book_root.join("runs/heavy.lock"))?;
    if lease.metadata()?.ino() != expected.ino() || lease.metadata()?.dev() != expected.dev() {
        return Err("wrong lease file".into());
    }
    if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("trainer worker does not own the heavy lease".into());
    }
    fs::create_dir(output.join("tmp"))?;
    let arpa = output.join("model.arpa.part");
    let mut command = Command::new(&config.kenlm);
    command
        .args(["-o", "3", "-S", "6G", "-T"])
        .arg(output.join("tmp"));
    if config.tiny_discount_fallback {
        command.arg("--discount_fallback");
    }
    command
        .stdin(Stdio::from(File::open(input)?))
        .stdout(Stdio::from(File::create(&arpa)?))
        .stderr(Stdio::from(File::create(output.join("kenlm.stderr"))?));
    inherit_lease(&mut command, fd);
    let argv: Vec<String> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    atomic_json(
        &output.join("command.json"),
        &serde_json::json!({"executable":config.kenlm,"sha256":hash_file(&config.kenlm)?,"argv":argv,"discount_fallback":config.tiny_discount_fallback,"stdin":input,"input_sha256":input_hash,"stdout":arpa,"start_unix_ms":now()}),
    )?;
    let child = command.spawn()?;
    let process = Command::new("/bin/ps")
        .args(["-p", &child.id().to_string(), "-o", "pid=,lstart=,command="])
        .output()?;
    atomic_json(
        &output.join("trainer.json"),
        &serde_json::json!({"pid":child.id(),"identity":String::from_utf8_lossy(&process.stdout),"worker":std::process::id()}),
    )?;
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let mut raw_status = 0;
    // wait4 records the actual trainer's resource usage, not the lp supervisor.
    loop {
        let result = unsafe { libc::wait4(child.id() as i32, &mut raw_status, 0, &mut usage) };
        if result >= 0 {
            break;
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    let status = std::process::ExitStatus::from_raw(raw_status);
    let exit = status.code();
    let signal = status.signal();
    let cancel_requested = output.join("cancel.requested").try_exists()?;
    atomic_json(
        &output.join("exit.json"),
        &serde_json::json!({"exit_code":exit,"signal":signal,"cancel_requested":cancel_requested,"state":terminal_state(exit,signal,cancel_requested),"finished_unix_ms":now(),"maximum_rss_bytes":usage.ru_maxrss,"user_seconds":usage.ru_utime.tv_sec as f64+usage.ru_utime.tv_usec as f64/1e6,"system_seconds":usage.ru_stime.tv_sec as f64+usage.ru_stime.tv_usec as f64/1e6}),
    )?;
    if !status.success() {
        return Err(format!("KenLM failed: {status}").into());
    }
    if hash_file(input)? != input_hash {
        return Err("training input changed during estimation".into());
    }
    arpa_valid(&arpa)?;
    File::open(&arpa)?.sync_all()?;
    fs::rename(arpa, output.join("model.arpa"))?;
    Ok(())
}

fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn stage_identity(identity: &str, name: &str, stages: &BTreeMap<String, Stage>) -> Result<String> {
    let dependency = match name {
        "acquire" => None,
        "prepare" => Some("acquire"),
        "train" => Some("prepare"),
        "compile" => Some("train"),
        "evaluate" => Some("compile"),
        _ => return Err("unknown checkpoint stage".into()),
    };
    Ok(hash_bytes(&serde_json::to_vec(&(
        identity,
        name,
        dependency
            .and_then(|d| stages.get(d))
            .map(|s| (&s.identity, &s.outputs)),
    ))?))
}

pub fn word_counts(path: &Path) -> Result<BTreeMap<String, u32>> {
    use std::io::BufRead;
    let mut counts = BTreeMap::new();
    for line in std::io::BufReader::new(File::open(path)?).lines() {
        let line = line?;
        let (word, count) = line.split_once('\t').ok_or("invalid frequency record")?;
        if counts.insert(word.into(), count.parse()?).is_some() {
            return Err("duplicate frequency record".into());
        }
    }
    Ok(counts)
}

pub fn compact_attribution(prepared: &Path, destination: &Path) -> Result<()> {
    let report: prepare::PrepareReport =
        serde_json::from_reader(File::open(prepared.join("report.json"))?)?;
    if report.grouping_version != prepare::GROUPING_VERSION {
        return Err("source_old: preparation lacks duplicate/original component isolation".into());
    }
    let connection = rusqlite::Connection::open_with_flags(
        prepared.join("families.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    connection.execute_batch("PRAGMA cache_size=-8192; PRAGMA temp_store=FILE;")?;
    let count: i64 =
        connection.query_row("SELECT COUNT(*) FROM attribution", [], |row| row.get(0))?;
    let audit_sha256 = hash_file(&prepared.join("attribution.jsonl"))?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut output = bzip2::write::BzEncoder::new(file, bzip2::Compression::best());
    serde_json::to_writer(
        &mut output,
        &serde_json::json!({
            "format":"linka-compact-attribution-jsonl-v1","grouping_version":prepare::GROUPING_VERSION,
            "source_ref":0,"sources":[{"id":"tatoeba","sentence_url_template":"https://tatoeba.org/en/sentences/show/{sentence_id}","license":"CC-BY-2.0-FR","license_url":"https://creativecommons.org/licenses/by/2.0/fr/"}],
            "fields":["sentence_id","author","segment","family_ref","component_ref"],"records":count,
            "family_reference":"1-based binary-ordered normalized-family label in the bound audit",
            "component_reference":"1-based SHA256-ordered component label in the bound audit",
            "audit_sha256":audit_sha256,"audit_retained":prepared.join("attribution.jsonl"),
            "raw_source_binding":"model provenance.source is the pinned raw-source SHA256"
        }),
    )?;
    writeln!(output)?;
    let mut statement=connection.prepare("SELECT a.sentence_id,a.author,a.segment,f.id,DENSE_RANK() OVER (ORDER BY f.component) FROM attribution a JOIN families f USING(family) ORDER BY f.family,a.sentence_id,a.segment,a.author")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        serde_json::to_writer(
            &mut output,
            &serde_json::json!([
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?
            ]),
        )?;
        writeln!(output)?;
    }
    output.finish()?.sync_all()?;
    Ok(())
}

pub fn reconcile_train(manifest: &mut Manifest, id: &str, lease_path: &Path) -> Result<()> {
    manifest.state = "reconcile_required".into();
    let result = (|| -> Result<()> {
        let _probe = Lease::claim(lease_path)?;
        let directory = manifest
            .active_directory
            .clone()
            .ok_or("active trainer directory is missing")?;
        let receipt: serde_json::Value =
            serde_json::from_reader(File::open(directory.join("exit.json"))?)?;
        let code = receipt["exit_code"].as_i64();
        let signal = receipt["signal"].as_i64();
        match receipt["state"].as_str() {
            Some("completed") if code == Some(0) && receipt["signal"].is_null() => {
                arpa_valid(&directory.join("model.arpa"))?;
                if manifest.identity == id {
                    manifest.stages.insert(
                        "train".into(),
                        Stage {
                            identity: stage_identity(id, "train", &manifest.stages)?,
                            outputs: outputs(&directory)?,
                            directory,
                        },
                    );
                }
                manifest.state = "trained".into();
                manifest.error = None;
                Ok(())
            }
            Some(state @ ("failed" | "cancelled"))
                if (code.is_some_and(|n| (1..=255).contains(&n))
                    && receipt["signal"].is_null())
                    || (receipt["exit_code"].is_null() && signal.is_some_and(|n| n > 0)) =>
            {
                if state == "cancelled" && receipt["cancel_requested"] != true {
                    return Err("cancellation receipt lacks explicit intent".into());
                }
                manifest.state = state.into();
                Err(
                    format!("trainer terminal {state}; a separate explicit attempt is required")
                        .into(),
                )
            }
            _ => Err("trainer receipt is not a valid terminal outcome".into()),
        }
    })();
    if let Err(error) = result {
        if manifest.state != "failed" && manifest.state != "cancelled" {
            manifest.state = "reconcile_required".into();
        }
        let message = if manifest.state == "reconcile_required" {
            format!("reconcile_required: {error}")
        } else {
            error.to_string()
        };
        manifest.error = Some(message.clone());
        return Err(message.into());
    }
    Ok(())
}

pub fn evaluate_stage(
    model: &Path,
    input: &Path,
    maximum_groups: usize,
) -> Result<serde_json::Value> {
    let grouped = evaluate::read_grouped_phrases(input, maximum_groups)?;
    let mut metrics = evaluate::evaluate_grouped(model, &grouped)?;
    metrics["synthetic_corrections"] =
        crate::diagnostics::correction_report(model, &grouped.phrases)?;
    metrics["model_sha256"] = serde_json::json!(hash_file(model)?);
    Ok(metrics)
}

/// Claim only a new directory, or read a valid existing resume marker without writes.
pub fn admit_run(root: &Path, resume: bool) -> Result<Manifest> {
    if !resume {
        fs::create_dir(root)?;
        return Ok(Manifest::default());
    }
    if !fs::symlink_metadata(root)?.is_dir() {
        return Err("resume requires an existing real run directory".into());
    }
    let path = root.join("manifest.json");
    if !fs::symlink_metadata(&path)?.is_file() {
        return Err("resume requires a regular readable manifest".into());
    }
    let manifest: Manifest = serde_json::from_reader(File::open(path)?)?;
    if manifest.identity.trim().is_empty() {
        return Err("invalid resume manifest: empty identity".into());
    }
    let active_required = match manifest.state.as_str() {
        "configured" | "acquired" | "prepared" | "trained" | "compiled" | "evaluated_dev" => false,
        "running" | "failed" | "cancelled" | "interrupted" | "reconcile_required" => true,
        _ => return Err("invalid resume manifest state".into()),
    };
    let names = ["acquire", "prepare", "train", "compile", "evaluate"];
    match (&manifest.active_stage, &manifest.active_directory) {
        (None, None) if !active_required => {}
        (Some(stage), Some(directory))
            if active_required
                && names.contains(&stage.as_str())
                && !directory.as_os_str().is_empty() => {}
        _ => return Err("invalid resume manifest tracking".into()),
    }
    if manifest.stages.iter().any(|(name, stage)| {
        !names.contains(&name.as_str())
            || stage.identity.trim().is_empty()
            || stage.directory.as_os_str().is_empty()
    }) {
        return Err("invalid resume checkpoint metadata".into());
    }
    Ok(manifest)
}

pub fn run(config_path: &Path, root: &Path, resume: bool) -> Result<Manifest> {
    let config = load_config(config_path)?;
    let mut manifest = admit_run(root, resume)?;
    let root_path = root.canonicalize()?;
    let root = root_path.as_path();
    let _run_lease = Lease::claim(&root.join("run.lock"))?;
    // The writer may have completed or changed tracking between admission and lock.
    if resume {
        manifest = admit_run(root, true)?;
    }
    let id = identity(&config)?;
    let manifest_path = root.join("manifest.json");
    let identity_changed = manifest.identity != id;
    // A dead lp is not evidence of dead KenLM. Check the inherited OS lease first.
    if manifest.active_stage.as_deref() == Some("train") {
        if let Err(error) = reconcile_train(
            &mut manifest,
            &id,
            &config.book_root.join("runs/heavy.lock"),
        ) {
            atomic_json(&manifest_path, &manifest)?;
            return Err(error);
        }
    }
    if identity_changed {
        manifest.stages.clear();
        manifest.identity = id.clone();
        manifest.state = "configured".into();
    }
    manifest.active_stage = None;
    manifest.active_directory = None;
    atomic_json(&root.join("resolved-config.json"), &config)?;
    let resolved = root.join("resolved-config.json").canonicalize()?;
    let mut invalidated = false;
    let names = ["acquire", "prepare", "train", "compile", "evaluate"];
    for (index, name) in names.iter().copied().enumerate() {
        let stage_id = stage_identity(&id, name, &manifest.stages)?;
        if !invalidated
            && manifest
                .stages
                .get(name)
                .is_some_and(|s| verified_checkpoint(s, &stage_id).unwrap_or(false))
        {
            continue;
        }
        if !invalidated {
            for descendant in &names[index..] {
                manifest.stages.remove(*descendant);
            }
        }
        invalidated = true;
        let directory = root.join(format!("{name}-{}-{}", now(), std::process::id()));
        fs::create_dir(&directory)?;
        manifest.state = "running".into();
        manifest.active_stage = Some(name.into());
        manifest.active_directory = Some(directory.clone());
        manifest.error = None;
        atomic_json(&manifest_path, &manifest)?;
        let result: Result<()> = (|| {
            match name {
                "acquire" => {
                    let file = if config.source.extension().is_some_and(|e| e == "bz2") {
                        "source.tsv.bz2"
                    } else {
                        "source.tsv"
                    };
                    cached_acquire(&config.source, &directory.join(file), &config.source_sha256)?;
                    fs::write(
                        directory.join("source-lock.json"),
                        serde_json::to_vec_pretty(
                            &serde_json::json!({"source":config.source,"sha256":config.source_sha256,"offline":true}),
                        )?,
                    )?;
                }
                "prepare" => {
                    let acquired = &manifest.stages["acquire"].directory;
                    let source = if acquired.join("source.tsv.bz2").exists() {
                        acquired.join("source.tsv.bz2")
                    } else {
                        acquired.join("source.tsv")
                    };
                    prepare::prepare(&source, &directory.join("data"), &config.holdout)?;
                }
                "train" => train(
                    &config,
                    &resolved,
                    &manifest.stages["prepare"].directory.join("data/train.txt"),
                    &directory,
                )?,
                "compile" => {
                    let stable_data_hashes: BTreeMap<_, _> = manifest.stages["prepare"]
                        .outputs
                        .iter()
                        .filter(|(name, _)| {
                            !name.ends_with("report.json") && !name.ends_with(".sqlite")
                        })
                        .collect();
                    let metadata = PackageMetadata {
                        provenance: serde_json::json!({"identity":id,"config":config,"source":config.source_sha256,"train":stable_data_hashes,
                            "source_tree_sha256":source_tree_hash(&config.source_root)?,"binary_sha256":hash_file(&std::env::current_exe()?)?,
                            "resolved_config_sha256":hash_file(&resolved)?,"evaluator_sha256":hash_file(&std::env::current_exe()?)?,
                            "holdout_sha256":hash_file(&config.holdout)?,"kenlm_sha256":config.kenlm_sha256}),
                        attribution: serde_json::json!({"source":"Tatoeba contributors","license":"CC-BY-2.0-FR","attribution_sidecar":"attribution.jsonl.bz2","attribution_format":"linka-compact-attribution-jsonl-v1","license_file":"notices/licenses/CC-BY-2.0-FR.html"}),
                        word_counts: word_counts(
                            &manifest.stages["prepare"]
                                .directory
                                .join("data/frequencies.tsv"),
                        )?,
                        ..Default::default()
                    };
                    Model::compile_arpa(
                        manifest.stages["train"].directory.join("model.arpa"),
                        directory.join("model.lpk"),
                        &metadata,
                    )?;
                    compact_attribution(
                        &manifest.stages["prepare"].directory.join("data"),
                        &directory.join("attribution.jsonl.bz2"),
                    )?;
                    fs::copy(
                        manifest.stages["prepare"]
                            .directory
                            .join("data/frequencies.tsv"),
                        directory.join("frequencies.tsv"),
                    )?;
                    copy_tree(&config.provenance_root, &directory.join("notices"))?;
                    fs::write(directory.join("NOTICES.txt"),"Tatoeba Russian sentences, original contributors listed in attribution.jsonl.bz2 (compact JSONL, schema/source/license table in first line). Every original record/author/segment is retained; family/component labels refer to the hash-bound verbose prepare audit, retained outside the common bundle. CC BY 2.0 France. Text normalized and component-grouped; train-only MKN model. https://tatoeba.org/ https://creativecommons.org/licenses/by/2.0/fr/\nOpenCorpora dictionary NOT included: upstream access/eligibility blocked.\n")?;
                    bundle_sizes(&directory, COMMON_PACK_LIMIT)?;
                }
                "evaluate" => {
                    let model = manifest.stages["compile"].directory.join("model.lpk");
                    let mut metrics = evaluate_stage(
                        &model,
                        &manifest.stages["prepare"].directory.join("data/dev.txt"),
                        config.evaluation_limit,
                    )?;
                    metrics["pipeline_identity"] = serde_json::json!(id);
                    metrics["model_sha256"] = serde_json::json!(hash_file(&model)?);
                    atomic_json(&directory.join("metrics.json"), &metrics)?;
                }
                _ => unreachable!(),
            }
            Ok(())
        })();
        if let Err(error) = result {
            let cancelled = File::open(directory.join("exit.json"))
                .ok()
                .and_then(|f| serde_json::from_reader::<_, serde_json::Value>(f).ok())
                .is_some_and(|v| v["state"] == "cancelled");
            manifest.state = if cancelled { "cancelled" } else { "failed" }.into();
            manifest.error = Some(error.to_string());
            atomic_json(&manifest_path, &manifest)?;
            return Err(error);
        }
        let hashes = outputs(&directory)?;
        for relative in hashes.keys() {
            File::open(directory.join(relative))?.sync_all()?;
        }
        manifest.stages.insert(
            name.into(),
            Stage {
                identity: stage_id,
                directory,
                outputs: hashes,
            },
        );
        manifest.state = match name {
            "acquire" => "acquired",
            "prepare" => "prepared",
            "train" => "trained",
            "compile" => "compiled",
            _ => "evaluated_dev",
        }
        .into();
        manifest.active_stage = None;
        manifest.active_directory = None;
        atomic_json(&manifest_path, &manifest)?;
    }
    manifest.state = "evaluated_dev".into();
    atomic_json(&manifest_path, &manifest)?;
    Ok(manifest)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_type()?.is_symlink() {
            return Err("notice symlink rejected".into());
        }
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), destination.join(entry.file_name()))?;
        }
    }
    Ok(())
}
