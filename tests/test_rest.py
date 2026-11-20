"""Tests for ordering, ranking, Alfred output, manifests, favicons and macOS helpers."""

import json
import os
import shutil
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "workflow/tabs.alfredui/lib"))

from browvitab import alfred, favicons, install, macos, tabs  # noqa: E402


def tab(i, title, url, last, **extra):
    return dict({"id": i, "title": title, "url": url, "lastAccessed": last}, **extra)


def connected(pid, bid, name, ts, profile=None):
    browser = {"id": bid, "name": name, "pid": pid + 1000}
    if profile:
        browser["profile"] = profile
    return {"socket": Path(f"/tmp/x/{pid}.sock"), "listing": {"browser": browser, "tabs": ts}, "generation": 1}


def titles(views):
    return [v.title for v in views]


class TestOrdering(unittest.TestCase):
    def test_pinned_first_then_sections_by_last_use_current_last(self):
        firefox = connected(
            10,
            "firefox",
            "Firefox",
            [
                tab(1, "current", "https://c.test", 900, active=True),
                tab(4, "older", "https://o.test", 200),
                tab(2, "playing", "https://p.test", 100, audible=True),
                tab(3, "pin", "https://pin.test", 50, pinned=True),
            ],
        )
        brave = connected(20, "brave-browser", "Brave", [tab(5, "brave", "https://b.test", 300)])
        ordered = tabs.order([brave, firefox])
        self.assertEqual(titles(ordered), ["pin", "playing", "older", "current", "brave"])
        self.assertEqual(ordered[0].group, "Pinned")
        self.assertEqual(ordered[1].group, "Firefox")
        self.assertTrue(ordered[3].current)
        self.assertEqual(ordered[4].uid, "20:5")

    def test_profiles_label_instances_of_the_same_browser(self):
        a = connected(1, "firefox", "Firefox", [tab(1, "a", "https://a", 1)], "work")
        b = connected(2, "firefox", "Firefox", [tab(1, "b", "https://b", 2)], "home")
        ordered = tabs.order([a, b])
        self.assertEqual([t.source for t in ordered], ["Firefox · home", "Firefox · work"])
        self.assertNotEqual(ordered[0].uid, ordered[1].uid)
        self.assertEqual(tabs.order([a])[0].source, "Firefox")

    def test_hosts_and_match_text(self):
        self.assertEqual(tabs.url_host("https://www.GitHub.com:443/a?b"), "www.github.com")
        self.assertEqual(tabs.url_host("about:config"), "")
        self.assertEqual(tabs.url_host("moz-extension://x/y"), "")
        view = tabs.order(
            [
                connected(
                    1,
                    "firefox",
                    "Firefox",
                    [tab(1, "PR", "https://github.com/openshift-pipelines/pipelines-as-code/pull/1", 1)],
                )
            ]
        )[0]
        text = tabs.match_text(view)
        self.assertIn(" pipelines as code pull 1 ", text)
        self.assertTrue(text.endswith("/pull/1"))

    def test_ranking_and_matching(self):
        ordered = tabs.order(
            [
                connected(
                    1,
                    "firefox",
                    "Firefox",
                    [
                        tab(1, "Tekton docs", "https://tekton.dev/docs", 5),
                        tab(2, "Pull requests", "https://github.com/pulls", 4),
                        tab(3, "Gmail", "https://mail.google.com/", 3),
                        tab(4, "Something about github", "https://example.com/", 2),
                    ],
                )
            ]
        )
        self.assertEqual(titles(tabs.rank(ordered, "git"))[:2], ["Pull requests", "Something about github"])
        self.assertEqual(titles(tabs.rank(ordered, "https://github.com/foo"))[0], "Pull requests")
        self.assertEqual(titles(tabs.rank(ordered, "  ")), titles(ordered))
        self.assertEqual(titles(tabs.rank(ordered, "mail.google.com"))[0], "Gmail")
        self.assertTrue(tabs.matches(ordered[1], "pull git"))
        self.assertFalse(tabs.matches(ordered[1], "ull"))


