"""Manual Linux CI recipe. --self-test uses synthetic temporary files only."""

import argparse
import bz2
from collections import Counter
import hashlib
import json
import math
import os
import functools
from pathlib import Path
import platform
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from urllib.request import urlopen


KENLM_COMMIT = "4cb443e60b7bf2c0ddf3c745378f76cb59e254e5"
MODEL_LIMIT = 128 * 1024 * 1024
CANCEL_REQUESTED = False


class CandidateFailure(Exception):
    def __init__(self, code, log):
        self.code, self.log = code, str(log)
        super().__init__("candidate failed; consult owned diagnostic log")


def write_json(path, value):
    partial = path.with_suffix(path.suffix + ".part")
    with partial.open("w", encoding="utf-8") as output:
        json.dump(value, output, indent=2)
        output.flush()
        os.fsync(output.fileno())
    partial.replace(path)


def run_command(logs, name, command, stdin=None, stdout=None, *, deadline=None, private_logs=True):
    record = {"argv": [str(x) for x in command], "state": "RUNNING", "child_reaped": False}
    receipt, error, resources = [logs / (name + suffix) for suffix in (".json", ".stderr", ".resources")]
    write_json(receipt, record)
    started = time.monotonic()
    wrapped = platform.system() == "Linux"
    argv = (["/usr/bin/time", "-v", "-o", str(resources)] if wrapped else []) + command
    with (error.open("xb") if private_logs else open(os.devnull, "wb")) as stderr, (
            (stdout or logs / f"{name}.stdout").open("xb") if stdout or private_logs
            else open(os.devnull, "wb")) as output:
        process = subprocess.Popen(argv, stdin=stdin, stdout=output, stderr=stderr,
                                   start_new_session=True, env={**os.environ, "LC_ALL": "C"})
        record["pid"] = process.pid
        write_json(receipt, record)
        try:
            code = process.wait(timeout=max(0, deadline - time.monotonic()) if deadline else None)
        except BaseException:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
            record.update(state="CANCELLED" if CANCEL_REQUESTED else "UNRECONCILED",
                          code=process.returncode, elapsed_seconds=time.monotonic() - started)
            write_json(receipt, record)
            raise
    signalled = code < 0 or (wrapped and "Command terminated by signal" in resources.read_text())
    record.update(state="UNRECONCILED" if signalled else ("COMPLETED" if code == 0 else "FAILED"),
                  code=code, child_reaped=code >= 0, elapsed_seconds=time.monotonic() - started,
                  stderr_sha256=digest(error) if private_logs else None)
    write_json(receipt, record)
    if signalled:
        raise InterruptedError("signalled child cannot be accepted as an optional candidate failure")
    if code:
        raise CandidateFailure(code, error)
    return record


def attempt_candidate(name, bundle, operation):
    if name not in {"U", "T"}:
        raise ValueError("only required U and optional T candidates exist")
    receipt = bundle / f"{name}.receipt.json"
    write_json(receipt, {"state": "RUNNING", "child_reaped": False})
    try:
        model = operation()
        if not model.is_file() or not 0 < model.stat().st_size <= MODEL_LIMIT:
            raise CandidateFailure(1, "model-size-or-missing-output")
        target = bundle / f"{name}.lpk"
        shutil.copyfile(model, target.with_suffix(".part"))
        target.with_suffix(".part").replace(target)
        result = {"state": "COMPLETED", "child_reaped": True, "code": 0,
                  "model_sha256": digest(target), "bytes": target.stat().st_size}
    except CandidateFailure as error:
        result = {"state": "FAILED", "child_reaped": True, "code": error.code, "log": error.log,
                  "log_sha256": digest(Path(error.log)) if Path(error.log).is_file() else None}
        write_json(receipt, result)
        if name == "U":
            raise
        return result
    except BaseException:
        write_json(receipt, {"state": "CANCELLED" if CANCEL_REQUESTED else "UNRECONCILED",
                             "child_reaped": False})
        raise
    write_json(receipt, result)
    return result


