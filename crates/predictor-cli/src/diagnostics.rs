use crate::{
    client::apply_suggestion,
    evaluate::{read_grouped_phrases, simulate, Actions},
    pipeline::{atomic_json, hash_file, host_guard, Config, Lease},
};
use predictor_core::{
    normalize, tokenize, ConfirmedEvent, PersonalModel, Predictor, Request, Response, Result,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Instant;
use unicode_segmentation::UnicodeSegmentation;

pub fn errors(target: &str) -> Vec<(String, String)> {
    let words = tokenize(target);
    let Some(last) = words.last() else {
        return vec![];
    };
    let token = &target[last.start_byte..last.end_byte];
    let graphemes: Vec<&str> = token.graphemes(true).collect();
    let mut cases = Vec::new();
    if graphemes.len() >= 3 {
        let mut variants = Vec::new();
        let mut insert = graphemes.clone();
        insert.insert(1, "а");
        variants.push(("insert", insert.concat()));
        let mut delete = graphemes.clone();
        delete.remove(1);
        variants.push(("delete", delete.concat()));
        let mut substitute = graphemes.clone();
        substitute[1] = if graphemes[1] == "я" { "б" } else { "я" };
        variants.push(("substitute", substitute.concat()));
        if let Some(index) = graphemes.windows(2).position(|w| w[0] != w[1]) {
            let mut swapped = graphemes.clone();
            swapped.swap(index, index + 1);
            variants.push(("transpose", swapped.concat()));
        }
        variants.push((
            "split",
            format!("{} {}", graphemes[..1].concat(), graphemes[1..].concat()),
        ));
        for (kind, replacement) in variants {
            cases.push((
                kind.into(),
                format!(
                    "{}{}{}",
                    &target[..last.start_byte],
                    replacement,
                    &target[last.end_byte..]
                ),
            ));
        }
    }
    if words.len() >= 2 {
        let previous = &words[words.len() - 2];
        let separator = &target[previous.end_byte..last.start_byte];
        if !separator.is_empty() && separator.chars().all(char::is_whitespace) {
            let joined = format!(
                "{}{}",
                &target[..previous.end_byte],
                &target[last.start_byte..]
            );
            cases.push(("join".into(), joined.clone()));
            let mut mixed = joined;
            mixed.insert(previous.end_byte, 'я');
            cases.push(("mixed".into(), mixed));
        }
    }
    cases
}

pub fn correction_report(model: &Path, phrases: &[String]) -> Result<Value> {
    let predictor = Predictor::load(model)?;
    let mut counters: BTreeMap<String, (u64, u64, u64, u64)> = BTreeMap::new();
    let mut details = Vec::new();
    for target in phrases {
        for (kind, q) in errors(target) {
            let completion = predictor.explain(Request::new(&q), None)?;
            let response: Response =
                serde_json::from_slice(&serde_json::to_vec(&completion.response)?)?;
            let hit = response.text.iter().position(|text| {
                apply_suggestion(&q, response.pos, text)
                    .ok()
                    .is_some_and(|s| normalize(&s) == normalize(target))
            });
            let pooled = completion.pool.iter().find(|c| {
                c.edit
                    .apply(&q)
                    .ok()
                    .is_some_and(|s| normalize(&s) == normalize(target))
            });
            let above = pooled.is_some_and(|c| c.score > completion.unchanged_score);
            let count = counters.entry(kind.clone()).or_default();
            count.0 += 1;
            count.1 += u64::from(hit == Some(0));
            count.2 += u64::from(hit.is_some());
            count.3 += u64::from(pooled.is_some());
            details.push(json!({"kind":kind,"query":q,"target":target,"top5_rank":hit.map(|r|r+1),"in_pool":pooled.is_some(),"target_above_unchanged":above,"unchanged_score":completion.unchanged_score,"truncated":completion.truncated}));
        }
    }
    Ok(
        json!({"slices":counters.into_iter().map(|(kind,(n,top1,top5,pool))|(kind,json!({"denominator":n,"top1":top1,"top5":top5,"pool_recall_hits":pool}))).collect::<BTreeMap<_,_>>(),"cases":details,"note":"Synthetic errors separate from clean paid-action KSR; no external lexicon ablation while OpenCorpora is blocked."}),
    )
}

pub fn percentile(sorted: &[u64], p: f64) -> u64 {
    sorted[((sorted.len() as f64 * p).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1)]
}

pub fn benchmark(config: &Config, model: &Path, input: &Path, output: &Path) -> Result<Value> {
    host_guard(config)?;
    if cfg!(debug_assertions) {
        return Err("qualification benchmark requires a release binary".into());
    }
    let _lease = Lease::claim(&config.book_root.join("runs/heavy.lock"))?;
    let bundle = model
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let bundle_sizes = crate::pipeline::bundle_sizes(bundle, crate::pipeline::COMMON_PACK_LIMIT)?;
    fs::create_dir(output)?;
    let slices: BTreeMap<String, Vec<String>> = serde_json::from_reader(File::open(input)?)?;
    for mode in ["completion", "next_word", "letters", "spaces", "mixed"] {
        if !slices
            .get(mode)
            .is_some_and(|queries| !queries.is_empty() && queries.iter().all(|q| q.len() <= 16384))
        {
            return Err(format!("invalid/missing benchmark slice {mode}").into());
        }
    }
    let opened = Instant::now();
    let predictor = Predictor::load(model)?;
    let open_ns = opened.elapsed().as_nanos();
    let first = Instant::now();
    let first_response = predictor.complete(Request::new(&slices["completion"][0]), None)?;
    std::hint::black_box(first_response);
    let first_ns = first.elapsed().as_nanos();
    let mut raw = BufWriter::new(File::create(output.join("timings.jsonl"))?);
    let mut metrics = Vec::new();
    for limit in [5, 100] {
        for mode in ["completion", "next_word", "letters", "spaces", "mixed"] {
            let queries = &slices[mode];
            for index in 0..50 {
                std::hint::black_box(predictor.complete(
                    Request {
                        q: &queries[index % queries.len()],
                        lang: "ru",
                        limit,
                    },
                    None,
                )?);
            }
            let mut durations = Vec::new();
            for index in 0..1000 {
                let q = &queries[index % queries.len()];
                let start = Instant::now();
                let response = predictor.complete(
                    Request {
                        q,
                        lang: "ru",
                        limit,
                    },
                    None,
                )?;
                std::hint::black_box(response);
                let ns = u64::try_from(start.elapsed().as_nanos())?;
                durations.push(ns);
                serde_json::to_writer(
                    &mut raw,
                    &json!({"mode":mode,"limit":limit,"iteration":index,"query":q,"duration_ns":ns}),
                )?;
                writeln!(raw)?;
            }
            durations.sort_unstable();
            metrics.push(json!({"mode":mode,"limit":limit,"samples":1000,"warmup":50,"p50_ns":percentile(&durations,0.5),"p95_ns":percentile(&durations,0.95),"p99_ns":percentile(&durations,0.99),"warm_p95_gate":limit!=5 || percentile(&durations,0.95)<=100000000}));
        }
    }
    raw.flush()?;
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let power = std::process::Command::new("/usr/bin/pmset")
        .args(["-g", "batt"])
        .output()?;
    let load = std::process::Command::new("/usr/bin/uptime").output()?;
    let bundle = model
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let bundle_complete = bundle.join("NOTICES.txt").is_file()
        && bundle.join("attribution.jsonl.bz2").is_file()
        && bundle.join("notices").is_dir();
    let report = json!({"model_sha256":hash_file(model)?,"input_sha256":hash_file(input)?,"binary_sha256":hash_file(&std::env::current_exe()?)?,"open_validation_ns":open_ns,"first_query_ns":first_ns,"cold_cache_note":"New model handle; OS page cache not flushed.","slices":metrics,"rss_max_bytes":usage.ru_maxrss,"heap_bytes":null,"heap_note":"No allocator instrumentation; mmap length is not reported as heap/RSS.","power":String::from_utf8_lossy(&power.stdout),"load":String::from_utf8_lossy(&load.stdout),"raw":"timings.jsonl","model_bytes":fs::metadata(model)?.len(),"bundle_directory_bytes":bundle_sizes.unpacked_bytes,"bundle_physical_bytes":bundle_sizes.physical_bytes,"bundle_unpacked_bytes":bundle_sizes.unpacked_bytes,"bundle_complete":bundle_complete,"bundle_size_gate":bundle_complete && bundle_sizes.unpacked_bytes<=crate::pipeline::COMMON_PACK_LIMIT,"qualification_note":"Full unpacked common bundle, including every bzip2 member, not compression savings. Directory includes all sibling files; mandatory notices/attribution must be present. Real LM size remains unmeasured; this does not certify missing source ablations or Android."});
    atomic_json(&output.join("benchmark.json"), &report)?;
    Ok(report)
}

fn general_sensitivity(
    predictor: &Predictor,
    phrases: &[String],
    personal: Option<&PersonalModel>,
) -> Result<Vec<Vec<Actions>>> {
    (1..=3)
        .map(|cost| {
            phrases
                .iter()
                .map(|phrase| {
                    simulate(phrase, cost, |q| {
                        predictor.complete(Request::new(q), personal)
                    })
                })
                .collect()
        })
        .collect()
}

pub fn personal_replay(
    model: &Path,
    events: &Path,
    general: &Path,
    output: &Path,
) -> Result<Value> {
    fs::create_dir(output)?;
    let predictor = Predictor::load(model)?;
    let model_before = hash_file(model)?;
    let mut personal = PersonalModel::default();
    let grouped = read_grouped_phrases(general, 250)?;
    let phrases = &grouped.phrases;
    let baseline = general_sensitivity(&predictor, phrases, None)?;
    let mut records = Vec::new();
    let mut previous_event = 0;
    for line in BufReader::new(File::open(events)?).lines() {
        let value: Value = serde_json::from_str(&line?)?;
        let query = value["query"].as_str().ok_or("replay query required")?;
        let expected = value["expected"]
            .as_str()
            .ok_or("replay expected required")?;
        let before = predictor.complete(Request::new(query), Some(&personal))?;
        let event: ConfirmedEvent = serde_json::from_value(value["event"].clone())?;
        if event.event_id < previous_event {
            return Err("replay events must be causal/nondecreasing".into());
        }
        previous_event = event.event_id;
        let learned = personal.observe(event)?;
        let after = predictor.complete(Request::new(query), Some(&personal))?;
        let available = |r: &Response| -> bool {
            r.text
                .iter()
                .any(|text| apply_suggestion(query, r.pos, text).ok().as_deref() == Some(expected))
        };
        records.push(json!({"query":query,"expected":expected,"before":available(&before),"after":available(&after),"learned":learned,"before_response":before,"after_response":after}));
    }
    if records.is_empty() {
        return Err("personal replay requires at least one explicit event".into());
    }
    let store = output.join("personal.json");
    personal.save(&store)?;
    let reopened = PersonalModel::load(&store)?;
    let after = general_sensitivity(&predictor, phrases, Some(&reopened))?;
    let mut general_costs = Vec::new();
    for (index, (before, after)) in baseline.into_iter().zip(after).enumerate() {
        let mean_delta = before
            .iter()
            .zip(&after)
            .map(|(before, after)| after.ksr - before.ksr)
            .sum::<f64>()
            / phrases.len() as f64;
        let before_mean =
            before.iter().map(|actions| actions.ksr).sum::<f64>() / phrases.len() as f64;
        let after_mean =
            after.iter().map(|actions| actions.ksr).sum::<f64>() / phrases.len() as f64;
        let per_phrase: Vec<Value> = phrases
            .iter()
            .zip(before.into_iter().zip(after))
            .map(|(target, (before, after))| json!({"target":target,"before":before,"after":after}))
            .collect();
        general_costs.push(json!({"selection_cost":index+1,"before_mean_ksr":before_mean,"after_mean_ksr":after_mean,"mean_ksr_delta":mean_delta,"regression_gate":mean_delta>=-0.01,"per_phrase":per_phrase}));
    }
    let primary = &general_costs[0];
    let report = json!({"events_sha256":hash_file(events)?,"general_sha256":grouped.input_sha256,"steps":records,
        "general_phrase_groups":grouped.group_count(),"general_phrase_count":phrases.len(),
        "general_grouping_version":grouped.grouping_version,"general_grouping_mode":grouped.grouping_mode,"general_group_map_sha256":grouped.group_map_sha256,
        "general_selection_protocol":crate::evaluate::SELECTION_PROTOCOL,"general_maximum_groups":grouped.maximum_groups,
        "general_costs":general_costs,
        "general_mean_ksr_delta":primary["mean_ksr_delta"],"general_mean_ksr_unit":"phrase within selected whole components, selection cost 1",
        "general_regression_gate":primary["regression_gate"],"model_hash_unchanged":model_before==hash_file(model)?,"reopened_words":reopened.words().count(),"note":"Only explicit causal events learn; query-only responses are measured before observe. OOV improvement must be checked against the chosen fixture; no universal pass implied."});
    private_atomic_json(&output.join("replay.json"), &report)?;
    Ok(report)
}

pub fn private_atomic_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let temporary = path.with_extension(format!("{}.private.part", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    File::open(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?
    .sync_all()?;
    Ok(())
}
