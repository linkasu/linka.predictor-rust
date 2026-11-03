"""Explicit bounded book.local Linux-container review/train entry point; no resume."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import resource
import signal
import stat
import subprocess
import sys

import model_recipe as recipe


MAX_LIMITS = {"memory_bytes": 6 * 1024**3, "cpus": 2, "tasks": 128}
SOURCE_ROOT = Path("/work/source")
RECEIPT_PATH = Path("/work/metadata/execution.json")


def host_guard():
    if platform.system() != "Linux" or os.environ.get("GITHUB_ACTIONS") == "true":
        raise ValueError("book execution requires Linux outside GitHub Actions")


def cgroup_limits(root=Path("/sys/fs/cgroup")):
    try:
        (root / "cgroup.controllers").read_text()
        memory = int((root / "memory.max").read_text().strip())
        quota, period = map(int, (root / "cpu.max").read_text().split())
        tasks = int((root / "pids.max").read_text().strip())
        if (not 0 < memory <= MAX_LIMITS["memory_bytes"] or not 0 < tasks <= MAX_LIMITS["tasks"]
                or period <= 0 or not 0 < quota <= MAX_LIMITS["cpus"] * period):
            raise ValueError("cgroup caps exceed book budget")
        return {"memory_bytes": memory, "cpus": quota / period, "tasks": tasks}
    except (OSError, ValueError) as error:
        raise ValueError("finite cgroup v2 memory/cpu/tasks caps are required") from error


def validate_receipt(receipt, job_id, attempt, image_digest, source_sha, limits):
    expected = {"schema": "linka-book-execution-v1", "host": "book.local",
                "colima_profile": "linka-model-400", "book_job_id": job_id, "attempt": attempt,
                "image_digest": image_digest, "source_sha": source_sha, "limits": limits}
    if (not isinstance(receipt, dict) or receipt != expected
            or type(receipt.get("attempt")) is not int or attempt <= 0
            or re.fullmatch(r"[0-9a-f]{32}", job_id) is None
            or re.fullmatch(r"sha256:[0-9a-f]{64}", image_digest) is None
            or re.fullmatch(r"[0-9a-f]{40}", source_sha) is None
            or not isinstance(receipt.get("limits"), dict)
            or any(type(receipt["limits"].get(key)) is not int for key in ("memory_bytes", "tasks"))
            or type(receipt["limits"].get("cpus")) not in (int, float)
            or set(receipt["limits"]) != set(MAX_LIMITS)):
        raise ValueError("exact reviewed book execution receipt required")


def read_receipt(path, pin):
    if (path != RECEIPT_PATH or path.is_relative_to(SOURCE_ROOT)
            or re.fullmatch(r"[0-9a-f]{64}", pin) is None
            or any(parent.is_symlink() for parent in (path, *path.parents))):
        raise ValueError("exact external receipt path and lowercase SHA256 pin required")
    try:
        before = path.lstat()
        if not stat.S_ISREG(before.st_mode) or before.st_mode & 0o222 or before.st_size > 65536:
            raise ValueError("receipt must be a read-only regular file of at most 64 KiB")
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, "rb") as stream:
            current = os.fstat(stream.fileno())
            if (not stat.S_ISREG(current.st_mode) or current.st_mode & 0o222
                    or (current.st_dev, current.st_ino) != (before.st_dev, before.st_ino)):
                raise ValueError("receipt changed while opening")
            content = stream.read(65537)
        if len(content) > 65536 or hashlib.sha256(content).hexdigest() != pin:
            raise ValueError("execution receipt SHA256/size mismatch")
        receipt = recipe.parse_json(content)
        canonical = (json.dumps(receipt, sort_keys=True, separators=(",", ":"),
                                ensure_ascii=True) + "\n").encode("utf-8")
        if content != canonical:
            raise ValueError("execution receipt bytes must use the canonical JSON plus newline format")
        return receipt
    except OSError as error:
        raise ValueError("cannot read exact execution receipt") from error


def tracked_file(path):
    if path.is_absolute() or ".." in path.parts or not path.parts:
        raise ValueError("inputs must be exact repository-relative tracked paths")
    if any(parent.is_symlink() for parent in (path, *path.parents)) or not path.is_file():
        raise ValueError("tracked input must be a regular non-symlink file")
    subprocess.run(["git", "ls-files", "--error-unmatch", "--", str(path)],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)


def record_completion(output, run_id):
    logs, bundle = output / "logs", output / "bundle"
    final_path = logs / "recipe.json"
    if final_path.is_symlink():
        raise ValueError("final recipe receipt must be a regular file")
    final = recipe.read_json(final_path)
    if (not isinstance(final, dict) or final.get("state") != "COMPLETED"
            or final.get("mode") != "train" or final.get("run_id") != run_id):
        raise ValueError("completed matching Book training recipe required")
    commands = {}
    allowed_t_failures = {"kenlm-pin", "kenlm-clean", "kenlm-configure", "kenlm-build", "mkn3", "compile-T"}
    for path in sorted(logs.glob("*.json")):
        if path.name == "recipe.json":
            continue
        if path.is_symlink() or not path.is_file():
            raise ValueError("command receipts must be regular files")
        command = recipe.read_json(path)
        if (not isinstance(command, dict) or command.get("child_reaped") is not True
                or type(command.get("code")) is not int
                or not (command.get("state") == "COMPLETED" and command["code"] == 0
                        or command.get("state") == "FAILED" and command["code"] > 0)):
            raise ValueError("all Book commands must have reconciled completion/failure receipts")
        if command["state"] == "FAILED":
            if path.stem not in allowed_t_failures:
                raise ValueError("only known optional T commands may fail")
            t_receipt = recipe.read_json(bundle / "T.receipt.json")
            if (not isinstance(t_receipt, dict) or t_receipt.get("state") != "FAILED"
                    or t_receipt.get("child_reaped") is not True
                    or type(t_receipt.get("code")) is not int or t_receipt["code"] <= 0):
                raise ValueError("failed optional command requires a reaped failed T candidate")
        commands[path.stem] = command
    required = {"build-preparer", "prepare", "build-cli", "compile-U"}
    if not required.issubset(commands) or any(commands[name]["state"] != "COMPLETED" for name in required):
        raise ValueError("required Book preparation/U command completion evidence missing")
    provenance_path = bundle / "provenance.json"
    provenance = recipe.read_json(provenance_path)
    if not isinstance(provenance, dict):
        raise ValueError("candidate provenance must be an object")
    provenance["book_completion"] = {"schema": "linka-book-completion-v1", "recipe": final, "commands": commands}
    recipe.write_json(provenance_path, provenance)


def run(args):
    host_guard()
    limits = cgroup_limits()
    if (Path.cwd() != SOURCE_ROOT or SOURCE_ROOT.is_symlink()
            or SOURCE_ROOT != Path(subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True).strip())):
        raise ValueError("run from the exact source checkout root")
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    if head != args.source_sha:
        raise ValueError("source SHA does not match checkout")
    subprocess.run(["git", "diff", "--exit-code", "HEAD"], stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL, check=True)
    paths = [args.manifest, args.exclusion_file, Path("scripts/book_model.py"), Path("scripts/model_recipe.py")]
    if args.mode == "train":
        if args.approval is None or args.review_report is None:
            raise ValueError("train requires explicit tracked approval and review report")
        paths += [args.approval, args.review_report]
    elif args.approval is not None or args.review_report is not None:
        raise ValueError("review does not accept approval/training inputs")
    for path in paths:
        tracked_file(path)
    receipt = read_receipt(args.runner_receipt, args.receipt_sha256)
    validate_receipt(receipt, args.job_id, args.attempt,
                     args.image_digest, args.source_sha, limits)
    if (not args.output.is_absolute() or args.output.resolve().is_relative_to(SOURCE_ROOT)
            or any(parent.is_symlink() for parent in (args.output, *args.output.parents))):
        raise ValueError("output must be a new absolute worker-local path outside source")
    # The primary's outer Docker timeout also bounds downloads and non-child work.
    resource.setrlimit(resource.RLIMIT_FSIZE, (1024**3, 1024**3))
    execution = {**receipt, "kind": "book-local-linux-container", "receipt_sha256": args.receipt_sha256}
    run_id = f"book:{args.job_id}:{args.attempt}"
    result = recipe._linux_recipe(args, run_id, execution)
    if args.mode == "train":
        record_completion(args.output, run_id)
    return result


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--mode", choices=("review", "train"), required=True)
    for name in ("manifest", "exclusion-file", "runner-receipt", "output"):
        result.add_argument(f"--{name}", type=Path, required=True)
    for name in ("approval", "review-report"):
        result.add_argument(f"--{name}", type=Path)
    for name in ("job-id", "image-digest", "source-sha", "receipt-sha256"):
        result.add_argument(f"--{name}", required=True)
    result.add_argument("--attempt", type=int, required=True)
    result.add_argument("--kenlm-source", type=Path, default=Path(".local/kenlm-source"))
    return result


def cancelled(_signal, _frame):
    recipe.CANCEL_REQUESTED = True
    raise InterruptedError("owned book job cancellation requested")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, cancelled)
    signal.signal(signal.SIGINT, cancelled)
    try:
        run(parser().parse_args())
    except Exception as error:
        print(f"book model did not complete: {type(error).__name__}; inspect sanitized receipts", file=sys.stderr)
        raise SystemExit(1)