def require_approval(manifest, approval, review, review_sha256, exclusions_sha256, code_sha256,
                     execution_context="github-linux"):
    source = manifest["source"]
    fields = {"schema", "status", "source_sha256", "evidence_sha256", "reviewer", "reference",
              "content_reviewed", "raw_blob_artifact_approved", "exclusions_sha256",
              "source_seal_sha256", "preparation_code_sha256", "excluded_ids_sha256",
              "review_run_id", "review_report_sha256"}
    legacy = approval.get("status") == "approved-for-ci-training-only"
    if not legacy:
        fields.add("execution_context")
    run_pattern = (r"[0-9]+:[0-9]+" if execution_context == "github-linux"
                   else r"book:[0-9a-f]{32}:[1-9][0-9]*")
    scope_valid = (execution_context == "github-linux" if legacy else
                   approval.get("status") == "approved-for-filtered-training-only"
                   and approval.get("execution_context") == execution_context)
    if execution_context == "book-local-linux-container":
        scope_valid = (scope_valid and approval.get("raw_blob_artifact_approved") is False
                       and isinstance(review.get("execution"), dict)
                       and review["execution"].get("kind") == execution_context)
    hashes = fields - {"schema", "status", "reviewer", "reference", "content_reviewed",
                        "execution_context",
                        "raw_blob_artifact_approved", "review_run_id"}
    if (set(approval) != fields or approval.get("schema") != "linka-model-content-review-v1"
            or execution_context not in {"github-linux", "book-local-linux-container"}
            or not scope_valid
            or approval.get("source_sha256") != source["sha256"]
            or approval.get("evidence_sha256") != source["evidence_sha256"]
            or approval.get("content_reviewed") is not True
            or type(approval.get("raw_blob_artifact_approved")) is not bool
            or any(not isinstance(approval.get(key), str) or len(approval[key]) != 64
                   or any(c not in "0123456789abcdef" for c in approval[key]) for key in hashes)
            or not isinstance(approval.get("review_run_id"), str)
            or re.fullmatch(run_pattern, approval["review_run_id"]) is None
            or review.get("schema") != "linka-model-review-v1" or review.get("state") != "COMPLETED"
            or review.get("mode") != "review" or review.get("source_sha256") != source["sha256"]
            or review.get("exclusions_sha256") != exclusions_sha256
            or review.get("preparation_code_sha256") != code_sha256
            or approval.get("review_report_sha256") != review_sha256
            or any(approval.get(key) != review.get(key) for key in (
                "review_run_id", "source_seal_sha256", "exclusions_sha256",
                "preparation_code_sha256", "excluded_ids_sha256"))
            or any(not isinstance(approval.get(key), str) or not approval[key].strip()
                   for key in ("reviewer", "reference"))):
        raise ValueError("separate source/exclusions/run/code-bound human content approval is required")


def archive_approved_source(raw, archive, approved, binding):
    if not approved:
        return
    archive.mkdir()
    shutil.copyfile(raw, archive / "source.tsv.bz2")
    write_json(archive / "approval-binding.json", binding)


def defer_core_dev(output):
    # The old phrase-mean diagnostic is not the canonical, separately frozen UI DEV gate.
    write_json(output, {"schema": "linka-core-dev-diagnostic-v1", "state": "NOT_RUN",
                        "canonical_ui_dev_required": True,
                        "reason": "UI_KSR_V1 DEV selection is required separately; no core diagnostic quality claim"})