class TestAlfred(unittest.TestCase):
    def test_placeholders_in_titles_are_defused(self):
        self.assertEqual(alfred.defuse("plain {curly}"), "plain {curly}")
        self.assertEqual(alfred.defuse("{file:/etc/passwd}"), "{\u200bfile:/etc/passwd}")
        self.assertEqual(alfred.defuse("a {VAR:x} b"), "a {\u200bVAR:x} b")

    def test_fingerprints_follow_content(self):
        self.assertEqual(alfred.fingerprint([{"uid": "1:1"}]), alfred.fingerprint([{"uid": "1:1"}]))
        self.assertNotEqual(alfred.fingerprint([{"uid": "1:1"}]), alfred.fingerprint([{"uid": "1:2"}]))

    def test_script_filter_uses_ctrl_to_close(self):
        view = tabs.order([connected(1, "firefox", "Firefox", [tab(1, "a", "https://a.test/", 1)])])
        out = alfred.script_filter(view, {})
        self.assertTrue(out["strictmods"] and out["skipknowledge"])
        self.assertEqual(out["items"][0]["mods"]["ctrl"]["variables"]["tabaction"], "close")
        self.assertEqual(out["items"][0]["mods"]["cmd"]["arg"], "https://a.test/")


class TestInstall(unittest.TestCase):
    def setUp(self):
        self.support = Path(tempfile.mkdtemp())
        (self.support / "Firefox").mkdir()
        (self.support / "BraveSoftware/Brave-Browser").mkdir(parents=True)

    def tearDown(self):
        shutil.rmtree(self.support, ignore_errors=True)

    def test_installs_for_present_browsers_and_is_idempotent(self):
        reports = install.install_manifests(self.support, Path("/opt/browvitab"))
        self.assertEqual([r.outcome for r in reports], ["written", "written"])
        firefox = self.support / "Mozilla/NativeMessagingHosts/com.vicinae.vicinae.json"
        self.assertIn("firefox@vicinae.com", firefox.read_text())
        again = install.install_manifests(self.support, Path("/opt/browvitab"))
        self.assertEqual({r.outcome for r in again}, {"unchanged"})
        removed = install.uninstall_manifests(self.support)
        self.assertEqual({r.outcome for r in removed}, {"removed"})
        self.assertFalse(firefox.exists())

    def test_vicinae_manifests_are_kept_unless_forced(self):
        d = self.support / "Mozilla/NativeMessagingHosts"
        d.mkdir(parents=True)
        path = d / "com.vicinae.vicinae.json"
        path.write_text(json.dumps({"name": "com.vicinae.vicinae", "path": "/usr/bin/vicinae"}))
        reports = install.install_manifests(self.support, Path("/x"))
        self.assertIn(("kept", "it starts Vicinae"), [(r.outcome, r.detail) for r in reports if r.path == path])
        install.uninstall_manifests(self.support)
        self.assertTrue(path.exists())
        forced = install.install_manifests(self.support, Path("/x"), force=True)
        self.assertIn("written", [r.outcome for r in forced if r.path == path])

    def test_symlinked_manifests_survive_install_and_uninstall(self):
        d = self.support / "Mozilla/NativeMessagingHosts"
        d.mkdir(parents=True)
        target = self.support / "real.json"
        target.write_text(install.manifest_json("gecko", Path("/x")))
        link = d / "com.vicinae.vicinae.json"
        link.symlink_to(target)
        reports = install.install_manifests(self.support, Path("/y"))
        self.assertIn("kept", [r.outcome for r in reports if r.path == link])
        install.uninstall_manifests(self.support)
        self.assertTrue(link.is_symlink() and target.exists())

    def test_install_files_copies_bin_and_lib(self):
        src = Path(tempfile.mkdtemp())
        try:
            (src / "bin").mkdir()
            (src / "bin/browvitab").write_text("#!/bin/sh\n")
            (src / "lib/browvitab").mkdir(parents=True)
            (src / "lib/browvitab/__init__.py").write_text('__version__ = "9.9"\n')
            launcher = install.install_files(self.support, src)
            self.assertTrue(os.access(launcher, os.X_OK))
            self.assertEqual(install.installed_version(self.support), "9.9")
        finally:
            shutil.rmtree(src, ignore_errors=True)


