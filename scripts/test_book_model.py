"""Synthetic tests only; never downloads or prepares corpus content."""

import os
import bz2
import hashlib
import io
import json
from pathlib import Path
import tempfile
import subprocess
import sys
import time
import unittest
from unittest.mock import patch, Mock

import book_model as book
import model_recipe as recipe


def synthetic_credit(family="синтетическая canary sentence never export"):
    return {"source": "tatoeba", "family": family, "sentence_id": "123", "segment": 0,
            "author": None, "split": "train", "component": "a" * 64,
            "source_url": "https://tatoeba.org/en/sentences/show/123", "license": "CC0-1.0",
            "license_url": "https://creativecommons.org/publicdomain/zero/1.0/",
            "source_profile": "tatoeba-cc0",
            "source_sha256": "3b6645bbc18e4e3444ebc9195c4a6c41ff521a8bc4306fec8fcc217ae734db76",
            "modified": "2026-10-31 00:00:00"}


class BookTests(unittest.TestCase):
    def test_compressed_credits_export_metadata_not_sentence_text(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original, output = root / "attribution.jsonl", root / "attribution.jsonl.bz2"
            credit = synthetic_credit()
            source = {"profile": credit["source_profile"], "sha256": credit["source_sha256"],
                      "license": credit["license"], "license_url": credit["license_url"]}
            rows = [credit, {**credit, "modified": "", "split": "test"}]
            content = "".join(json.dumps(row, ensure_ascii=False) + "\n" for row in rows)
            original.write_text(content, encoding="utf-8")
            recipe.export_attribution(original, output, source)
            with bz2.open(output, "rt", encoding="utf-8") as archive:
                exported = archive.read()
            self.assertNotIn(credit["family"], exported)
            projected = [recipe.parse_json(line) for line in exported.splitlines()]
            for before, after in zip(rows, projected):
                self.assertNotIn("family", after)
                self.assertEqual(after, {**{key: value for key, value in before.items() if key != "family"},
                    "family_sha256": hashlib.sha256(before["family"].encode("utf-8")).hexdigest()})
            self.assertEqual(original.read_text(encoding="utf-8"), content)

    def test_credit_export_rejects_free_text_and_invalid_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original = root / "attribution.jsonl"
            credit = synthetic_credit()
            source = {"profile": credit["source_profile"], "sha256": credit["source_sha256"],
                      "license": credit["license"], "license_url": credit["license_url"]}
            invalid_changes = ({"text": "must not export"}, {"family_sha256": "b" * 64},
                {"family": None}, {"family": ""}, {"author": "a person"}, {"source": "other"},
                {"source_profile": "tatoeba-ccby"}, {"license": "CC-BY-2.0"},
                {"license_url": "https://example.invalid/"}, {"source_sha256": "b" * 64},
                {"source_url": "https://tatoeba.org/en/sentences/show/456"},
                {"sentence_id": "123 extra text"}, {"sentence_id": 123}, {"segment": True},
                {"segment": -1}, {"split": "free text"}, {"component": "A" * 64},
                {"modified": "a sentence"}, {"modified": "2026-02-30 00:00:00"},
                {"modified": "2026-10-31 00:00:00\ntext"}, {"modified": None})
            for index, changes in enumerate(invalid_changes):
                with self.subTest(changes=changes):
                    original.write_text(json.dumps({**credit, **changes}) + "\n", encoding="utf-8")
                    with self.assertRaises(ValueError):
                        recipe.export_attribution(original, root / f"bad-{index}.bz2", source)
            for index, changes in enumerate(({"profile": "tatoeba-ccby"}, {"license": "other"},
                                              {"license_url": "https://example.invalid/"})):
                original.write_text(json.dumps(credit) + "\n", encoding="utf-8")
                with self.assertRaises(ValueError):
                    recipe.export_attribution(original, root / f"bad-source-{index}.bz2", {**source, **changes})

    def test_worker_local_cache_requires_exact_safe_verified_file(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            cache_root = root / "source-cache"; cache_root.mkdir()
            content = b"synthetic compressed bytes only"
            sha = hashlib.sha256(content).hexdigest()
            manifest = {"source": {"bytes": len(content), "sha256": sha}}
            path = cache_root / f"{sha}.tsv.bz2"
            with patch.object(book, "SOURCE_CACHE_ROOT", cache_root):
                with self.assertRaises((OSError, ValueError)):
                    book.validate_source_cache(path, manifest)
                path.write_bytes(content); path.chmod(0o444)
                book.validate_source_cache(path, manifest)
                self.assertEqual(path.read_bytes(), content)
                for invalid in (cache_root / "other.tsv.bz2", root / path.name):
                    with self.assertRaises(ValueError):
                        book.validate_source_cache(invalid, manifest)
                path.chmod(0o644)
                with self.assertRaises(ValueError):
                    book.validate_source_cache(path, manifest)
                for bad_content in (content[:-1], content + b"x", b"x" * len(content), b"x" * 516149):
                    path.write_bytes(bad_content); path.chmod(0o444)
                    with self.assertRaises(ValueError):
                        book.validate_source_cache(path, manifest)
                    path.chmod(0o644)
                path.unlink()
                target = root / "target"; target.write_bytes(content); target.chmod(0o444)
                path.symlink_to(target)
                with self.assertRaises(ValueError):
                    book.validate_source_cache(path, manifest)
                path.unlink(); path.mkdir()
                with self.assertRaises(ValueError):
                    book.validate_source_cache(path, manifest)
                path.rmdir(); cache_root.rmdir()
                cache_root.symlink_to(root, target_is_directory=True)
                with self.assertRaises(ValueError):
                    book.validate_source_cache(path, manifest)

    def test_shared_cache_stream_has_same_predecode_size_hash_gate_as_network(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source = {"profile": "tatoeba-cc0", "bytes": 516148,
                      "sha256": "3b6645bbc18e4e3444ebc9195c4a6c41ff521a8bc4306fec8fcc217ae734db76",
                      "url": "https://downloads.tatoeba.org/exports/per_language/rus/rus_sentences_CC0.tsv.bz2"}
            manifest, exclusions, cache = root / "manifest.json", root / "index.json", root / "synthetic-cache"
            recipe.write_json(manifest, {"source": source, "kenlm": {"commit": recipe.KENLM_COMMIT}})
            recipe.write_json(exclusions, {"synthetic": True})
            args = Mock(mode="review", manifest=manifest, exclusion_file=exclusions)
            with self.assertRaises(ValueError):
                recipe._linux_recipe(args, "123:1", source_cache=cache)
            for use_cache in (False, True):
                for size in (16366, 516148, 516149):
                    args.output = root / f"output-{use_cache}-{size}"
                    content = b"x" * size
                    cache.write_bytes(content)
                    with (patch.object(recipe.subprocess, "run"),
                          patch.object(recipe, "preparation_code_hash", return_value="e" * 64),
                          patch.object(recipe, "urlopen", return_value=io.BytesIO(content)) as network,
                          patch.object(recipe, "run_command") as compute):
                        with self.assertRaises(ValueError):
                            recipe._linux_recipe(args, "book:" + "a" * 32 + ":1",
                                {"kind": "book-local-linux-container"}, source_cache=cache if use_cache else None)
                        compute.assert_not_called()
                        if use_cache:
                            network.assert_not_called()
                    self.assertFalse((args.output / "private/source.tsv.bz2").exists())
                    self.assertFalse((args.output / "bundle").exists())
                    self.assertEqual(recipe.read_json(args.output / "logs/recipe.json")["state"], "FAILED")

    def test_cgroup_caps(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            for name, value in {"cgroup.controllers": "cpu memory pids", "memory.max": "6442450944",
                                "cpu.max": "200000 100000", "pids.max": "128"}.items():
                (root / name).write_text(value)
            self.assertEqual(book.cgroup_limits(root), book.MAX_LIMITS)
            for name, value in (("memory.max", "max"), ("memory.max", "6442450945"),
                                ("cpu.max", "200001 100000"), ("cpu.max", "max 100000"),
                                ("pids.max", "129"), ("pids.max", "0")):
                old = (root / name).read_text()
                (root / name).write_text(value)
                with self.assertRaises(ValueError):
                    book.cgroup_limits(root)
                (root / name).write_text(old)
            (root / "cpu.max").unlink()
            with self.assertRaises(ValueError):
                book.cgroup_limits(root)

    def test_receipt_strict(self):
        receipt = {"schema": "linka-book-execution-v1", "host": "book.local",
                   "colima_profile": "linka-model-400", "book_job_id": "a" * 32,
                   "attempt": 1, "image_digest": "sha256:" + "b" * 64,
                   "source_sha": "c" * 40, "limits": book.MAX_LIMITS}
        book.validate_receipt(receipt, "a" * 32, 1, "sha256:" + "b" * 64, "c" * 40,
                              book.MAX_LIMITS)
        for changes in ({"extra": True}, {"host": "other"}, {"attempt": True},
                        {"image_digest": "latest"}, {"source_sha": "C" * 40},
                        {"book_job_id": "d" * 32}, {"limits": {**book.MAX_LIMITS, "tasks": 129}}):
            with self.assertRaises(ValueError):
                book.validate_receipt({**receipt, **changes}, "a" * 32, 1,
                                      "sha256:" + "b" * 64, "c" * 40, book.MAX_LIMITS)

    def test_approval_context(self):
        manifest = {"source": {"sha256": "a" * 64, "evidence_sha256": "b" * 64}}
        review = {"schema": "linka-model-review-v1", "state": "COMPLETED", "mode": "review",
                  "review_run_id": "book:" + "a" * 32 + ":1", "source_sha256": "a" * 64,
                  "exclusions_sha256": "c" * 64, "source_seal_sha256": "d" * 64,
                  "preparation_code_sha256": "e" * 64, "excluded_ids_sha256": "f" * 64,
                  "execution": {"kind": "book-local-linux-container"}}
        approval = {key: review[key] for key in ("review_run_id", "source_sha256", "exclusions_sha256",
                    "source_seal_sha256", "preparation_code_sha256", "excluded_ids_sha256")}
        approval.update(schema="linka-model-content-review-v1", status="approved-for-filtered-training-only",
                        execution_context="book-local-linux-container", evidence_sha256="b" * 64,
                        reviewer="synthetic", reference="synthetic-only", content_reviewed=True,
                        raw_blob_artifact_approved=False, review_report_sha256="1" * 64)
        def check(value, evidence=review, context="book-local-linux-container"):
            recipe.require_approval(manifest, value, evidence, "1" * 64, "c" * 64, "e" * 64, context)
        check(approval)
        for changes in ({"raw_blob_artifact_approved": True}, {"execution_context": "github-linux"},
                        {"review_run_id": "book:" + "a" * 32 + ":0"}, {"unknown": 1}):
            with self.assertRaises(ValueError):
                check({**approval, **changes})
        with self.assertRaises(ValueError):
            check(approval, context="github-linux")
        with self.assertRaises(ValueError):
            check(approval, {**review, "execution": {"kind": "github-linux"}})

    def test_operational_receipt_file(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source, metadata = root / "source", root / "metadata"
            source.mkdir(); metadata.mkdir()
            path = metadata / "execution.json"
            content = b'{"synthetic":true}\n'
            pin = hashlib.sha256(content).hexdigest()
            with patch.object(book, "SOURCE_ROOT", source), patch.object(book, "RECEIPT_PATH", path):
                path.write_bytes(content); path.chmod(0o444)
                self.assertEqual(book.read_receipt(path, pin), {"synthetic": True})
                for noncanonical in (b'{ "synthetic": true }\n', b'{"synthetic":true}',
                                     b'{"synthetic":true}\r\n'):
                    path.chmod(0o644); path.write_bytes(noncanonical); path.chmod(0o444)
                    with self.assertRaises(ValueError):
                        book.read_receipt(path, hashlib.sha256(noncanonical).hexdigest())
                path.chmod(0o644); path.write_bytes(content); path.chmod(0o444)
                for invalid_pin in ("a" * 64, pin.upper(), "not-a-hash"):
                    with self.assertRaises(ValueError):
                        book.read_receipt(path, invalid_pin)
                path.chmod(0o644)
                with self.assertRaises(ValueError):
                    book.read_receipt(path, pin)
                path.unlink()
                path.symlink_to(source / "missing")
                with self.assertRaises(ValueError):
                    book.read_receipt(path, pin)
                path.unlink(); path.mkdir()
                with self.assertRaises(ValueError):
                    book.read_receipt(path, pin)
                path.rmdir(); path.write_bytes(b" " * (65536 + 1)); path.chmod(0o444)
                with self.assertRaises(ValueError):
                    book.read_receipt(path, hashlib.sha256(path.read_bytes()).hexdigest())
                path.chmod(0o644); path.write_bytes(b'{"x":1,"x":2}'); path.chmod(0o444)
                with self.assertRaises(ValueError):
                    book.read_receipt(path, hashlib.sha256(path.read_bytes()).hexdigest())
                with self.assertRaises(ValueError):
                    book.read_receipt(source / "execution.json", pin)

    def test_receipt_parent_symlink(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            actual = root / "actual"; actual.mkdir()
            metadata = root / "metadata"; metadata.symlink_to(actual, target_is_directory=True)
            path = metadata / "execution.json"
            path.write_bytes(b"{}"); path.chmod(0o444)
            with patch.object(book, "SOURCE_ROOT", root / "source"), patch.object(book, "RECEIPT_PATH", path):
                with self.assertRaises(ValueError):
                    book.read_receipt(path, hashlib.sha256(b"{}").hexdigest())

    def test_parser_requires_explicit_mode_and_receipt_pin(self):
        argv = ["--mode", "review", "--manifest", "manifest.json", "--exclusion-file", "index.json",
                "--runner-receipt", "/work/metadata/execution.json", "--output", "/work/job/output",
                "--job-id", "a" * 32, "--attempt", "1", "--image-digest", "sha256:" + "b" * 64,
                "--source-sha", "c" * 40, "--receipt-sha256", "d" * 64]
        self.assertEqual(book.parser().parse_args(argv).receipt_sha256, "d" * 64)
        for remove in ("--mode", "--receipt-sha256"):
            invalid = argv.copy(); index = invalid.index(remove); del invalid[index:index + 2]
            with patch("sys.stderr"), self.assertRaises(SystemExit):
                book.parser().parse_args(invalid)

    def test_entry_point_passes_full_pinned_execution(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source = root / "source"; source.mkdir()
            metadata = root / "metadata"; metadata.mkdir()
            path = metadata / "execution.json"
            receipt = {"schema": "linka-book-execution-v1", "host": "book.local",
                       "colima_profile": "linka-model-400", "book_job_id": "a" * 32,
                       "attempt": 1, "image_digest": "sha256:" + "b" * 64,
                       "source_sha": "c" * 40, "limits": book.MAX_LIMITS}
            path.write_bytes((json.dumps(receipt, sort_keys=True, separators=(",", ":"),
                                        ensure_ascii=True) + "\n").encode("utf-8")); path.chmod(0o444)
            pin = hashlib.sha256(path.read_bytes()).hexdigest()
            args = book.parser().parse_args([
                "--mode", "review", "--manifest", "manifest.json", "--exclusion-file", "index.json",
                "--runner-receipt", str(path), "--output", str(root / "output"), "--job-id", "a" * 32,
                "--attempt", "1", "--image-digest", "sha256:" + "b" * 64,
                "--source-sha", "c" * 40, "--receipt-sha256", pin])
            with (patch.object(book, "SOURCE_ROOT", source), patch.object(book, "RECEIPT_PATH", path),
                  patch.object(book, "host_guard"), patch.object(book, "cgroup_limits", return_value=book.MAX_LIMITS),
                  patch.object(Path, "cwd", return_value=source), patch.object(book, "tracked_file") as tracked,
                  patch.object(book.subprocess, "check_output", side_effect=lambda argv, **kw: str(source) if "--show-toplevel" in argv else "c" * 40),
                  patch.object(book.subprocess, "run") as git_run, patch.object(book.resource, "setrlimit"),
                  patch.object(recipe, "_linux_recipe") as shared,
                  patch.object(book, "record_completion") as completion):
                book.run(args)
                execution = {**receipt, "kind": "book-local-linux-container", "receipt_sha256": pin}
                shared.assert_called_once_with(args, "book:" + "a" * 32 + ":1", execution, source_cache=None)
                completion.assert_not_called()
                self.assertNotIn(path, [call.args[0] for call in tracked.call_args_list])
                git_run.assert_called_once()
                shared.reset_mock()
                args.source_cache = Path("/work/source-cache") / (("f" * 64) + ".tsv.bz2")
                manifest = {"source": {"sha256": "f" * 64, "bytes": 10}}
                with (patch.object(recipe, "read_json", return_value=manifest),
                      patch.object(book, "validate_source_cache") as cache_gate,
                      patch.object(book, "read_receipt", return_value=receipt)):
                    book.run(args)
                    cache_gate.assert_called_once_with(args.source_cache, manifest)
                    shared.assert_called_once_with(args, "book:" + "a" * 32 + ":1", execution,
                                                   source_cache=args.source_cache)
                    shared.reset_mock()
                    cache_gate.side_effect = ValueError("synthetic invalid cache")
                    with self.assertRaises(ValueError):
                        book.run(args)
                    shared.assert_not_called()
                args.source_cache = None
                args.mode = "train"
                with self.assertRaises(ValueError):
                    book.run(args)
                shared.assert_not_called()
                args.approval = Path("approval.json"); args.review_report = Path("review.json")
                book.run(args)
                shared.assert_called_once_with(args, "book:" + "a" * 32 + ":1", execution, source_cache=None)
                completion.assert_called_once_with(args.output, "book:" + "a" * 32 + ":1")
                completion.reset_mock(); shared.reset_mock()
                shared.side_effect = InterruptedError("synthetic cancellation")
                with self.assertRaises(InterruptedError):
                    book.run(args)
                completion.assert_not_called()
                shared.side_effect = None
                shared.reset_mock()
                git_run.side_effect = subprocess.CalledProcessError(1, "git diff")
                with self.assertRaises(subprocess.CalledProcessError):
                    book.run(args)
                shared.assert_not_called()

    def test_completion_requires_final_job_and_command_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            logs, bundle = output / "logs", output / "bundle"
            logs.mkdir(); bundle.mkdir()
            run_id = "book:" + "a" * 32 + ":1"
            final = {"state": "COMPLETED", "mode": "train", "run_id": run_id}
            command = {"state": "COMPLETED", "child_reaped": True, "code": 0,
                       "argv": ["synthetic-command", "/worker/path"], "elapsed_seconds": 1}
            names = ("build-preparer", "prepare", "build-cli", "compile-U")
            recipe.write_json(logs / "recipe.json", final)
            for name in names:
                recipe.write_json(logs / f"{name}.json", command)
            recipe.write_json(bundle / "provenance.json", {"files_sha256": {"U.lpk": "a" * 64}})
            for invalid in (None, [], {**final, "state": "RUNNING"}, {**final, "state": "CANCELLED"},
                            {**final, "state": "UNRECONCILED"}, {**final, "mode": "review"},
                            {**final, "run_id": "book:" + "b" * 32 + ":1"}):
                if invalid is None:
                    (logs / "recipe.json").unlink()
                else:
                    recipe.write_json(logs / "recipe.json", invalid)
                with self.assertRaises((OSError, ValueError)):
                    book.record_completion(output, run_id)
                self.assertNotIn("book_completion", recipe.read_json(bundle / "provenance.json"))
            recipe.write_json(logs / "recipe.json", final)
            for invalid in (None, [], {**command, "state": "CANCELLED"},
                            {**command, "state": "RUNNING"}, {**command, "state": "UNRECONCILED"},
                            {**command, "child_reaped": False}, {**command, "code": True},
                            {**command, "code": 1}, {**command, "state": "FAILED", "code": 1}):
                if invalid is None:
                    (logs / "compile-U.json").unlink()
                else:
                    recipe.write_json(logs / "compile-U.json", invalid)
                with self.assertRaises(ValueError):
                    book.record_completion(output, run_id)
                self.assertNotIn("book_completion", recipe.read_json(bundle / "provenance.json"))
            recipe.write_json(logs / "compile-U.json", command)
            (logs / "private.stderr").write_text("SYNTHETIC-NOT-FOR-PROVENANCE")
            book.record_completion(output, run_id)
            provenance = recipe.read_json(bundle / "provenance.json")
            self.assertEqual(provenance["book_completion"], {"schema": "linka-book-completion-v1",
                "recipe": final, "commands": {name: command for name in names}})
            self.assertEqual(provenance["files_sha256"], {"U.lpk": "a" * 64})

    def test_completion_optional_t_failure_requires_known_reaped_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            logs, bundle = output / "logs", output / "bundle"
            logs.mkdir(); bundle.mkdir()
            run_id = "book:" + "a" * 32 + ":1"
            recipe.write_json(logs / "recipe.json", {"state": "COMPLETED", "mode": "train", "run_id": run_id})
            completed = {"state": "COMPLETED", "child_reaped": True, "code": 0}
            failed = {"state": "FAILED", "child_reaped": True, "code": 7}
            for name in ("build-preparer", "prepare", "build-cli", "compile-U"):
                recipe.write_json(logs / f"{name}.json", completed)
            for name in ("kenlm-pin", "kenlm-clean", "kenlm-configure", "kenlm-build", "mkn3", "compile-T"):
                path = logs / f"{name}.json"
                recipe.write_json(bundle / "provenance.json", {})
                recipe.write_json(path, failed)
                for invalid in (None, completed, {**failed, "child_reaped": False}, {**failed, "code": True}):
                    if invalid is None:
                        (bundle / "T.receipt.json").unlink(missing_ok=True)
                    else:
                        recipe.write_json(bundle / "T.receipt.json", invalid)
                    with self.assertRaises((OSError, ValueError)):
                        book.record_completion(output, run_id)
                    self.assertNotIn("book_completion", recipe.read_json(bundle / "provenance.json"))
                recipe.write_json(bundle / "T.receipt.json", failed)
                for invalid in ({**failed, "code": 0}, {**failed, "code": -9},
                                {**failed, "child_reaped": False}, {**failed, "code": True}):
                    recipe.write_json(path, invalid)
                    with self.assertRaises(ValueError):
                        book.record_completion(output, run_id)
                recipe.write_json(path, failed)
                book.record_completion(output, run_id)
                self.assertEqual(recipe.read_json(bundle / "provenance.json")["book_completion"]["commands"][name], failed)
                path.unlink()
            recipe.write_json(bundle / "provenance.json", {})
            recipe.write_json(logs / "unknown-T-step.json", failed)
            with self.assertRaises(ValueError):
                book.record_completion(output, run_id)
            self.assertNotIn("book_completion", recipe.read_json(bundle / "provenance.json"))

    def test_tracked_input_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            path = root / "input.json"; path.write_text("{}")
            # Relative path is required before Git is consulted.
            with patch.object(book.subprocess, "run") as git_run:
                for invalid in (path, Path("../input.json"), root / "missing"):
                    with self.assertRaises(ValueError):
                        book.tracked_file(invalid)
                git_run.assert_not_called()
            with (patch.object(Path, "is_file", return_value=True), patch.object(Path, "is_symlink", return_value=False),
                  patch.object(book.subprocess, "run") as git_run):
                book.tracked_file(Path("provenance/synthetic.json"))
                self.assertEqual(git_run.call_args.args[0], ["git", "ls-files", "--error-unmatch", "--", "provenance/synthetic.json"])
                git_run.side_effect = subprocess.CalledProcessError(1, "git ls-files")
                with self.assertRaises(subprocess.CalledProcessError):
                    book.tracked_file(Path("untracked.json"))

    def test_book_commands_do_not_record_corpus_logs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            recipe.run_command(root, "synthetic", [sys.executable, "-c",
                "import sys; print('synthetic corpus'); print('synthetic corpus', file=sys.stderr)"],
                private_logs=False, deadline=time.monotonic() + 10)
            self.assertFalse((root / "synthetic.stdout").exists())
            self.assertFalse((root / "synthetic.stderr").exists())
            self.assertIsNone(recipe.read_json(root / "synthetic.json")["stderr_sha256"])
            with self.assertRaises(subprocess.TimeoutExpired):
                recipe.run_command(root, "timeout", [sys.executable, "-c", "while True: pass"],
                                   private_logs=False, deadline=time.monotonic())
            self.assertEqual(recipe.read_json(root / "timeout.json")["state"], "UNRECONCILED")

    def test_ci_wrapper_guard_and_legacy_self_test(self):
        with patch.object(recipe.platform, "system", return_value="Darwin"), patch.dict(os.environ, GITHUB_ACTIONS="true"):
            with self.assertRaises(ValueError):
                recipe.recipe(Mock())
        with (patch.object(recipe.platform, "system", return_value="Linux"),
              patch.dict(os.environ, GITHUB_ACTIONS="true", GITHUB_RUN_ID="123", GITHUB_RUN_ATTEMPT="1"),
              patch.object(recipe, "_linux_recipe") as shared):
            args = Mock(); recipe.recipe(args)
            shared.assert_called_once_with(args, "123:1")

    def test_shared_review_and_train_keep_execution_and_failure_contract(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source = {"profile": "tatoeba-cc0", "bytes": 516148,
                      "license": "CC0-1.0", "license_url": "https://creativecommons.org/publicdomain/zero/1.0/",
                      "sha256": "3b6645bbc18e4e3444ebc9195c4a6c41ff521a8bc4306fec8fcc217ae734db76",
                      "url": "https://downloads.tatoeba.org/exports/per_language/rus/rus_sentences_CC0.tsv.bz2"}
            manifest, exclusions = root / "manifest.json", root / "index.json"
            recipe.write_json(manifest, {"source": source, "kenlm": {"commit": recipe.KENLM_COMMIT}})
            recipe.write_json(exclusions, {"synthetic": True})
            args = Mock(mode="review", manifest=manifest, exclusion_file=exclusions, output=root / "output")
            execution = {"kind": "book-local-linux-container", "receipt_sha256": "a" * 64,
                         "schema": "linka-book-execution-v1", "host": "book.local",
                         "colima_profile": "linka-model-400", "book_job_id": "b" * 32,
                         "attempt": 1, "image_digest": "sha256:" + "c" * 64,
                         "source_sha": "d" * 40, "limits": book.MAX_LIMITS}
            calls = []
            def command(logs, name, argv, **kwargs):
                calls.append((name, kwargs))
                if name == "prepare":
                    prepared = args.output / "private/prepared"; prepared.mkdir()
                    recipe.write_json(prepared / "report.json", {
                        "grouping_version": "duplicate-original-components-v1", "split_overlap": 0,
                        "component_overlap": 0, "original_overlap": 0, "source": source,
                        "independent_groups": {"train": 1, "dev": 1},
                        "source_filter": {"exclusion_index_sha256": recipe.digest(exclusions),
                                          "source_seal_sha256": "f" * 64, "excluded_ids": {}}})
                    if args.mode == "train":
                        for name, content in {"train.txt": "synthetic\n", "frequencies.tsv": "synthetic\t1\n",
                                              "train.groups.tsv": "synthetic\n", "dev.groups.tsv": "synthetic\n",
                                              "dev.txt": "synthetic\n",
                                              "attribution.jsonl": json.dumps(synthetic_credit("synthetic")) + "\n"}.items():
                            (prepared / name).write_text(content)
                elif name == "compile-U":
                    (args.output / "private/U.lpk").write_bytes(b"synthetic-model-only")
                elif name == "dev-U":
                    recipe.write_json(args.output / "private/U.dev.json", {"synthetic": True})
                elif name == "kenlm-pin":
                    raise recipe.CandidateFailure(7, "synthetic-optional-T-failure")
            original_digest = recipe.digest
            cache = root / "synthetic-cache.tsv.bz2"
            cache.write_bytes(b"x" * source["bytes"])
            def synthetic_digest(path):
                if path.name == "source.tsv.part":
                    return source["sha256"]
                return "1" * 64 if path.name in {"prepare_source", "lp"} else original_digest(path)
            with (patch.object(recipe.subprocess, "run"),
                  patch.object(recipe.subprocess, "check_output", return_value="synthetic-tool"),
                  patch.object(recipe, "preparation_code_hash", return_value="e" * 64),
                  patch.object(recipe, "urlopen") as network,
                  patch.object(recipe, "digest", side_effect=synthetic_digest),
                  patch.object(recipe, "run_command", side_effect=command),
                  patch.object(recipe, "archive_approved_source") as archive):
                recipe._linux_recipe(args, "book:" + "b" * 32 + ":1", execution, source_cache=cache)
                network.assert_not_called()
                archive.assert_not_called()
            review = recipe.read_json(args.output / "review/review.json")
            self.assertEqual(review["execution"], execution)
            self.assertEqual(review["source_transport"], "verified-worker-local-cache")
            self.assertNotIn("context", review)
            self.assertFalse(review["content_reviewed"])
            self.assertEqual([name for name, _ in calls], ["build-preparer", "prepare"])
            self.assertTrue(all(kwargs["private_logs"] is False for _, kwargs in calls))
            self.assertFalse((args.output / "bundle").exists())
            self.assertEqual(recipe.read_json(args.output / "logs/recipe.json")["state"], "COMPLETED")
            # No actual approval is produced: bypass its separately tested parser with synthetic minimal metadata.
            args.mode = "train"; args.output = root / "train-output"
            args.approval = root / "synthetic-approval.json"
            args.review_report = root / "output/review/review.json"
            recipe.write_json(args.approval, {"raw_blob_artifact_approved": False,
                "excluded_ids_sha256": recipe.canonical_hash({}), "source_seal_sha256": "f" * 64})
            execution = {**execution, "attempt": 2}
            with (patch.object(recipe.subprocess, "run"),
                  patch.object(recipe.subprocess, "check_output", return_value="synthetic-tool"),
                  patch.object(recipe, "preparation_code_hash", return_value="e" * 64),
                  patch.object(recipe, "urlopen") as network,
                  patch.object(recipe, "digest", side_effect=synthetic_digest),
                  patch.object(recipe, "run_command", side_effect=command),
                  patch.object(recipe.platform, "platform", return_value="synthetic-linux"),
                  patch.object(recipe, "require_approval")):
                recipe._linux_recipe(args, "book:" + "b" * 32 + ":2", execution, source_cache=cache)
                network.assert_not_called()
            provenance = recipe.read_json(args.output / "bundle/provenance.json")
            self.assertEqual(provenance["schema"], "linka-model-candidates-v1")
            self.assertEqual(provenance["execution"], execution)
            self.assertEqual(provenance["source_transport"], "verified-worker-local-cache")
            self.assertNotIn("context", provenance)
            self.assertEqual(recipe.read_json(args.output / "bundle/T.receipt.json")["state"], "FAILED")
            self.assertTrue((args.output / "bundle/U.lpk").is_file())
            self.assertFalse((args.output / "bundle/T.lpk").exists())
            self.assertFalse((args.output / "approved-source").exists())
            with bz2.open(args.output / "bundle/attribution.jsonl.bz2", "rt", encoding="utf-8") as archive:
                exported_credit = recipe.parse_json(archive.readline())
            self.assertNotIn("family", exported_credit)
            self.assertEqual(exported_credit["family_sha256"], hashlib.sha256(b"synthetic").hexdigest())
            self.assertEqual(provenance["files_sha256"]["attribution.jsonl.bz2"],
                             original_digest(args.output / "bundle/attribution.jsonl.bz2"))

    def test_host_guard(self):
        with patch.object(book.platform, "system", return_value="Darwin"):
            with self.assertRaises(ValueError):
                book.host_guard()
        with patch.object(book.platform, "system", return_value="Linux"), patch.dict(os.environ, GITHUB_ACTIONS="true"):
            with self.assertRaises(ValueError):
                book.host_guard()

    def test_metadata_duplicate_keys(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "receipt.json"
            path.write_text('{"schema":1,"schema":2}')
            with self.assertRaises(ValueError):
                recipe.read_json(path)


if __name__ == "__main__":
    unittest.main()