def unigram_arpa(frequencies, train, output):
    counts = {}
    for line in frequencies.read_text(encoding="utf-8").splitlines():
        word, raw_count = line.split("\t")
        count = int(raw_count)
        if (not word or any(c.isspace() for c in word)
                or word in {"<s>", "</s>", "<unk>"} or word in counts
                or not 0 < count <= 2**32 - 1):
            raise ValueError("invalid train-only frequency record")
        counts[word] = count
    observed = Counter()
    sentences = 0
    with train.open(encoding="utf-8") as text:
        for line in text:
            observed.update(line.split())
            sentences += 1
    if not counts or sentences == 0 or counts != observed:
        raise ValueError("counts must exactly match nonempty unique train families")
    # One explicit unknown pseudocount; EOS counts actual train sentences, BOS is not predicted.
    total = sum(counts.values()) + sentences + 1
    with output.open("x", encoding="utf-8") as out:
        out.write(f"\\data\\\nngram 1={len(counts) + 3}\n\n\\1-grams:\n")
        for word, count in sorted({**counts, "</s>": sentences, "<unk>": 1}.items()):
            out.write(f"{math.log10(count / total):.12g}\t{word}\n")
        out.write("-99\t<s>\n\n\\end\\\n")


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(65536), b""):
            result.update(block)
    return result.hexdigest()


def canonical_hash(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def parse_json(content):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate metadata JSON key")
            result[key] = value
        return result
    return json.loads(content, object_pairs_hook=unique)


def read_json(path):
    if path.stat().st_size > 16 * 1024**2:
        raise ValueError("metadata JSON exceeds 16 MiB")
    return parse_json(path.read_text(encoding="utf-8"))


def candidate_provenance(bundle, metadata):
    required = {"source_revision", "preparation_code_sha256", "source_sha256", "exclusions_sha256",
                "review_run_id", "train_sha256", "frequencies_sha256", "lp_sha256", "tool_versions",
                "arpa_sha256", "quality_approved", "redistribution_approved", "final_evaluated"}
    if not required.issubset(metadata) or "exclusion_sha256" in metadata or any(
            metadata[key] is not False for key in ("quality_approved", "redistribution_approved", "final_evaluated")):
        raise ValueError("invalid unpublished candidate provenance contract")
    if {p.name for p in bundle.glob("*.lpk")} - {"U.lpk", "T.lpk"}:
        raise ValueError("noncanonical model filename")
    files = {}
    for path in sorted(bundle.iterdir()):
        if path.name == "provenance.json":
            continue  # The file map cannot recursively hash itself.
        if path.is_symlink() or not path.is_file():
            raise ValueError("candidate bundle must contain regular files only")
        files[path.name] = digest(path)
    completed = set()
    for name in ("U", "T"):
        receipt = read_json(bundle / f"{name}.receipt.json")
        model, dev = bundle / f"{name}.lpk", bundle / f"{name}.dev.json"
        if receipt.get("state") == "COMPLETED":
            if (type(receipt.get("code")) is not int or receipt["code"] != 0 or receipt.get("child_reaped") is not True
                    or not model.is_file() or not dev.is_file()
                    or type(receipt.get("bytes")) is not int
                    or not 0 < receipt["bytes"] <= MODEL_LIMIT or receipt["bytes"] != model.stat().st_size
                    or receipt.get("model_sha256") != files.get(model.name)):
                raise ValueError("completed candidate receipt/model/file-map mismatch")
            read_json(dev)
            completed.add(name)
        elif (name != "T" or receipt.get("state") != "FAILED"
              or receipt.get("child_reaped") is not True
              or type(receipt.get("code")) is not int or receipt["code"] == 0
              or model.exists() or dev.exists()):
            raise ValueError("U must complete; failed T cannot supply a model or DEV report")
    if set(metadata["arpa_sha256"]) != completed:
        raise ValueError("ARPA provenance must match completed candidates only")
    return {**metadata, "schema": "linka-model-candidates-v1",
            "source_manifest_sha256": files["source-manifest.json"], "files_sha256": files}


def preparation_code_hash():
    paths = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
             "crates/predictor-cli/Cargo.toml", "crates/predictor-cli/src/lib.rs", "crates/predictor-cli/src/prepare.rs",
             "crates/predictor-core/Cargo.toml", "crates/predictor-core/src/lib.rs",
             "crates/predictor-cli/src/prepare/exclusions.rs", "crates/predictor-cli/src/pipeline.rs",
             "crates/predictor-core/src/text.rs", "crates/predictor-cli/examples/prepare_source.rs",
             "crates/predictor-cli/examples/make_eval_exclusions.rs",
              "scripts/model_recipe.py", ".github/workflows/model-candidates.yml"]
    paths += [name for name in ("scripts/book_model.py",)
              if Path(name).is_file()]
    return canonical_hash({name: digest(Path(name)) for name in paths})


