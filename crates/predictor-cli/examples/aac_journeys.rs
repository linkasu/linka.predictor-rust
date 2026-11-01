use predictor_core::{
    normalize, tokenize, ConfirmedEvent, EventOrigin, PersonalModel, Predictor, Request, Response,
    Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::Path,
    time::Instant,
};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    version: u32,
    scenarios: Vec<Scenario>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scenario {
    id: String,
    group: String,
    target: String,
    steps: Vec<Step>,
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    Type { text: String },
    Backspace { n: usize },
    Query,
    Select { rank: usize },
    Undo,
    Reject,
    Cancel,
    Confirm,
    RetryEvent,
    Restart,
}

#[derive(Default, Serialize)]
struct Actions {
    typed_graphemes: usize,
    typed_spaces: usize,
    backspaces: usize,
    selections: usize,
    unavailable_selections: usize,
    confirms: usize,
    undos: usize,
    rejects: usize,
    cancels: usize,
}
impl Actions {
    fn paid(&self, selection_cost: usize) -> usize {
        self.typed_graphemes
            + self.backspaces
            + self.selections * selection_cost
            + self.confirms
            + self.undos
            + self.rejects
            + self.cancels
    }
    fn type_text(&mut self, text: &str) {
        self.typed_graphemes += graphemes(text);
        self.typed_spaces += text.graphemes(true).filter(|s| *s == " ").count();
    }
}

fn graphemes(text: &str) -> usize {
    text.graphemes(true).count()
}

// Independent client: only the public wire response, never TailEdit/project.
fn apply(q: &str, pos: i32, text: &str) -> Result<String> {
    let mut units: Vec<u16> = q.encode_utf16().collect();
    if pos < 0 {
        let keep = i64::try_from(units.len())? + i64::from(pos);
        if keep < 0 {
            return Err("UTF-16 edit precedes buffer".into());
        }
        units.truncate(usize::try_from(keep)?);
        let prefix = String::from_utf16(&units)?;
        if prefix.len() != q.len() && !q.grapheme_indices(true).any(|(i, _)| i == prefix.len()) {
            return Err("UTF-16 edit splits grapheme".into());
        }
    } else if pos == 1 {
        units.push(u16::from(b' '));
    } else if pos != 0 {
        return Err("unsupported positive response position".into());
    }
    units.extend(text.encode_utf16());
    Ok(String::from_utf16(&units)?)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut sha = Sha256::new();
    let mut chunk = [0; 65536];
    loop {
        let n = file.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        sha.update(&chunk[..n]);
    }
    Ok(format!("{:x}", sha.finalize()))
}
fn profile_bytes(personal: &PersonalModel) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(personal)?)
}
fn ensure(ok: bool, message: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.to_owned().into())
    }
}
fn line(writer: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writeln!(writer)?;
    Ok(())
}

fn query(predictor: &Predictor, q: &str, personal: &PersonalModel) -> Result<(Response, u64)> {
    let before = profile_bytes(personal)?;
    let start = Instant::now();
    let response = predictor.complete(
        Request {
            q,
            lang: "ru",
            limit: 5,
        },
        Some(personal),
    )?;
    let ns = u64::try_from(start.elapsed().as_nanos())?;
    // Serialize/deserialize the public response before independently applying it.
    let wire: Response = serde_json::from_slice(&serde_json::to_vec(&response)?)?;
    let repeated = predictor.complete(
        Request {
            q,
            lang: "ru",
            limit: 5,
        },
        Some(personal),
    )?;
    ensure(wire == repeated, "query changed without an event")?;
    ensure(
        before == profile_bytes(personal)?,
        "prediction mutated profile",
    )?;
    ensure(wire.text.len() <= 5, "response exceeded limit")?;
    for text in &wire.text {
        apply(q, wire.pos, text)?;
    }
    Ok((wire, ns))
}
fn applied(q: &str, response: &Response) -> Result<Vec<String>> {
    response
        .text
        .iter()
        .map(|text| apply(q, response.pos, text))
        .collect()
}
fn result(
    scenario: &Scenario,
    mode: &str,
    cost: usize,
    q: &str,
    actions: &Actions,
    confirmations: usize,
) -> Value {
    let manual = graphemes(&scenario.target) + confirmations;
    let paid = actions.paid(cost);
    json!({"scenario":scenario.id,"group":scenario.group,"mode":mode,
        "selection_cost":cost,"target":scenario.target,"final_q":q,
        "exact_target":q==scenario.target,"manual_actions":manual,"paid_actions":paid,
        "saved_actions":manual as i64-paid as i64,
        "ksr":if manual==0 { None } else { Some(1.0-paid as f64/manual as f64) },
        "actions":actions})
}

