use predictor_core::{PersonalModel, Predictor, Request, Result};
use std::{hint::black_box, time::Instant};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let load = Instant::now();
    let predictor = Predictor::load(&args[1])?;
    let load_ns = load.elapsed().as_nanos();
    let personal = args.get(4).map(PersonalModel::load).transpose()?;
    let count: usize = args[3].parse()?;
    let first = Instant::now();
    black_box(predictor.complete(Request::new(&args[2]), personal.as_ref())?);
    eprintln!(
        "{}",
        serde_json::json!({"load_ns":load_ns,"first_query_ns":first.elapsed().as_nanos(),"cold_policy":"new process; filesystem cache not flushed; validation warms mmap"})
    );
    for _ in 0..10 {
        black_box(predictor.complete(Request::new(&args[2]), personal.as_ref())?);
    }
    for i in 0..count {
        let start = Instant::now();
        let result = predictor.explain(Request::new(&args[2]), personal.as_ref())?;
        println!(
            "{}",
            serde_json::json!({"i":i,"ns":start.elapsed().as_nanos(),"pool":result.pool.len(),"truncated":result.truncated,"response":result.response})
        );
        black_box(result);
    }
    let rss = std::process::Command::new("ps")
        .args(["-p", &std::process::id().to_string(), "-o", "rss="])
        .output()?;
    if !rss.status.success() {
        return Err("RSS inspection failed".into());
    }
    eprintln!(
        "{}",
        serde_json::json!({"steady_rss_kib":std::str::from_utf8(&rss.stdout)?.trim().parse::<u64>()?})
    );
    Ok(())
}