class TestFavicons(unittest.TestCase):
    def test_keys_and_sniffing(self):
        self.assertEqual(favicons.key("a.b-c.com"), "a.b-c.com")
        self.assertEqual(favicons.key("[::1]"), "___1_")
        self.assertEqual(favicons.sniff(b"\x89PNG\r\n"), "png")
        self.assertEqual(favicons.sniff(b"\x00\x00\x01\x00x"), "ico")
        self.assertEqual(favicons.sniff(b"<svg xmlns=..."), "svg")

    def test_looks_up_a_gecko_database(self):
        d = Path(tempfile.mkdtemp())
        try:
            db = d / "favicons.sqlite"
            conn = sqlite3.connect(db)
            conn.executescript(
                "CREATE TABLE moz_pages_w_icons(id INTEGER PRIMARY KEY, page_url TEXT);"
                "CREATE TABLE moz_icons(id INTEGER PRIMARY KEY, width INT, data BLOB);"
                "CREATE TABLE moz_icons_to_pages(page_id INT, icon_id INT);"
                "INSERT INTO moz_pages_w_icons VALUES(1,'https://example.org/a');"
                "INSERT INTO moz_icons VALUES(1,16,X'89504E470D0A');"
                "INSERT INTO moz_icons_to_pages VALUES(1,1);"
            )
            conn.commit()
            conn.close()
            cache = d / "cache"
            cache.mkdir()
            favicons.lookup("gecko", db, ["example.org", "none.test"], cache)
            self.assertTrue((cache / "example.org.png").is_file())
            self.assertIsNone(favicons.cached(cache, "none.test"))
        finally:
            shutil.rmtree(d, ignore_errors=True)


class TestMacos(unittest.TestCase):
    def test_bundles_processes_and_profiles(self):
        exe = "/Applications/Firefox.app/Contents/MacOS/plugin-container.app/Contents/MacOS/x"
        self.assertEqual(macos.app_bundle(exe), "/Applications/Firefox.app")
        self.assertIsNone(macos.app_bundle("/usr/bin/true"))
        self.assertTrue(macos.alive(os.getpid()))
        self.assertIsNotNone(macos.parent_of(os.getpid()))
        self.assertIsNotNone(macos.exe_path(os.getpid()))
        root = Path(tempfile.mkdtemp())
        try:
            (root / "Profiles/abcd.work").mkdir(parents=True)
            (root / "profiles.ini").write_text(
                "[Profile0]\nName=Work\nIsRelative=1\nPath=Profiles/abcd.work\n\n[General]\nVersion=2\n"
            )
            self.assertEqual(macos.gecko_profile_name(str(root / "Profiles/abcd.work")), "Work")
            self.assertEqual(macos.gecko_profile_name(str(root / "Profiles/zz.default-release")), "default-release")
        finally:
            shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    unittest.main()


class TestSetup(unittest.TestCase):
    def setUp(self):
        from browvitab import setup

        self.setup = setup
        self.home = Path(tempfile.mkdtemp())
        self.old_home = os.environ["HOME"]
        os.environ["HOME"] = str(self.home)
        support = self.home / "Library/Application Support"
        (self.home / "Applications/Firefox.app").mkdir(parents=True)
        (self.home / "Applications/Brave Browser.app").mkdir(parents=True)
        (support / "BraveSoftware/Brave-Browser/Default").mkdir(parents=True)
        profile = support / "Firefox/Profiles/abcd.default"
        profile.mkdir(parents=True)
        (profile / "extensions.json").write_text('{"addons":[{"id":"firefox@vicinae.com"}]}')
        self.support = support

    def tearDown(self):
        os.environ["HOME"] = self.old_home
        shutil.rmtree(self.home, ignore_errors=True)

    def steps(self, browsers=()):
        return {
            r["stitle.text"]: (r["sdetail.text"], r["action"], r["takeover"]) for r in self.setup.rows(list(browsers))
        }

    def test_checklist_walks_through_extension_connect_and_connected(self):
        steps = self.steps()
        self.assertEqual(steps["Firefox"][1], "connect")
        self.assertEqual(steps["Brave"][1], "store:brave-browser")
        self.assertEqual(self.setup.next_action(self.setup.rows([])), "connect")

        install.install_manifests(self.support, Path("/x/browvitab"))
        self.assertIn("Waiting", self.steps()["Firefox"][0])

        firefox = connected(1, "firefox", "Firefox", [tab(1, "a", "https://a", 1)])
        self.assertEqual(self.steps([firefox])["Firefox"][0], "Connected, 1 tab")

    def test_vicinae_owned_browsers_offer_a_takeover(self):
        d = self.support / "Mozilla/NativeMessagingHosts"
        d.mkdir(parents=True)
        (d / "com.vicinae.vicinae.json").write_text(json.dumps({"path": "/usr/bin/vicinae"}))
        self.assertEqual(self.steps()["Firefox"][2], "takeover:firefox")

    def test_script_filter_shows_the_checklist_without_browsers(self):
        out = alfred.script_filter([], {}, self.setup.rows([]))
        titles_ = [i["title"] for i in out["items"]]
        self.assertIn("Firefox", titles_)
        firefox = next(i for i in out["items"] if i["title"] == "Firefox")
        self.assertEqual((firefox["arg"], firefox["variables"]["tabaction"]), ("connect", "setup"))