struct WarmQuery {
    scenario: String,
    group: String,
    q: String,
}

fn scripts(
    model: &Path,
    fixture: &Fixture,
    out: &Path,
    cost: usize,
    log: &mut impl Write,
    warm_queries: &mut Vec<WarmQuery>,
) -> Result<Vec<Value>> {
    let mut predictor = Predictor::load(model)?;
    let mut personal = PersonalModel::default();
    let mut event_id = 0;
    let mut last_event: Option<ConfirmedEvent> = None;
    let mut results = Vec::new();
    for (utterance, scenario) in fixture.scenarios.iter().enumerate() {
        let mut q = String::new();
        let mut undo = None;
        let mut last_query = None;
        let mut actions = Actions::default();
        for (index, step) in scenario.steps.iter().enumerate() {
            let before_q = q.clone();
            let before_profile = profile_bytes(&personal)?;
            let (response, ns) = query(&predictor, &q, &personal)?;
            let candidates = applied(&q, &response)?;
            let mut event = None;
            let mut observed = None;
            match step {
                Step::Type { text } => {
                    q.push_str(text);
                    actions.type_text(text);
                }
                Step::Backspace { n } => {
                    for _ in 0..*n {
                        let (offset, _) = q
                            .grapheme_indices(true)
                            .next_back()
                            .ok_or("script backspace on empty buffer")?;
                        q.truncate(offset);
                        actions.backspaces += 1;
                    }
                }
                Step::Query => {
                    last_query = Some(q.clone());
                }
                Step::Select { rank } => {
                    ensure((1..=5).contains(rank), "script rank must be 1..5")?;
                    actions.selections += 1;
                    undo = Some(q.clone());
                    if let Some(text) = response.text.get(rank - 1) {
                        q = candidates[rank - 1].clone();
                        event_id += 1;
                        event = Some(ConfirmedEvent {
                            event_id,
                            utterance_id: utterance as u64 + 1,
                            origin: EventOrigin::Selection,
                            text: text.clone(),
                        });
                    } else {
                        actions.unavailable_selections += 1;
                    }
                }
                Step::Undo => {
                    q = undo
                        .take()
                        .ok_or("undo requires preceding selection attempt")?;
                    actions.undos += 1;
                }
                Step::Reject => {
                    actions.rejects += 1;
                }
                Step::Cancel => {
                    actions.cancels += 1;
                }
                Step::Confirm => {
                    actions.confirms += 1;
                    event_id += 1;
                    event = Some(ConfirmedEvent {
                        event_id,
                        utterance_id: utterance as u64 + 1,
                        origin: EventOrigin::Confirmation,
                        text: q.clone(),
                    });
                }
                Step::RetryEvent => {
                    let retry = last_event.clone().ok_or("retry requires previous event")?;
                    let changed = personal.observe(retry.clone())?;
                    ensure(
                        !changed && before_profile == profile_bytes(&personal)?,
                        "duplicate event was not idempotent",
                    )?;
                    event = Some(retry);
                    observed = Some(false);
                }
                Step::Restart => {
                    let path = out.join(format!("profile-c{cost}-{}-{index}.json", scenario.id));
                    personal.save(&path)?;
                    personal = PersonalModel::load(&path)?;
                    predictor = Predictor::load(model)?;
                    ensure(
                        before_profile == profile_bytes(&personal)?,
                        "restart changed profile",
                    )?;
                    ensure(
                        query(&predictor, &q, &personal)?.0 == response,
                        "restart changed prediction",
                    )?;
                    if let Some(retry) = &last_event {
                        ensure(!personal.observe(retry.clone())?, "restart lost event IDs")?;
                        ensure(
                            before_profile == profile_bytes(&personal)?,
                            "restart retry mutated profile",
                        )?;
                    }
                }
            }
            if let Some(new_event) = &event {
                if observed.is_none() {
                    observed = Some(personal.observe(new_event.clone())?);
                    ensure(observed == Some(true), "fresh explicit event was ignored")?;
                    last_event = Some(new_event.clone());
                }
            } else {
                ensure(
                    before_profile == profile_bytes(&personal)?,
                    "non-event action learned",
                )?;
            }
            line(
                log,
                &json!({"mode":"script","selection_cost":cost,"scenario":scenario.id,
                "group":scenario.group,"step":index,"action":step,"q":before_q,
                "response":response,"applied":candidates,"query_ns":ns,"after_q":q,
                "event":event,"observe_return":observed,
                "profile_before_sha256":digest(&before_profile),
                "profile_after_sha256":digest(&profile_bytes(&personal)?),
                "actions":actions,"paid_actions":actions.paid(cost)}),
            )?;
        }
        results.push(result(
            scenario,
            "script",
            cost,
            &q,
            &actions,
            actions.confirms,
        ));
        if cost == 1 {
            warm_queries.push(WarmQuery {
                scenario: scenario.id.clone(),
                group: scenario.group.clone(),
                q: last_query.ok_or("scenario lacks explicit query for latency")?,
            });
        }
    }
    personal.save(out.join(format!("profile-final-c{cost}.json")))?;
    Ok(results)
}