def recipe(args):
    if platform.system() != "Linux" or os.environ.get("GITHUB_ACTIONS") != "true":
        raise ValueError("full preparation/training is restricted to the manual Linux CI job")
    return _linux_recipe(args, os.environ["GITHUB_RUN_ID"] + ":" + os.environ["GITHUB_RUN_ATTEMPT"])


def _linux_recipe(args, run_id, execution=None, source_cache=None):
    """Shared implementation; entry points must validate their execution boundary first."""
    execution_context = execution["kind"] if execution else "github-linux"
    if source_cache is not None and execution_context != "book-local-linux-container":
        raise ValueError("verified worker-local source cache is restricted to Book execution")
    command = functools.partial(run_command, deadline=time.monotonic() + 45 * 60,
                                private_logs=execution is None)
    paths = [args.manifest, args.exclusion_file]
    if args.mode == "train":
        if args.approval is None or args.review_report is None:
            raise ValueError("train requires separate source_review and completed review_report")
        paths += [args.approval, args.review_report]
    for path in paths:
        subprocess.run(["git", "ls-files", "--error-unmatch", "--", str(path)],
                       stdout=subprocess.DEVNULL, check=True)
    manifest = read_json(args.manifest)
    source = manifest["source"]
    if (source["profile"] != "tatoeba-cc0" or source["bytes"] != 516148
            or source["sha256"] != "3b6645bbc18e4e3444ebc9195c4a6c41ff521a8bc4306fec8fcc217ae734db76"
            or source["url"] != "https://downloads.tatoeba.org/exports/per_language/rus/rus_sentences_CC0.tsv.bz2"):
        raise ValueError("only the primary-reviewed pinned CC0 source is admitted")
    if manifest["kenlm"]["commit"] != KENLM_COMMIT:
        raise ValueError("unreviewed KenLM pin")
    subprocess.run(["git", "diff", "--exit-code", "HEAD"], check=True)
    code_sha = preparation_code_hash()
    exclusions_sha = digest(args.exclusion_file)
    args.output.mkdir()  # New owned root only; no automatic resume/retry.
    logs = args.output / "logs"
    logs.mkdir()
    status = {"state": "RUNNING", "mode": args.mode, "run_id": run_id}
    write_json(logs / "recipe.json", status)
    try:
        if args.mode == "train":
            approval, review = read_json(args.approval), read_json(args.review_report)
            require_approval(manifest, approval, review, digest(args.review_report), exclusions_sha, code_sha,
                             execution_context)
        private = args.output / "private"
        private.mkdir(mode=0o700)
        raw = private / "source.tsv.bz2"
        partial = raw.with_suffix(".part")
        size = 0
        with (source_cache.open("rb") if source_cache is not None else
              urlopen(source["url"], timeout=60)) as response, partial.open("xb") as output:
            while True:
                block = response.read(min(65536, source["bytes"] + 1 - size))
                if not block:
                    break
                size += len(block)
                if size > source["bytes"]:
                    raise ValueError("source exceeds frozen byte count")
                output.write(block)
        if size != source["bytes"] or digest(partial) != source["sha256"]:
            raise ValueError("source hash/size mismatch before any decoding")
        partial.rename(raw)
        if args.mode == "train":
            archive_approved_source(raw, args.output / "approved-source",
                approval["raw_blob_artifact_approved"], {
                "state": "APPROVED_FOR_OWNED_CI_ARTIFACT", "source_sha256": source["sha256"],
                "review_run_id": review["review_run_id"], "review_report_sha256": digest(args.review_report),
                "approval_sha256": digest(args.approval), "archive_run_id": run_id})
        command(logs, "build-preparer", ["cargo", "build", "--locked", "--release", "-j", "2",
                                             "-p", "predictor-cli", "--example", "prepare_source"])
        prepared = private / "prepared"
        command(logs, "prepare", ["target/release/examples/prepare_source", args.mode,
                    str(args.manifest), str(raw), str(prepared), str(args.exclusion_file)])
        report = read_json(prepared / "report.json")
        if (report["grouping_version"] != "duplicate-original-components-v1"
                or any(report[k] != 0 for k in ("split_overlap", "component_overlap", "original_overlap"))):
            raise ValueError("grouped split overlap invariant failed")
        filtered = report["source_filter"]
        if filtered["exclusion_index_sha256"] != exclusions_sha or report["source"] != source:
            raise ValueError("prepared source/index binding changed during the run")
        excluded_sha = canonical_hash(filtered["excluded_ids"])
        tool_versions = {tool: subprocess.check_output([tool, "--version"], text=True)
                         for tool in ("rustc", "cargo")}
        if args.mode == "review":
            review_dir = args.output / "review"
            review_dir.mkdir()
            review = {
                "schema": "linka-model-review-v1", "state": "COMPLETED", "mode": "review",
                "review_run_id": run_id, "source_sha256": source["sha256"],
                "source_manifest_sha256": digest(args.manifest), "exclusions_sha256": exclusions_sha,
                "source_seal_sha256": filtered["source_seal_sha256"],
                "preparation_code_sha256": code_sha, "excluded_ids_sha256": excluded_sha,
                "source_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
                "preparer_sha256": digest(Path("target/release/examples/prepare_source")),
                "tool_versions": tool_versions,
                "content_reviewed": False, "raw_blob_uploaded": False,
                "statistics": report,
                "limitations": ["Heuristic identifiers only, not proof of perfect privacy",
                    "No semantic, clinical-record, named-person or translation-family detector",
                    "Ordinary needs/pain words are not banned; false positives and false negatives remain",
                    "Fixed token-shingle Jaccard is not semantic near-duplicate isolation",
                    "Source train/DEV shingle overlap is not measured; only exclusion-index matches are blocked",
                    "No source text, raw blob, ARPA, model, independent FINAL or automatic approval uploaded"],
            }
            if execution:
                review["execution"] = execution
            if source_cache is not None:
                review["source_transport"] = "verified-worker-local-cache"
            write_json(review_dir / "review.json", review)
        else:
            if (excluded_sha != approval["excluded_ids_sha256"]
                    or filtered["source_seal_sha256"] != approval["source_seal_sha256"]
                    or any(report["independent_groups"][k] == 0 for k in ("train", "dev"))):
                raise ValueError("review/preparation exclusions mismatch or empty train/DEV")
            bundle = args.output / "bundle"
            bundle.mkdir()
            command(logs, "build-cli", ["cargo", "build", "--locked", "--release", "-j", "2",
                                             "-p", "predictor-cli", "--bin", "lp"])
            lp = Path("target/release/lp").resolve()
            train, frequencies = prepared / "train.txt", prepared / "frequencies.tsv"
            arpas = {}

            def compile_dev(name, arpa):
                model = private / f"{name}.lpk"
                command(logs, f"compile-{name}", [str(lp), "compile", str(arpa), str(model), str(frequencies), "-"])
                if model.stat().st_size > MODEL_LIMIT:
                    raise CandidateFailure(1, "model-exceeds-128-MiB")
                defer_core_dev(private / f"{name}.dev.json")
                arpas[name] = digest(arpa)
                return model

            def build_u():
                arpa = private / "U.arpa"
                unigram_arpa(frequencies, train, arpa)
                return compile_dev("U", arpa)

            attempt_candidate("U", bundle, build_u)
            shutil.copyfile(private / "U.dev.json", bundle / "U.dev.json")

            def build_t():
                pin = private / "kenlm-pin.stdout"
                command(logs, "kenlm-pin", ["git", "-C", str(args.kenlm_source), "rev-parse", "HEAD"], stdout=pin)
                if pin.read_text().strip() != KENLM_COMMIT:
                    raise CandidateFailure(1, "kenlm-checkout-pin-mismatch")
                command(logs, "kenlm-clean", ["git", "-C", str(args.kenlm_source), "diff", "--exit-code", "HEAD"])
                command(logs, "kenlm-configure", ["cmake", "-S", str(args.kenlm_source), "-B", str(private / "kenlm"),
                                                     "-DCMAKE_BUILD_TYPE=Release", "-DCMAKE_POLICY_VERSION_MINIMUM=3.5"])
                command(logs, "kenlm-build", ["cmake", "--build", str(private / "kenlm"), "--target", "lmplz", "-j", "2"])
                arpa = private / "T.arpa"
                temporary = private / "sort"
                temporary.mkdir()
                with train.open("rb") as text:
                    command(logs, "mkn3", [str(private / "kenlm/bin/lmplz"), "-o", "3", "-S", "2G", "-T", str(temporary)],
                                stdin=text, stdout=arpa)
                return compile_dev("T", arpa)

            t_receipt = attempt_candidate("T", bundle, build_t)
            if t_receipt["state"] == "COMPLETED":
                shutil.copyfile(private / "T.dev.json", bundle / "T.dev.json")
            for name in ("report.json", "frequencies.tsv", "train.groups.tsv", "dev.groups.tsv"):
                shutil.copyfile(prepared / name, bundle / name)
            with (prepared / "attribution.jsonl").open("rb") as audit:
                with bz2.open(bundle / "attribution.jsonl.bz2", "xb") as credits:
                    shutil.copyfileobj(audit, credits, 65536)
            for original, name in ((args.manifest, "source-manifest.json"), (args.approval, "source-review.json"),
                                   (args.review_report, "review-report.json"),
                                   (Path("provenance/licenses/KenLM-LICENSE"), "KenLM-LICENSE")):
                shutil.copyfile(original, bundle / name)
            (bundle / "NOTICES.txt").write_text(
                f"Tatoeba Russian public sentence examples: {source['license']}\n{source['url']}\n{source['license_url']}\n"
                "Original IDs and available modified dates are retained in attribution.jsonl.bz2; absent CC0 authors are null.\n"
                "Modified: NFC/lowercase, segmentation, grouped deduplication, fixed hash/shingle and direct-identifier exclusions, "
                "train-only counts, required count unigram and optional external MKN3. No discount fallback.\n"
                "Candidate only; no redistribution, resource, AAC or FINAL approval.\n", encoding="utf-8")
            lmplz = private / "kenlm/bin/lmplz"
            write_json(bundle / "provenance.json", candidate_provenance(bundle, {
                "source_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
                "preparation_code_sha256": code_sha, "source_sha256": source["sha256"],
                "exclusions_sha256": exclusions_sha, "review_run_id": review["review_run_id"],
                "train_sha256": digest(train), "frequencies_sha256": digest(frequencies),
                "lp_sha256": digest(lp), "kenlm_commit": KENLM_COMMIT,
                "tool_versions": tool_versions,
                "lmplz_sha256": digest(lmplz) if lmplz.is_file() else None, "arpa_sha256": arpas,
                "python_version": sys.version, "platform": platform.platform(),
                "quality_approved": False, "redistribution_approved": False, "final_evaluated": False,
                "semantic_translation_isolation": False, **({"execution": execution} if execution else {}),
                **({"source_transport": "verified-worker-local-cache"} if source_cache is not None else {})}))
        for receipt in logs.glob("*.json"):
            if receipt.name != "recipe.json" and read_json(receipt)["state"] in {"RUNNING", "CANCELLED", "UNRECONCILED"}:
                raise InterruptedError("unreconciled command receipt prevents job success")
        status["state"] = "COMPLETED"
    except BaseException as error:
        status.update(state=("CANCELLED" if CANCEL_REQUESTED else "UNRECONCILED")
                      if isinstance(error, InterruptedError) else "FAILED", error_type=type(error).__name__)
        raise
    finally:
        write_json(logs / "recipe.json", status)


