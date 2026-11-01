use predictor_core::{Model, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
};

const TOLERANCE: f64 = 0.0001;

#[test]
fn test_parse_kenlm_raw_totals_and_empty_case() {
    let cases = parse_references(std::io::Cursor::new(
        "я=3 2 -0.6\t</s>=2 2 -0.4\tTotal: -1 OOV: 0\n</s>=2 1 -1.04\tTotal: -1.04 OOV: 0\nPerplexity including OOVs:\t10\nPerplexity excluding OOVs:\t10\nOOVs:\t0\nTokens:\t3\n",
    )).unwrap();
    assert_eq!(cases.len(), 2);
    assert_eq!(cases[0].words, ["я"]);
    assert_eq!(cases[0].total, -1.0);
    assert!(cases[1].words.is_empty());
}

#[test]
fn test_reject_malformed_kenlm_totals() {
    for line in [
        "</s>=2 1 -1 Total:-1 OOV: 0",
        "</s>=2 1 -1 Total: NaN OOV: 0",
        "</s>=2 1 -1 Total: inf OOV: 0",
        "</s>=2 1 -1 Total: -1 OOV: nope",
        "</s>=2 1 -1 Total: -1 OOV: 0 trailing",
        "</s>=2 1 -1 Total: -1 Total: -2 OOV: 0",
        "я=3 1 -1 Total: -1 OOV: 0",
        "unrecognized output",
        "",
    ] {
        assert!(
            parse_references(std::io::Cursor::new(line)).is_err(),
            "{line}"
        );
    }
}

#[derive(Debug)]
struct ReferenceCase {
    words: Vec<String>,
    total: f64,
    oov: usize,
}

fn parse_references(input: impl BufRead) -> Result<Vec<ReferenceCase>> {
    let mut cases = Vec::new();
    for (index, line) in input.lines().enumerate() {
        let line = line?;
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.is_empty() {
            continue;
        }
        let malformed = || format!("malformed KenLM stdout line {}: {line}", index + 1);
        let totals: Vec<_> = fields
            .iter()
            .enumerate()
            .filter(|(_, f)| **f == "Total:")
            .map(|(i, _)| i)
            .collect();
        if totals.len() == 1 {
            let total = totals[0];
            if total < 3
                || total % 3 != 0
                || fields.len() != total + 4
                || fields[total + 2] != "OOV:"
            {
                return Err(malformed().into());
            }
            let probability: f64 = fields[total + 1].parse().map_err(|_| malformed())?;
            let oov: usize = fields[total + 3].parse().map_err(|_| malformed())?;
            if !probability.is_finite() {
                return Err(malformed().into());
            }
            let mut words = Vec::new();
            for token in fields[..total].as_chunks::<3>().0 {
                let (word, id) = token[0].rsplit_once('=').ok_or_else(malformed)?;
                let _: usize = id.parse().map_err(|_| malformed())?;
                let order: usize = token[1].parse().map_err(|_| malformed())?;
                let score: f64 = token[2].parse().map_err(|_| malformed())?;
                if word.is_empty() || order == 0 || !score.is_finite() {
                    return Err(malformed().into());
                }
                words.push(word.to_owned());
            }
            if words.pop().as_deref() != Some("</s>") || oov > words.len() {
                return Err(malformed().into());
            }
            cases.push(ReferenceCase {
                words,
                total: probability,
                oov,
            });
        } else if line.starts_with("Perplexity including OOVs:")
            || line.starts_with("Perplexity excluding OOVs:")
        {
            if fields.len() != 4 {
                return Err(malformed().into());
            }
            let value: f64 = fields[3].parse().map_err(|_| malformed())?;
            if !value.is_finite() || value < 0.0 {
                return Err(malformed().into());
            }
        } else if matches!(fields[0], "OOVs:" | "Tokens:") {
            if fields.len() != 2 || fields[1].parse::<usize>().is_err() {
                return Err(malformed().into());
            }
        } else {
            return Err(malformed().into());
        }
    }
    if cases.is_empty() {
        return Err("KenLM stdout contains no Total cases".into());
    }
    Ok(cases)
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = BufReader::new(File::open(path)?);
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn evaluate(model_path: &Path, input_path: &Path, reference_path: &Path) -> Result<Value> {
    let reference = parse_references(BufReader::new(File::open(reference_path)?))?;
    let model = Model::load(model_path)?; // Exactly one load for all sampled sentences.
    let mut count = 0usize;
    let mut maximum_error = 0.0f64;
    let mut mismatches = 0;
    let mut examples = Vec::new();
    let mut empty_cases = 0;
    let mut oov_cases = 0;
    for (index, line) in BufReader::new(File::open(input_path)?).lines().enumerate() {
        let line = line?;
        let words: Vec<_> = line.split_whitespace().collect();
        count += 1;
        if words.is_empty() {
            empty_cases += 1;
        }
        let Some(expected) = reference.get(index) else {
            continue;
        };
        if expected.oov > 0 {
            oov_cases += 1;
        }
        let actual = model.score_sentence(&words);
        let error = (actual - expected.total).abs();
        maximum_error = maximum_error.max(error);
        let aligned = words
            .iter()
            .copied()
            .eq(expected.words.iter().map(String::as_str));
        if !aligned || !actual.is_finite() || error > TOLERANCE {
            mismatches += 1;
            if examples.len() < 10 {
                examples.push(json!({"line":index+1,"input_matches_reference_tokens":aligned,"expected_log10":expected.total,"actual_log10":actual,"abs_error":error}));
            }
        }
    }
    let missing = count.abs_diff(reference.len());
    Ok(
        json!({"passed":mismatches==0 && missing==0 && count>0,"case_count":count,"reference_case_count":reference.len(),"compared_case_count":count.min(reference.len()),"unmatched_case_count":missing,"mismatch_count":mismatches,"max_abs_error":maximum_error,"empty_cases":empty_cases,"oov_cases":oov_cases,"mismatch_examples":examples,"model_load_count":1}),
    )
}

fn run(model: &Path, input: &Path, reference: &Path) -> Result<Value> {
    let hashes = [hash_file(model)?, hash_file(input)?, hash_file(reference)?];
    let mut report = match evaluate(model, input, reference) {
        Ok(report) => report,
        Err(error) => {
            json!({"passed":false,"error":error.to_string(),"case_count":0,"max_abs_error":null})
        }
    };
    report["schema_version"] = json!(1);
    report["score_log_base"] = json!(10);
    report["includes_bos_context"] = json!(true);
    report["includes_eos_score"] = json!(true);
    report["absolute_tolerance"] = json!(TOLERANCE);
    report["model"] = json!({"path":model,"sha256":hashes[0]});
    report["input"] = json!({"path":input,"sha256":hashes[1]});
    report["reference"] =
        json!({"path":reference,"sha256":hashes[2],"format":"raw pinned KenLM query stdout"});
    let unchanged = [hash_file(model)?, hash_file(input)?, hash_file(reference)?] == hashes;
    report["files_unchanged_during_run"] = json!(unchanged);
    if !unchanged {
        report["passed"] = json!(false);
        report["error"] = json!("an input file changed during differential evaluation");
    }
    Ok(report)
}

fn required_path(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("required env {name} is missing"))
}