fn oracle(
    scenario: &Scenario,
    mode: &str,
    cost: usize,
    learn: bool,
    mut suggest: impl FnMut(&str, &PersonalModel) -> Result<Response>,
    log: &mut impl Write,
) -> Result<Value> {
    let mut personal = PersonalModel::default();
    let mut q = String::new();
    let mut actions = Actions::default();
    let mut event_id = 0;
    let mut index = 0;
    while q != scenario.target {
        let before_q = q.clone();
        let before_profile = profile_bytes(&personal)?;
        let response = suggest(&q, &personal)?;
        ensure(
            before_profile == profile_bytes(&personal)?,
            "oracle query mutated profile",
        )?;
        let candidates = applied(&q, &response)?;
        let mut best = None;
        for (rank, candidate) in candidates.iter().enumerate() {
            let boundary = candidate == &scenario.target
                || scenario
                    .target
                    .grapheme_indices(true)
                    .any(|(i, _)| i == candidate.len());
            if boundary
                && scenario.target.starts_with(candidate)
                && graphemes(candidate) > graphemes(&q) + cost
                && best.is_none_or(|r: usize| candidate.len() > candidates[r].len())
            {
                best = Some(rank);
            }
        }
        let mut event = None;
        let action;
        if let Some(rank) = best {
            q = candidates[rank].clone();
            actions.selections += 1;
            action = json!({"op":"select","rank":rank+1});
            if learn {
                event_id += 1;
                let e = ConfirmedEvent {
                    event_id,
                    utterance_id: 1,
                    origin: EventOrigin::Selection,
                    text: response.text[rank].clone(),
                };
                ensure(
                    personal.observe(e.clone())?,
                    "oracle selection not observed",
                )?;
                event = Some(e);
            }
        } else {
            let next = scenario.target[q.len()..]
                .graphemes(true)
                .next()
                .ok_or("oracle failed to advance")?;
            q.push_str(next);
            actions.type_text(next);
            action = json!({"op":"type","text":next});
        }
        line(
            log,
            &json!({"mode":mode,"selection_cost":cost,"scenario":scenario.id,
            "group":scenario.group,"step":index,"q":before_q,"response":response,
            "applied":candidates,"action":action,"after_q":q,"event":event,
            "observe_return":event.as_ref().map(|_|true),
            "profile_before_sha256":digest(&before_profile),
            "profile_after_sha256":digest(&profile_bytes(&personal)?),
            "actions":actions,"paid_actions":actions.paid(cost)}),
        )?;
        index += 1;
    }
    if !q.is_empty() {
        let before_profile = profile_bytes(&personal)?;
        let response = suggest(&q, &personal)?;
        actions.confirms += 1;
        let mut event = None;
        if learn {
            event_id += 1;
            let e = ConfirmedEvent {
                event_id,
                utterance_id: 1,
                origin: EventOrigin::Confirmation,
                text: q.clone(),
            };
            ensure(
                personal.observe(e.clone())?,
                "oracle confirmation not observed",
            )?;
            event = Some(e);
        }
        line(
            log,
            &json!({"mode":mode,"selection_cost":cost,"scenario":scenario.id,
            "group":scenario.group,"step":index,"q":q,"after_q":q,"response":response,
            "applied":applied(&q,&response)?,"action":{"op":"confirm"},"event":event,
            "observe_return":event.as_ref().map(|_|true),
            "profile_before_sha256":digest(&before_profile),
            "profile_after_sha256":digest(&profile_bytes(&personal)?),
            "actions":actions,"paid_actions":actions.paid(cost)}),
        )?;
    }
    Ok(result(
        scenario,
        mode,
        cost,
        &q,
        &actions,
        usize::from(!scenario.target.is_empty()),
    ))
}

