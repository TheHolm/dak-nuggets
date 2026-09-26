#!/usr/bin/env python3
"""Tests for fetch-freebsd-deps.py.

Run directly (`python3 scripts/test_fetch_freebsd_deps.py`) or via
`python3 -m unittest scripts/test_fetch_freebsd_deps.py`. Not wired into the
top-level Makefile: `scripts/` is shared CI tooling, not one of the
per-program `PROGRAMS` the root Makefile's `test` target recurses into (see
NOTES.md).

`fetch-freebsd-deps.py` has a hyphen in its name, so it cannot be `import`ed
normally; `_load_module()` below loads it by file path instead.
"""

import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
import urllib.request
from contextlib import redirect_stderr

HERE = os.path.dirname(os.path.abspath(__file__))


def _load_module():
    """Load fetch-freebsd-deps.py as a module despite its hyphenated name."""
    path = os.path.join(HERE, "fetch-freebsd-deps.py")
    spec = importlib.util.spec_from_file_location("fetch_freebsd_deps", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


fetch_freebsd_deps = _load_module()


class ResolveClosureTests(unittest.TestCase):
    """Tests for `resolve_closure`, in particular the `exclude` parameter.

    The graph used throughout:

        root-pkg      -> keep-pkg, excluded-pkg
        excluded-pkg  -> excl-only-pkg      (reachable only through excluded-pkg)
        other-root    -> keep-pkg           (a second, independent path to keep-pkg)
    """

    def setUp(self):
        self.packages = {
            "root-pkg": {"deps": {"keep-pkg": {}, "excluded-pkg": {}}},
            "excluded-pkg": {"deps": {"excl-only-pkg": {}}},
            "excl-only-pkg": {"deps": {}},
            "keep-pkg": {"deps": {}},
            "other-root": {"deps": {"keep-pkg": {}}},
        }

    def test_no_exclude_returns_full_closure(self):
        """With no `exclude`, every reachable package is returned exactly once."""
        names = fetch_freebsd_deps.resolve_closure(self.packages, ["root-pkg"])
        self.assertEqual(
            set(names), {"root-pkg", "keep-pkg", "excluded-pkg", "excl-only-pkg"}
        )
        self.assertEqual(len(names), len(set(names)), "no duplicates")

    def test_exclude_prunes_the_excluded_package_itself(self):
        """An excluded package never appears in the closure."""
        names = fetch_freebsd_deps.resolve_closure(
            self.packages, ["root-pkg"], exclude={"excluded-pkg"}
        )
        self.assertNotIn("excluded-pkg", names)

    def test_exclude_prunes_dependencies_only_reachable_through_it(self):
        """A dependency reachable *only* through an excluded package is also dropped."""
        names = fetch_freebsd_deps.resolve_closure(
            self.packages, ["root-pkg"], exclude={"excluded-pkg"}
        )
        self.assertNotIn("excl-only-pkg", names)
        self.assertEqual(set(names), {"root-pkg", "keep-pkg"})

    def test_exclude_keeps_dependency_reachable_via_another_path(self):
        """A dependency also reachable via a non-excluded root is still included.

        `other-root` depends on `keep-pkg` independently of `root-pkg`, so
        excluding `excluded-pkg` (root-pkg's own subtree) must not affect
        `keep-pkg`'s presence via the `other-root` path.
        """
        names = fetch_freebsd_deps.resolve_closure(
            self.packages, ["root-pkg", "other-root"], exclude={"excluded-pkg"}
        )
        self.assertEqual(set(names), {"root-pkg", "other-root", "keep-pkg"})

    def test_exclude_matching_nothing_is_a_harmless_no_op(self):
        """Excluding a name that never appears in the closure changes nothing."""
        names = fetch_freebsd_deps.resolve_closure(
            self.packages, ["root-pkg"], exclude={"not-in-graph-at-all"}
        )
        self.assertEqual(set(names), {"root-pkg", "keep-pkg", "excluded-pkg", "excl-only-pkg"})

    def test_missing_root_raises(self):
        """A root missing from the repository is a hard error, not a skip."""
        with self.assertRaises(SystemExit):
            fetch_freebsd_deps.resolve_closure(self.packages, ["does-not-exist"])

    def test_missing_transitive_dependency_warns_and_skips(self):
        """A missing *transitive* dependency is reported and skipped, not fatal."""
        packages = dict(self.packages)
        packages["root-pkg"] = {"deps": {"keep-pkg": {}, "renamed-away": {}}}
        stderr = io.StringIO()
        with redirect_stderr(stderr):
            names = fetch_freebsd_deps.resolve_closure(packages, ["root-pkg"])
        self.assertNotIn("renamed-away", names)
        self.assertIn("renamed-away", stderr.getvalue())

    def test_excluding_a_root_drops_it_without_raising(self):
        """Excluding a root is treated the same as excluding anything else.

        This is not expected to be used in practice (the current callers only
        exclude transitive dependencies), but it must not raise the "missing
        from repository" error that an *unintentionally* missing root would.
        """
        names = fetch_freebsd_deps.resolve_closure(
            self.packages, ["root-pkg"], exclude={"root-pkg"}
        )
        self.assertEqual(names, [])


class ParsePkgConfigModuleTests(unittest.TestCase):
    """Tests for `parse_pkg_config_module`."""

    def test_valid_mapping(self):
        self.assertEqual(
            fetch_freebsd_deps.parse_pkg_config_module("evolution-data-server=libecal-2.0"),
            ("evolution-data-server", "libecal-2.0"),
        )

    def test_missing_equals_sign_is_rejected(self):
        import argparse

        with self.assertRaises(argparse.ArgumentTypeError):
            fetch_freebsd_deps.parse_pkg_config_module("no-equals-sign-here")


class ParseArgsTests(unittest.TestCase):
    """Tests for the `--exclude` CLI flag."""

    def test_exclude_is_repeatable_and_defaults_to_empty(self):
        args = fetch_freebsd_deps.parse_args(
            ["--sysroot", "/tmp/x", "--root", "r", "--exclude", "a", "--exclude", "b"]
        )
        self.assertEqual(args.exclude, ["a", "b"])

    def test_exclude_defaults_to_empty_list(self):
        args = fetch_freebsd_deps.parse_args(["--sysroot", "/tmp/x", "--root", "r"])
        self.assertEqual(args.exclude, [])


def _make_fake_pkg(repo_dir: str, name: str) -> str:
    """Write a minimal real `.pkg` (zstd tar) containing one marker file.

    Returns the manifest `path` (relative to `repo_dir`) for this package.
    """
    stage = os.path.join(repo_dir, f"stage-{name}")
    marker_dir = os.path.join(stage, "usr", "local", "share", "fetch-freebsd-deps-test")
    os.makedirs(marker_dir, exist_ok=True)
    with open(os.path.join(marker_dir, name), "w", encoding="utf-8") as f:
        f.write(f"marker for {name}\n")

    rel_path = f"All/{name}-1.pkg"
    abs_path = os.path.join(repo_dir, rel_path)
    os.makedirs(os.path.dirname(abs_path), exist_ok=True)
    subprocess.run(
        [
            "tar", "--zstd", "-cf", abs_path, "-C", stage,
            "usr/local/share/fetch-freebsd-deps-test/" + name,
        ],
        check=True,
    )
    return rel_path


class MainIntegrationTest(unittest.TestCase):
    """End-to-end test of `main()` against a small local fake pkg repository.

    Uses a `file://` repo URL so this needs no network access - `urlopen`
    (and therefore `download()`) supports the `file` scheme natively.

    The graph mirrors `ResolveClosureTests`: `root-pkg` depends on `keep-pkg`
    and `excluded-pkg` (which in turn depends on `excl-only-pkg`).
    """

    def test_exclude_flag_removes_pruned_packages_from_the_sysroot(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo_dir = os.path.join(tmp, "repo")
            os.makedirs(repo_dir)
            sysroot = os.path.join(tmp, "sysroot")

            packages = {
                "root-pkg": {"deps": {"keep-pkg": {}, "excluded-pkg": {}}},
                "excluded-pkg": {"deps": {"excl-only-pkg": {}}},
                "excl-only-pkg": {"deps": {}},
                "keep-pkg": {"deps": {}},
            }
            manifest_lines = []
            for name, entry in packages.items():
                rel_path = _make_fake_pkg(repo_dir, name)
                manifest_lines.append(json.dumps({
                    "name": name,
                    "version": "1",
                    "origin": f"fake/{name}",
                    "path": rel_path,
                    "deps": entry["deps"],
                }))
            manifest_path = os.path.join(repo_dir, "packagesite.yaml")
            with open(manifest_path, "w", encoding="utf-8") as f:
                f.write("\n".join(manifest_lines) + "\n")

            deps_output = os.path.join(tmp, "deps.json")
            argv = [
                "--sysroot", sysroot,
                "--repo", "file://" + repo_dir,
                "--manifest", manifest_path,
                "--root", "root-pkg",
                "--exclude", "excluded-pkg",
                "--deps-output", deps_output,
            ]
            rc = fetch_freebsd_deps.main(argv)
            self.assertEqual(rc, 0)

            marker_dir = os.path.join(sysroot, "usr", "local", "share", "fetch-freebsd-deps-test")
            extracted = set(os.listdir(marker_dir)) if os.path.isdir(marker_dir) else set()
            self.assertEqual(extracted, {"root-pkg", "keep-pkg"})

            with open(deps_output, encoding="utf-8") as f:
                deps = json.load(f)
            self.assertEqual(deps, {"root-pkg": {"origin": "fake/root-pkg", "version": "1"}})


if __name__ == "__main__":
    unittest.main()