def self_test():
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        raw = root / "synthetic-source.tsv.bz2"
        raw.write_bytes(b"synthetic source only")
        archive = root / "approved-source"
        binding = {"state": "APPROVED_FOR_OWNED_CI_ARTIFACT"}
        archive_approved_source(raw, archive, False, binding)
        assert not archive.exists()
        archive_approved_source(raw, archive, True, binding)
        assert (archive / "source.tsv.bz2").read_bytes() == raw.read_bytes()
        assert read_json(archive / "approval-binding.json") == binding
        try:
            archive_approved_source(raw, archive, True, binding)
        except FileExistsError:
            pass
        else:
            raise AssertionError("existing source archive overwritten")
        bundle, logs = root / "bundle", root / "logs"
        bundle.mkdir(); logs.mkdir()
        def build_u():
            model = root / "synthetic-U.lpk"
            model.write_bytes(b"synthetic-receipt-test-only")
            return model
        attempt_candidate("U", bundle, build_u)
        def fail_t():
            run_command(logs, "synthetic-T", [sys.executable, "-c", "raise SystemExit(7)"])
        attempt_candidate("T", bundle, fail_t)
        assert (bundle / "U.lpk").exists()
        receipt = json.loads((bundle / "T.receipt.json").read_text())
        assert receipt["state"] == "FAILED" and receipt["code"] == 7 and receipt["child_reaped"]
        write_json(bundle / "U.dev.json", {"synthetic": True})
        deferred = root / "deferred-core-dev.json"
        defer_core_dev(deferred)
        diagnostic = read_json(deferred)
        assert diagnostic["state"] == "NOT_RUN" and diagnostic["canonical_ui_dev_required"] is True
        assert not ({"ksr", "quality_pass", "passed"} & set(diagnostic))
        write_json(bundle / "source-manifest.json", {"synthetic": True})
        metadata = {key: "a" * 64 for key in ("preparation_code_sha256", "source_sha256",
                    "exclusions_sha256", "train_sha256", "frequencies_sha256", "lp_sha256")}
        metadata.update(source_revision="a" * 40, review_run_id="123:1", tool_versions={},
                        arpa_sha256={"U": "b" * 64}, quality_approved=False,
                        redistribution_approved=False, final_evaluated=False)
        provenance = candidate_provenance(bundle, metadata)
        assert provenance["schema"] == "linka-model-candidates-v1"
        assert provenance["source_manifest_sha256"] == digest(bundle / "source-manifest.json")
        assert provenance["files_sha256"]["U.lpk"] == read_json(bundle / "U.receipt.json")["model_sha256"]
        assert "T.lpk" not in provenance["files_sha256"]
        u = read_json(bundle / "U.receipt.json")
        for invalid in ({**u, "model_sha256": "0" * 64}, {**u, "bytes": u["bytes"] + 1},
                        {**u, "state": "FAILED"}):
            write_json(bundle / "U.receipt.json", invalid)
            try:
                candidate_provenance(bundle, metadata)
            except ValueError:
                pass
            else:
                raise AssertionError("receipt/file-map mismatch accepted")
        write_json(bundle / "U.receipt.json", u)
        (bundle / "T.lpk").write_bytes(b"failed-T-must-not-qualify")
        try:
            candidate_provenance(bundle, metadata)
        except ValueError:
            pass
        else:
            raise AssertionError("failed T with model accepted")
        (bundle / "T.lpk").unlink()
        def interrupted():
            raise InterruptedError("synthetic cancellation")
        try:
            attempt_candidate("T", root, interrupted)
        except InterruptedError:
            pass
        else:
            raise AssertionError("optional T swallowed cancellation")
        required = root / "required-U-failure"
        required.mkdir()
        def fail_u():
            run_command(logs, "synthetic-U", [sys.executable, "-c", "raise SystemExit(9)"])
        try:
            attempt_candidate("U", required, fail_u)
        except CandidateFailure as error:
            assert error.code == 9 and not (required / "U.lpk").exists()
        else:
            raise AssertionError("required U failure was accepted")
        try:
            run_command(logs, "synthetic-signal", [sys.executable, "-c",
                "import os,signal; os.kill(os.getpid(), signal.SIGTERM)"])
        except InterruptedError:
            assert read_json(logs / "synthetic-signal.json")["state"] == "UNRECONCILED"
        else:
            raise AssertionError("signal was accepted as ordinary optional failure")
    manifest = {"source": {"sha256": "a" * 64, "evidence_sha256": "b" * 64}}
    review = {"schema": "linka-model-review-v1", "state": "COMPLETED", "mode": "review",
              "review_run_id": "123:1", "source_sha256": "a" * 64,
              "exclusions_sha256": "c" * 64, "source_seal_sha256": "d" * 64,
              "preparation_code_sha256": "e" * 64, "excluded_ids_sha256": "f" * 64}
    approval = {
        "schema": "linka-model-content-review-v1",
        "status": "approved-for-ci-training-only",
        "source_sha256": "a" * 64,
        "evidence_sha256": "b" * 64,
        "reviewer": "synthetic-test",
        "reference": "synthetic-test-only",
        "content_reviewed": True,
        "raw_blob_artifact_approved": True,
        "exclusions_sha256": "c" * 64,
        "source_seal_sha256": "d" * 64,
        "preparation_code_sha256": "e" * 64,
        "excluded_ids_sha256": "f" * 64,
        "review_run_id": "123:1",
        "review_report_sha256": "1" * 64,
    }
    require_approval(manifest, approval, review, "1" * 64, "c" * 64, "e" * 64)
    require_approval(manifest, {**approval, "raw_blob_artifact_approved": False}, review,
                     "1" * 64, "c" * 64, "e" * 64)
    for invalid in [{}, {**approval, "status": "pending"},
                    {**approval, "source_sha256": "c" * 64},
                    {**approval, "content_reviewed": False},
                    {**approval, "reference": None},
                    {**approval, "exclusions_sha256": "0" * 64},
                    {**approval, "review_run_id": "other-run"},
                    {**approval, "source_seal_sha256": "0" * 64},
                    {**approval, "raw_blob_artifact_approved": "true"}]:
        try:
            require_approval(manifest, invalid, review, "1" * 64, "c" * 64, "e" * 64)
        except ValueError:
            pass
        else:
            raise AssertionError("unapproved source accepted")
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        train, counts, arpa = [root / n for n in ("train", "counts", "model.arpa")]
        train.write_text("чай чай\nвода\n", encoding="utf-8")
        counts.write_text("вода\t1\nчай\t2\n", encoding="utf-8")
        unigram_arpa(counts, train, arpa)
        text = arpa.read_text(encoding="utf-8")
        assert "ngram 1=5" in text and "\\end\\" in text
        probabilities = [float(line.split()[0]) for line in text.splitlines()
                         if line and line[0] in "-0123456789"]
        assert math.isclose(sum(10 ** p for p in probabilities), 1, abs_tol=1e-8)
        counts.write_text("вода\t1\nчай\t3\n", encoding="utf-8")
        try:
            unigram_arpa(counts, train, root / "bad.arpa")
        except ValueError:
            pass
        else:
            raise AssertionError("non-train counts accepted")
    print("synthetic recipe self-test passed; no training")


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        self_test()
    else:
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument("--mode", choices=("review", "train"), default="review")
        for name in ("manifest", "exclusion-file", "output"):
            parser.add_argument(f"--{name}", required=True, type=Path)
        for name in ("approval", "review-report"):
            parser.add_argument(f"--{name}", type=Path)
        parser.add_argument("--kenlm-source", type=Path, default=Path(".local/kenlm-source"))
        def cancelled(_signal, _frame):
            global CANCEL_REQUESTED
            CANCEL_REQUESTED = True
            raise InterruptedError("owned CI run cancellation requested")
        signal.signal(signal.SIGTERM, cancelled)
        signal.signal(signal.SIGINT, cancelled)
        try:
            recipe(parser.parse_args())
        except Exception as error:
            print(f"model recipe did not complete: {type(error).__name__}; inspect sanitized receipts", file=sys.stderr)
            raise SystemExit(1)