fn read_counts(path: &Path) -> Result<Vec<(String, u64)>> {
    let mut seen = BTreeSet::new();
    let mut words = Vec::new();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        let (word, count) = line
            .split_once('\t')
            .ok_or("counts require word<TAB>count")?;
        let count: u64 = count.parse()?;
        let tokens = tokenize(word);
        ensure(
            count > 0
                && normalize(word) == word
                && tokens.len() == 1
                && tokens[0].start_byte == 0
                && tokens[0].end_byte == word.len()
                && seen.insert(word.to_owned()),
            "invalid/duplicate counts word",
        )?;
        words.push((word.to_owned(), count));
    }
    ensure(!words.is_empty(), "empty counts baseline")?;
    words.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(words)
}
fn frequency(q: &str, words: &[(String, u64)]) -> Result<Response> {
    let tokens = tokenize(q);
    let tail = tokens.last().filter(|t| t.end_byte == q.len());
    let prefix = tail.map(|t| t.normalized.as_str()).unwrap_or("");
    let pos = if let Some(t) = tail {
        -i32::try_from(q[t.start_byte..].encode_utf16().count())?
    } else if !q.is_empty() && !q.chars().last().is_some_and(char::is_whitespace) {
        1
    } else {
        0
    };
    let text: Vec<_> = words
        .iter()
        .filter(|(w, _)| w.starts_with(prefix) && w != prefix)
        .take(5)
        .map(|(w, _)| w.clone())
        .collect();
    Ok(Response {
        end_of_word: !text.is_empty(),
        text,
        pos,
    })
}

fn percentiles(samples: &mut [u64]) -> Value {
    samples.sort_unstable();
    let p = |fraction: f64| samples[((samples.len() as f64 * fraction).ceil() as usize) - 1];
    json!({"samples":samples.len(),"p50_ns":p(0.50),"p95_ns":p(0.95),"p99_ns":p(0.99)})
}
fn latency_samples(value: Option<&str>) -> Result<usize> {
    let count = value.map(str::parse).transpose()?.unwrap_or(100);
    ensure(count > 0, "AAC_LATENCY_SAMPLES must be positive")?;
    Ok(count)
}
fn latency(
    predictor: &Predictor,
    warm_queries: &[WarmQuery],
    out: &Path,
    samples: usize,
) -> Result<Value> {
    let mut groups: BTreeMap<&str, Vec<&WarmQuery>> = BTreeMap::new();
    for query in warm_queries {
        groups.entry(&query.group).or_default().push(query);
    }
    let mut raw = BufWriter::new(File::create(out.join("latency.jsonl"))?);
    let seed = out.join("profile-final-c1.json");
    let personal = PersonalModel::load(&seed)?;
    let seeded_bytes = profile_bytes(&personal)?;
    let profile_hash = digest(&seeded_bytes);
    let mut series = BTreeMap::new();
    for (label, profile) in [("none", None), ("final_script_c1", Some(&personal))] {
        let mut all = Vec::new();
        let mut summary = BTreeMap::new();
        for (group, queries) in &groups {
            for i in 0..10 {
                std::hint::black_box(predictor.complete(
                    Request {
                        q: &queries[i % queries.len()].q,
                        lang: "ru",
                        limit: 5,
                    },
                    profile,
                )?);
            }
            let mut times = Vec::new();
            for (qi, query) in queries.iter().enumerate() {
                let q = &query.q;
                for iteration in 0..samples {
                    let start = Instant::now();
                    let response = predictor.complete(
                        Request {
                            q,
                            lang: "ru",
                            limit: 5,
                        },
                        profile,
                    )?;
                    let ns = u64::try_from(start.elapsed().as_nanos())?;
                    std::hint::black_box(response);
                    times.push(ns);
                    all.push(ns);
                    line(
                        &mut raw,
                        &json!({"group":group,"scenario":query.scenario,
                        "query_index":qi,"iteration":iteration,"q":q,"duration_ns":ns,
                        "lang":"ru","limit":5,"profile":label,
                        "profile_sha256":profile.map(|_|profile_hash.as_str())}),
                    )?;
                }
            }
            summary.insert(
                *group,
                json!({"queries":queries.len(),"warmups":10,
                "samples_per_query":samples,"latency":percentiles(&mut times)}),
            );
        }
        ensure(
            seeded_bytes == profile_bytes(&personal)?,
            "timed calls mutated seeded profile",
        )?;
        series.insert(
            label,
            json!({"overall":percentiles(&mut all),"groups":summary,
            "profile":label,"profile_sha256":profile.map(|_|profile_hash.as_str()),
            "profile_seed":profile.map(|_|seed.as_path()),"learning_during_timing":false}),
        );
    }
    ensure(
        hash_file(&seed)? == profile_hash,
        "timed calls changed persisted profile",
    )?;
    raw.flush()?;
    Ok(json!({"series":series,"samples_per_query":samples,
        "sample_design":"Repeated fixed queries, non-IID; no uncertainty/confidence claim. p99 is noisy at low sample counts."}))
}

