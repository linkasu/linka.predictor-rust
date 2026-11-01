use predictor_cli::{diagnostics, evaluate, lexicon, pipeline, prepare};
use predictor_core::{Model, PackageMetadata, PersonalModel, Predictor, Request, Result};
use serde_json::{json, Value};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

fn arg(args: &[String], index: usize) -> Result<&str> {
    args.get(index)
        .map(String::as_str)
        .ok_or_else(|| format!("missing positional argument {index}; see lp help").into())
}
fn path(args: &[String], index: usize) -> Result<&Path> {
    Ok(Path::new(arg(args, index)?))
}
fn print(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn direct_compile(
    arpa: &Path,
    out: &Path,
    frequencies: Option<&Path>,
    external: Option<&Path>,
) -> Result<()> {
    if out.exists() {
        return Err("compile output exists; choose a new immutable path".into());
    }
    let mut metadata = PackageMetadata {
        provenance: json!({"arpa_sha256":pipeline::hash_file(arpa)?,"binary_sha256":pipeline::hash_file(&std::env::current_exe()?)?,"frequency_sha256":frequencies.map(pipeline::hash_file).transpose()?,"external_lexicon_sha256":external.map(pipeline::hash_file).transpose()?}),
        attribution: json!({"note":"Direct compilation: caller must provide required original source attribution/notices alongside package; this command alone is not a redistributable bundle."}),
        ..Default::default()
    };
    if let Some(path) = frequencies {
        metadata.word_counts = pipeline::word_counts(path)?;
    }
    if let Some(path) = external {
        metadata.external_words = BufReader::new(File::open(path)?)
            .lines()
            .collect::<std::io::Result<Vec<_>>>()?;
    }
    let partial = out.with_extension("lpk.part");
    if partial.exists() {
        return Err("partial compile output exists; reconcile it or choose another path".into());
    }
    Model::compile_arpa(arpa, &partial, &metadata)?;
    Model::load(&partial)?;
    File::open(&partial)?.sync_all()?;
    fs::rename(partial, out)?;
    print(&json!({"model":out,"sha256":pipeline::hash_file(out)?}))
}

fn execute(args: &[String]) -> Result<()> {
    let bounds = match arg(args, 1)? {
        "help" | "--help" => 2..=2,
        "run" | "resume" | "lexicon" | "score" => 4..=4,
        "prepare" | "acquire" | "train" | "__train-worker" => 5..=5,
        "compile" | "complete" | "explain" => 4..=6,
        "evaluate" => 5..=7,
        "benchmark" | "personal-replay" => 6..=6,
        other => return Err(format!("unknown command {other}; lp help").into()),
    };
    if !bounds.contains(&args.len()) {
        return Err("unexpected argument count; lp help (unknown flags are never ignored)".into());
    }
    match arg(args, 1)? {
        "help" | "--help" => {
            println!("lp run|resume CONFIG RUN_DIR\nlp prepare RAW_TSV[.bz2] OUTPUT_DIR HOLDOUT\nlp acquire CACHED_SOURCE OUTPUT EXPECTED_SHA256\nlp lexicon DICT_XML[.bz2] WORDS_OUTPUT\nlp train CONFIG TRAIN_TXT OUTPUT_DIR\nlp compile ARPA OUTPUT_LPK [FREQUENCIES_TSV|-] [EXTERNAL_WORDS|-]\nlp complete|explain MODEL QUERY [LIMIT] [PERSONAL_STORE]\nlp score MODEL SENTENCE\nlp evaluate MODEL DEV_TXT OUTPUT_JSON [MAX_GROUPS] [FROZEN_TEST_MANIFEST]\nlp benchmark CONFIG MODEL INPUT_PHRASES OUTPUT_DIR\nlp personal-replay MODEL EVENTS_JSONL GENERAL_PHRASES OUTPUT_DIR");
            Ok(())
        }
        "acquire" => {
            pipeline::cached_acquire(path(args, 2)?, path(args, 3)?, arg(args, 4)?)?;
            print(&json!({"sha256":pipeline::hash_file(path(args,3)?)?,"offline":true}))
        }
        "prepare" => {
            let output = path(args, 3)?;
            if output.exists() {
                return Err("prepare output exists".into());
            }
            let part = output.with_extension("part");
            let report = prepare::prepare(path(args, 2)?, &part, path(args, 4)?)?;
            fs::rename(part, output)?;
            print(&report)
        }
        "lexicon" => {
            let out = path(args, 3)?;
            if out.exists() {
                return Err("lexicon output exists".into());
            }
            let part = out.with_extension("part");
            let counts = lexicon::extract_report(path(args, 2)?, &part)?;
            fs::rename(part, out)?;
            print(
                &json!({"wordforms":counts.wordforms,"unsupported_forms":counts.unsupported_forms,"duplicate_forms":counts.duplicate_forms,"sha256":pipeline::hash_file(out)?,"role":"external_lexical_candidates_only"}),
            )
        }
        "run" | "resume" => print(&pipeline::run(
            path(args, 2)?,
            path(args, 3)?,
            arg(args, 1)? == "resume",
        )?),
        "train" => {
            let config = pipeline::load_config(path(args, 2)?)?;
            pipeline::host_guard(&config)?;
            fs::create_dir(path(args, 4)?)?;
            pipeline::train(
                &config,
                &path(args, 2)?.canonicalize()?,
                &path(args, 3)?.canonicalize()?,
                &path(args, 4)?.canonicalize()?,
            )?;
            print(&json!({"state":"trained","directory":path(args,4)?}))
        }
        "__train-worker" => pipeline::train_worker(
            &pipeline::load_config(path(args, 2)?)?,
            path(args, 3)?,
            path(args, 4)?,
        ),
        "compile" => direct_compile(
            path(args, 2)?,
            path(args, 3)?,
            args.get(4).filter(|s| s.as_str() != "-").map(Path::new),
            args.get(5).filter(|s| s.as_str() != "-").map(Path::new),
        ),
        "score" => {
            let model = Model::load(path(args, 2)?)?;
            let text = arg(args, 3)?;
            let tokens = predictor_core::tokenize(text);
            let words: Vec<&str> = tokens.iter().map(|t| t.normalized.as_str()).collect();
            let mut context = vec!["<s>"];
            let mut scores = Vec::new();
            for word in words.iter().copied().chain(std::iter::once("</s>")) {
                scores.push(json!({"word":word,"log10":model.score(&context,word)}));
                context.push(word);
            }
            print(
                &json!({"sentence_log10":model.score_sentence(&words),"tokens_including_eos":scores}),
            )
        }
        "complete" | "explain" => {
            let predictor = Predictor::load(path(args, 2)?)?;
            let personal = args
                .get(5)
                .map(|p| PersonalModel::load(PathBuf::from(p)))
                .transpose()?;
            let request = Request {
                q: arg(args, 3)?,
                lang: "ru",
                limit: args.get(4).map(|s| s.parse()).transpose()?.unwrap_or(5),
            };
            if arg(args, 1)? == "complete" {
                print(&predictor.complete(request, personal.as_ref())?)
            } else {
                print(&predictor.explain(request, personal.as_ref())?)
            }
        }
        "evaluate" => {
            let model = path(args, 2)?;
            let input = path(args, 3)?;
            let out = path(args, 4)?;
            if out.exists() {
                return Err("evaluation output exists".into());
            }
            let grouped = evaluate::read_grouped_phrases(
                input,
                args.get(5).map(|s| s.parse()).transpose()?.unwrap_or(250),
            )?;
            let looks_test = input
                .file_name()
                .is_some_and(|n| n.to_string_lossy().to_lowercase().contains("test"));
            let mut final_claim = None;
            if looks_test || args.get(6).is_some() {
                let freeze_path = path(args, 6)?;
                let frozen: Value = serde_json::from_reader(File::open(freeze_path)?)?;
                evaluate::validate_frozen_selection(&frozen, &grouped)?;
                for (field, actual) in [
                    ("model_sha256", pipeline::hash_file(model)?),
                    (
                        "binary_sha256",
                        pipeline::hash_file(&std::env::current_exe()?)?,
                    ),
                    ("phrases_sha256", pipeline::hash_file(input)?),
                ] {
                    if frozen[field].as_str() != Some(&actual) {
                        return Err(format!("frozen manifest mismatch: {field}").into());
                    }
                }
                let config_path = Path::new(
                    frozen["config_path"]
                        .as_str()
                        .ok_or("frozen config_path required")?,
                );
                let config = pipeline::load_config(config_path)?;
                let policy = Path::new(
                    frozen["policy_path"]
                        .as_str()
                        .ok_or("frozen policy_path required")?,
                );
                for (field, actual) in [
                    (
                        "source_tree_sha256",
                        pipeline::source_tree_hash(&config.source_root)?,
                    ),
                    ("resolved_config_sha256", pipeline::hash_file(config_path)?),
                    ("policy_sha256", pipeline::hash_file(policy)?),
                    (
                        "evaluator_sha256",
                        pipeline::hash_file(&std::env::current_exe()?)?,
                    ),
                    ("pipeline_identity", pipeline::identity(&config)?),
                ] {
                    if frozen[field].as_str() != Some(&actual) {
                        return Err(format!("frozen manifest mismatch: {field}").into());
                    }
                }
                let claim_path = freeze_path.with_extension("evaluation-claim.json");
                let claim = std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&claim_path)?;
                serde_json::to_writer(
                    &claim,
                    &json!({"state":"claimed","pid":std::process::id(),"output":out,"note":"Unknown/interrupted outcomes require explicit reconciliation; never auto-replay final test."}),
                )?;
                claim.sync_all()?;
                final_claim = Some(claim_path);
            }
            let mut report = evaluate::evaluate_grouped(model, &grouped)?;
            report["synthetic_corrections"] =
                diagnostics::correction_report(model, &grouped.phrases)?;
            report["model_sha256"] = json!(pipeline::hash_file(model)?);
            report["phrases_sha256"] = json!(grouped.input_sha256);
            report["binary_sha256"] = json!(pipeline::hash_file(&std::env::current_exe()?)?);
            pipeline::atomic_json(out, &report)?;
            if let Some(claim) = final_claim {
                pipeline::atomic_json(
                    &claim,
                    &json!({"state":"completed","report":out,"report_sha256":pipeline::hash_file(out)?}),
                )?;
            }
            print(&json!({"report":out,"sha256":pipeline::hash_file(out)?}))
        }
        "benchmark" => print(&diagnostics::benchmark(
            &pipeline::load_config(path(args, 2)?)?,
            path(args, 3)?,
            path(args, 4)?,
            path(args, 5)?,
        )?),
        "personal-replay" => print(&diagnostics::personal_replay(
            path(args, 2)?,
            path(args, 3)?,
            path(args, 4)?,
            path(args, 5)?,
        )?),
        other => Err(format!("unknown command {other}; lp help").into()),
    }
}

fn main() {
    if let Err(error) = execute(&std::env::args().collect::<Vec<_>>()) {
        eprintln!("lp: {error}");
        std::process::exit(1);
    }
}