#[test]
#[ignore = "consumes existing immutable model and stored book.local KenLM output; set LP_DIFFERENTIAL_MODEL/INPUT/REFERENCE"]
fn stored_kenlm_differential() {
    let report = run(
        &required_path("LP_DIFFERENTIAL_MODEL"),
        &required_path("LP_DIFFERENTIAL_INPUT"),
        &required_path("LP_DIFFERENTIAL_REFERENCE"),
    )
    .unwrap();
    let bytes = serde_json::to_vec_pretty(&report).unwrap();
    if let Some(path) = std::env::var_os("LP_DIFFERENTIAL_REPORT") {
        // Never overwrite an existing report or an input/model supplied by the caller.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        file.write_all(&bytes).unwrap();
        file.write_all(b"\n").unwrap();
        file.sync_all().unwrap();
    }
    println!("{}", String::from_utf8(bytes).unwrap());
    assert_eq!(report["passed"], true, "stored KenLM differential failed");
}

#[test]
#[ignore = "optional local setup only: compile actual stored tiny ARPA to a NEW temporary LP_DIFFERENTIAL_MODEL path; never trains"]
fn compile_stored_tiny_package_for_harness() {
    let path = required_path("LP_DIFFERENTIAL_MODEL");
    assert!(!path.exists(), "setup must not overwrite a model");
    let arpa = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/tiny/model.arpa");
    Model::compile_arpa(&arpa,&path,&predictor_core::PackageMetadata{provenance:json!({"fixture":"actual book.local tiny ARPA","arpa_sha256":hash_file(&arpa).unwrap()}),..Default::default()}).unwrap();
    println!(
        "compiled stored actual tiny ARPA (no training): model={} sha256={}",
        path.display(),
        hash_file(&path).unwrap()
    );
}

#[test]
#[ignore = "negative controls consuming the same stored LP_DIFFERENTIAL_MODEL/INPUT/REFERENCE; no trainer/reference invocation"]
fn stored_harness_negative_controls() {
    let model = required_path("LP_DIFFERENTIAL_MODEL");
    let input = required_path("LP_DIFFERENTIAL_INPUT");
    let reference = required_path("LP_DIFFERENTIAL_REFERENCE");
    let directory = tempfile::tempdir().unwrap();
    let input_text = std::fs::read_to_string(&input).unwrap();
    let raw = std::fs::read_to_string(&reference).unwrap();

    let wrong_count = directory.path().join("extra-cases.txt");
    std::fs::write(&wrong_count, format!("{input_text}\n\n")).unwrap();
    let report = run(&model, &wrong_count, &reference).unwrap();
    assert_eq!(report["passed"], false);
    assert!(report["unmatched_case_count"].as_u64().unwrap() > 0);

    let wrong_total = directory.path().join("wrong-total.stdout");
    let start = raw.find("Total: ").unwrap() + "Total: ".len();
    let end = start + raw[start..].find(char::is_whitespace).unwrap();
    let mut changed = raw.clone();
    changed.replace_range(start..end, "-999");
    std::fs::write(&wrong_total, changed).unwrap();
    let report = run(&model, &input, &wrong_total).unwrap();
    assert_eq!(report["passed"], false);
    assert!(report["mismatch_count"].as_u64().unwrap() > 0);

    let malformed = directory.path().join("malformed-total.stdout");
    let mut changed = raw;
    changed.replace_range(start..end, "NaN");
    std::fs::write(&malformed, changed).unwrap();
    let report = run(&model, &input, &malformed).unwrap();
    assert_eq!(report["passed"], false);
    assert!(report["error"]
        .as_str()
        .unwrap()
        .contains("malformed KenLM"));

    let misaligned = directory.path().join("misaligned.txt");
    let first = input_text.find(char::is_whitespace).unwrap();
    std::fs::write(&misaligned, format!("DIFFERENT{}", &input_text[first..])).unwrap();
    let report = run(&model, &misaligned, &reference).unwrap();
    assert_eq!(report["passed"], false);
    assert_eq!(
        report["mismatch_examples"][0]["input_matches_reference_tokens"],
        false
    );
    println!("negative controls passed: unequal case count, incorrect finite Total, malformed NaN Total, mismatched input/reference token labels");
}