fn verify_freeze(fixture: &Path) -> Result<Value> {
    let parent = fixture.parent().unwrap_or(Path::new("."));
    let prefix = fixture
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".fixtures.json"))
        .ok_or("fixture must end .fixtures.json")?;
    let policy = parent.join(format!("{prefix}.policy.md"));
    let exclusions = parent.join("journeys-exclusions.txt");
    let freeze = parent.join(format!("{prefix}.freeze.sha256"));
    let original_freeze = parent.join("journeys.freeze.sha256");
    let originals = if prefix == "journeys" {
        None
    } else {
        Some(verify_freeze(&parent.join("journeys.fixtures.json"))?)
    };
    let lines = fs::read_to_string(&freeze)?;
    let mut paths = vec![fixture, policy.as_path(), exclusions.as_path()];
    if originals.is_some() {
        paths.push(original_freeze.as_path());
    }
    for path in paths {
        let name = path.file_name().ok_or("missing filename")?;
        let expected: Vec<_> = lines
            .lines()
            .filter_map(|line| {
                let (sha, p) = line.split_once("  ")?;
                (Path::new(p).file_name() == Some(name)).then_some(sha)
            })
            .collect();
        ensure(
            expected.len() == 1 && expected[0] == hash_file(path)?,
            "frozen input hash mismatch",
        )?;
    }
    Ok(
        json!({"fixture_sha256":hash_file(fixture)?,"policy_sha256":hash_file(&policy)?,"originals":originals,
        "exclusions_sha256":hash_file(&exclusions)?,
        "freeze_sha256":hash_file(&freeze)?}),
    )
}
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure(
        args.len() == 3 || args.len() == 4,
        "usage: aac_journeys MODEL FIXTURE OUT [COUNTS]",
    )?;
    let model = Path::new(&args[0]);
    let input = Path::new(&args[1]);
    let out = Path::new(&args[2]);
    let samples_env = std::env::var("AAC_LATENCY_SAMPLES").ok();
    let samples = latency_samples(samples_env.as_deref())?;
    let freeze = verify_freeze(input)?;
    let fixture: Fixture = serde_json::from_reader(File::open(input)?)?;
    ensure(
        fixture.version == 1 && !fixture.scenarios.is_empty(),
        "invalid fixture version/empty",
    )?;
    let mut ids = BTreeSet::new();
    for s in &fixture.scenarios {
        ensure(
            !s.id.is_empty()
                && s.id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
                && ids.insert(&s.id)
                && !s.group.is_empty()
                && !s.steps.is_empty(),
            "invalid scenario",
        )?;
    }
    let model_hash = hash_file(model)?;
    let counts = args
        .get(3)
        .map(|path| read_counts(Path::new(path)))
        .transpose()?;
    fs::create_dir(out)?;
    let mut log = BufWriter::new(File::create(out.join("steps.jsonl"))?);
    let predictor = Predictor::load(model)?;
    let mut results = Vec::new();
    let mut warm_queries = Vec::new();
    for cost in 1..=3 {
        results.extend(scripts(
            model,
            &fixture,
            out,
            cost,
            &mut log,
            &mut warm_queries,
        )?);
        for scenario in &fixture.scenarios {
            results.push(oracle(
                scenario,
                "target_aware_oracle",
                cost,
                true,
                |q, p| Ok(query(&predictor, q, p)?.0),
                &mut log,
            )?);
            if let Some(words) = &counts {
                results.push(oracle(
                    scenario,
                    "train_frequency_oracle",
                    cost,
                    false,
                    |q, _| frequency(q, words),
                    &mut log,
                )?);
            }
        }
    }
    log.flush()?;
    let timings = latency(&predictor, &warm_queries, out, samples)?;
    ensure(model_hash == hash_file(model)?, "common model mutated")?;
    let baseline = if let Some(path) = args.get(3) {
        json!({"available":true,"counts_sha256":hash_file(Path::new(path))?,
            "words":counts.as_ref().map(Vec::len),"provenance":"caller-supplied, not independently verified"})
    } else {
        json!({"available":false,"reason":"no real training-count TSV supplied; ARPA proxy forbidden"})
    };
    let mut grouped: BTreeMap<String, usize> = BTreeMap::new();
    for scenario in &fixture.scenarios {
        *grouped.entry(scenario.group.clone()).or_default() += 1;
    }
    let report = json!({"schema":"synthetic-aac-journeys-v1","synthetic_not_human_acceptance":true,
        "frozen_inputs":freeze,"model_sha256":model_hash,
        "binary_sha256":hash_file(&std::env::current_exe()?)?,
        "common_model_unchanged":true,"scenarios":fixture.scenarios.len(),"group_counts":grouped,
        "evaluation_design":if input.file_name().is_some_and(|n|n=="journeys.fixtures.json") {
            "Original pre-execution authored synthetic suite; not human acceptance"
        } else { "Supplemental regression authored after inspecting model outputs; not independent quality evaluation" },
        "results":results,"frequency_baseline":baseline,"warm_latency":timings,
        "steps_sha256":hash_file(&out.join("steps.jsonl"))?,
        "latency_samples_sha256":hash_file(&out.join("latency.jsonl"))?,
        "limitations":["Synthetic scripted choices and optimistic oracle, not human acceptance",
            "No network sandbox or full isolation proved", "Restart is handle reload, not OS process restart",
            "Undo retains explicitly learned selection events", "Host timings are not Android qualification"]});
    let mut report_file = BufWriter::new(File::create(out.join("summary.json"))?);
    serde_json::to_writer_pretty(&mut report_file, &report)?;
    writeln!(report_file)?;
    report_file.flush()?;
    println!(
        "{} scenarios; results: {}",
        fixture.scenarios.len(),
        out.join("summary.json").display()
    );
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("aac_journeys: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_independent_utf16_and_graphemes() -> Result<()> {
        assert_eq!(apply("😀 е\u{308}ж вр", -2, "врач")?, "😀 е\u{308}ж врач");
        assert_eq!(apply("дай", 1, "воды")?, "дай воды");
        assert_eq!(apply("", 0, "ёж")?, "ёж");
        assert!(apply("😀", -1, "x").is_err());
        assert!(apply("е\u{308}", -1, "x").is_err());
        assert!(apply("x", -2, "x").is_err());
        assert!(apply("x", 2, "x").is_err());
        Ok(())
    }

    #[test]
    fn test_action_costs_include_spaces_repairs_and_negative_savings() {
        let scenario = Scenario {
            id: "repair".into(),
            group: "test".into(),
            target: "ёж".into(),
            steps: vec![],
        };
        let mut actions = Actions {
            backspaces: 2,
            selections: 1,
            confirms: 1,
            undos: 1,
            rejects: 1,
            cancels: 1,
            ..Actions::default()
        };
        actions.type_text("😀 е\u{308}ж");
        assert_eq!(actions.typed_graphemes, 4);
        assert_eq!(actions.typed_spaces, 1);
        for cost in 1..=3 {
            assert_eq!(actions.paid(cost), 10 + cost);
            let value = result(&scenario, "script", cost, "ёж", &actions, 1);
            assert!(value["ksr"].as_f64().unwrap() < 0.0);
            assert_eq!(value["saved_actions"], 3i64 - (10 + cost) as i64);
        }
    }

    #[test]
    fn test_oracle_cost_and_frequency_counts() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("counts.tsv");
        fs::write(&path, "ёж\t2\nврач\t9\nвода\t9\n")?;
        let words = read_counts(&path)?;
        assert_eq!(words[0].0, "вода");
        let response = frequency("😀 в", &words)?;
        assert_eq!(response.pos, -1);
        assert_eq!(applied("😀 в", &response)?, vec!["😀 вода", "😀 врач"]);
        let scenario = Scenario {
            id: "oracle".into(),
            group: "test".into(),
            target: "вода".into(),
            steps: vec![],
        };
        let mut log = Vec::new();
        let value = oracle(
            &scenario,
            "test",
            1,
            false,
            |q, _| frequency(q, &words),
            &mut log,
        )?;
        assert_eq!(value["paid_actions"], 2);
        assert_eq!(value["actions"]["selections"], 1);
        assert_eq!(value["actions"]["confirms"], 1);
        assert_eq!(value["manual_actions"], 5);
        assert!(value["exact_target"].as_bool().unwrap());
        fs::write(&path, "ёж\t2\nёж\t3\n")?;
        assert!(read_counts(&path).is_err());
        fs::write(&path, "ёж\t0\n")?;
        assert!(read_counts(&path).is_err());
        fs::write(&path, "Ёж\t2\n")?;
        assert!(read_counts(&path).is_err());
        Ok(())
    }

    #[test]
    fn test_explicit_origins_retry_and_restart() -> Result<()> {
        let mut personal = PersonalModel::default();
        let selection = ConfirmedEvent {
            event_id: 1,
            utterance_id: 1,
            origin: EventOrigin::Selection,
            text: "зюмарика".into(),
        };
        assert!(personal.observe(selection.clone())?);
        let (_, counts) = personal.words().next().unwrap();
        assert_eq!(counts.confirmed_count, 0);
        assert_eq!(counts.selection_count, 1);
        let confirmation = ConfirmedEvent {
            event_id: 2,
            utterance_id: 1,
            origin: EventOrigin::Confirmation,
            text: "зюмарика".into(),
        };
        assert!(personal.observe(confirmation.clone())?);
        let before = profile_bytes(&personal)?;
        assert!(!personal.observe(confirmation.clone())?);
        assert_eq!(before, profile_bytes(&personal)?);
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("personal.json");
        personal.save(&path)?;
        let mut loaded = PersonalModel::load(&path)?;
        assert!(!loaded.observe(selection)?);
        assert!(!loaded.observe(confirmation)?);
        assert_eq!(before, profile_bytes(&loaded)?);
        let (_, counts) = loaded.words().next().unwrap();
        assert_eq!(counts.confirmed_count, 1);
        assert_eq!(counts.selection_count, 1);
        Ok(())
    }

    #[test]
    fn test_supplement_freeze_binds_originals() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        for (name, content) in [
            ("journeys.fixtures.json", "original fixture"),
            ("journeys.policy.md", "original policy"),
            ("journeys-exclusions.txt", "мне нужна помощь\n"),
            ("journeys-retained.fixtures.json", "supplement fixture"),
            ("journeys-retained.policy.md", "post-model-open regression"),
        ] {
            fs::write(root.join(name), content)?;
        }
        let lock = |name: &str, files: &[&str]| -> Result<()> {
            let mut contents = String::new();
            for file in files {
                contents.push_str(&format!("{}  {file}\n", hash_file(&root.join(file))?));
            }
            fs::write(root.join(name), contents)?;
            Ok(())
        };
        lock(
            "journeys.freeze.sha256",
            &[
                "journeys.fixtures.json",
                "journeys.policy.md",
                "journeys-exclusions.txt",
            ],
        )?;
        lock(
            "journeys-retained.freeze.sha256",
            &[
                "journeys-retained.fixtures.json",
                "journeys-retained.policy.md",
                "journeys-exclusions.txt",
                "journeys.freeze.sha256",
            ],
        )?;
        let input = root.join("journeys-retained.fixtures.json");
        assert!(verify_freeze(&input).is_ok());
        fs::write(root.join("journeys.fixtures.json"), "changed original")?;
        assert!(verify_freeze(&input).is_err());
        Ok(())
    }

    #[test]
    fn test_latency_sample_override_and_nearest_rank() -> Result<()> {
        assert_eq!(latency_samples(None)?, 100);
        assert_eq!(latency_samples(Some("1000"))?, 1000);
        assert!(latency_samples(Some("0")).is_err());
        assert!(latency_samples(Some("bad")).is_err());
        let mut samples: Vec<_> = (1..=100).rev().collect();
        let result = percentiles(&mut samples);
        assert_eq!(result["p50_ns"], 50);
        assert_eq!(result["p95_ns"], 95);
        assert_eq!(result["p99_ns"], 99);
        Ok(())
    }
}
