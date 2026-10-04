"""Drive the UI in headless chromium and take screenshots; fails on JS errors.

  bench/scripts/limit.sh -m 4G python3 bench/web/shots.py [--port 18765] [--name win] [steps...]

Needs a server started with bench/web/serve.sh (token testtoken-NAME-0123456789). The `readme`
step writes the README's pictures (docs/assets/screenshots/web-ui-*.png); `--scrub DIR=NEW`
replaces a local path in the page before each picture, e.g. --scrub /home/me/images=/cases."""

import argparse
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
from cdp import DATA, Chrome  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--port", type=int, default=18765)
ap.add_argument("--name", default="win")
ap.add_argument("--out", default=os.path.join(DATA, "testdata/scratch/webui/shots"))
ap.add_argument("--size", default="1440x900")
ap.add_argument("--scrub", action="append", default=[], metavar="DIR=NEW", help="show NEW instead of DIR in the pictures")
ap.add_argument("steps", nargs="*")
a = ap.parse_args()
os.makedirs(a.out, exist_ok=True)
W, H = map(int, a.size.split("x"))
BASE = f"http://127.0.0.1:{a.port}"
TOKEN = f"testtoken-{a.name}-0123456789"

c = Chrome(W, H)
failed = []


def shot(name):
    c.eval("document.getElementById('toasts').replaceChildren()")
    p = os.path.join(a.out, f"{a.name}-{name}.png")
    c.shot(p)
    print("shot", p)


def step(name):
    def deco(fn):
        STEPS.append((name, fn))
        return fn
    return deco


STEPS = []


def login():
    c.goto(f"{BASE}/?r={time.time()}#token={TOKEN}", 1.0)
    c.wait("document.querySelector('.qs') || document.querySelector('.wb-panel')", 20)
    c.pump(0.4)


def workspace():
    """Past the Quick Start, results closed, nothing ticked."""
    if c.eval("!!document.querySelector('.qs')"):
        c.key("Escape")
    if c.eval("document.body.classList.contains('results-open')"):
        c.key("Escape")
    c.eval("document.getElementById('pl-tab-select').click()")
    c.wait("document.querySelector('.wb-plugins .pl-row')", 20)
    c.pump(0.3)


def theme(t):
    c.eval(f"localStorage.setItem('fastvol.theme', '{t}'); document.documentElement.setAttribute('data-theme', '{t}')")
    c.pump(0.3)


def scrub():
    """Local paths (and notifications) out of a picture (the page shows real paths)."""
    c.eval("document.getElementById('toasts').replaceChildren()")
    pairs = json.dumps([x.split("=", 1) for x in a.scrub])
    c.eval(f"""(() => {{ const map = {pairs};
      const fix = s => {{ for (const [f, t] of map) s = s.split(f).join(t); return s; }};
      const w = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
      let n; while ((n = w.nextNode())) {{ const v = fix(n.nodeValue); if (v !== n.nodeValue) n.nodeValue = v; }}
      for (const e of document.querySelectorAll('[title]')) e.title = fix(e.title); }})()""")


def start_run(name):
    """Run the built-in triage preset for the image's OS (else the first one); returns its name."""
    c.eval("document.getElementById('pl-tab-presets').click()")
    c.wait("document.querySelector('.ps-row')", 10)
    pick = "([...document.querySelectorAll('.ps-row')].find(r => /Triage/.test(r.textContent)) || document.querySelector('.ps-row'))"
    preset = c.eval(f"{pick}.querySelector('.ps-name').textContent")
    c.eval(f"{pick}.click()")
    c.wait("!document.getElementById('pl-run').hidden", 10)
    c.eval("document.getElementById('pl-run').click()")
    c.wait("document.body.classList.contains('results-open')", 20)
    # every plugin of the run finished
    c.wait("document.querySelectorAll('.rs-tab').length > 0 && ![...document.querySelectorAll('.rs-tab .rs-dot')].some(d => d.classList.contains('running') || d.classList.contains('queued'))", 300)
    c.pump(0.6)
    return preset


@step("quickstart")
def quickstart():
    login()
    shot("01-quickstart")
    c.key("2")
    c.wait("document.querySelector('.qs-file')", 10)
    c.pump(0.4)
    shot("02-quickstart-open")
    c.key("3")
    c.pump(0.8)
    shot("03-quickstart-previous")


@step("workspace")
def workspace_step():
    login()
    workspace()
    shot("04-workspace")


@step("select")
def select():
    workspace()
    c.eval("document.getElementById('pl-tab-select').click()")
    c.eval("(q => { q.value = 'ps'; q.dispatchEvent(new Event('input')); })(document.querySelector('#plugins input[type=search]'))")
    c.pump(0.3)
    c.eval("document.querySelectorAll('.pl-row')[0].click()")
    c.eval("document.querySelectorAll('.pl-row')[1].click()")
    c.wait("!document.getElementById('pl-run').hidden", 5)
    c.pump(0.3)
    shot("05-select")
    c.eval("(q => { q.value = ''; q.dispatchEvent(new Event('input')); })(document.querySelector('#plugins input[type=search]'))")


@step("presets")
def presets():
    workspace()
    c.eval("document.getElementById('pl-tab-presets').click()")
    c.wait("document.querySelector('.ps-row')", 10)
    c.pump(0.3)
    shot("06-presets")


@step("results")
def results():
    workspace()
    start_run("results")
    shot("07-results")
    # a tree (pstree, or the first plugin with nested rows)
    tree = c.eval("[...document.querySelectorAll('.rs-tab')].findIndex(t => /pstree/.test(t.textContent))")
    if tree >= 0:
        c.eval(f"document.querySelectorAll('.rs-tab')[{tree}].click()")
        c.wait("document.querySelector('.vt-row:not(.loading)')", 20)
        c.pump(0.6)
        shot("08-results-tree")
    c.eval("(q => { q.value = 'svchost'; q.dispatchEvent(new Event('input')); })(document.querySelector('.rs-q'))")
    c.pump(1.2)
    shot("09-results-filter")
    c.eval("(q => { q.value = ''; q.dispatchEvent(new Event('input')); })(document.querySelector('.rs-q'))")
    c.pump(0.5)


@step("options")
def options():
    workspace()
    c.eval("document.getElementById('wb-options').click()")
    c.wait("document.querySelectorAll('.op-row').length > 10", 10)
    c.pump(0.3)
    shot("10-options")
    c.key("Escape")


@step("light")
def light():
    workspace()
    theme("light")
    shot("11-workspace-light")
    theme("dark")


@step("laptop")
def laptop():
    workspace()
    c.resize(1280, 800)
    c.pump(0.5)
    shot("12-laptop-1280")
    c.eval("document.getElementById('wb-ovbtn').click()")
    c.pump(0.4)
    shot("13-laptop-overview")
    c.eval("document.getElementById('wb-ovbtn').click()")
    c.resize(W, H)
    c.pump(0.4)


@step("readme")
def readme():
    """The README's pictures: 1440x780, workspace with plugins ticked and a run open, results."""
    c.resize(1440, 780)
    for t in ("dark", "light"):
        theme(t)
        c.eval("for (const k of Object.keys(localStorage)) if (k.startsWith('fastvol.cols:') || k === 'fastvol.layout') localStorage.removeItem(k)")
        login()
        workspace()
        if not c.eval("document.querySelector('.rn-head')"):
            start_run("readme")
            workspace()
        head = "document.querySelector('.rn-head')"
        if not c.eval(f"{head}.parentElement.classList.contains('open')"):
            c.eval(f"{head}.querySelector('.rn-caret').click()")
        c.eval("document.getElementById('pl-tab-select').click()")
        c.eval("(q => { q.value = 'malware'; q.dispatchEvent(new Event('input')); })(document.querySelector('#plugins input[type=search]'))")
        c.pump(0.3)
        c.eval("[...document.querySelectorAll('.pl-row')].filter(r => /malfind|hollowprocesses|ldrmodules/.test(r.title)).forEach(r => r.click())")
        c.eval("document.querySelector('.pl-list').scrollTop = 0; document.activeElement.blur()")
        c.pump(0.6)
        scrub()
        c.shot(os.path.join(a.out, f"web-ui-workspace-{t}.png"))
        c.eval(f"{head}.click()")
        c.pump(0.2)
        if not c.eval("document.body.classList.contains('results-open')"):
            c.eval(f"{head}.click()")
        c.wait("document.body.classList.contains('results-open')", 10)
        # the tab with the most rows makes the fullest table
        c.eval("(ts => ts.sort((x, y) => (+y.querySelector('.rs-badge').textContent.replace(/,/g, '') || 0) - (+x.querySelector('.rs-badge').textContent.replace(/,/g, '') || 0))[0].click())([...document.querySelectorAll('.rs-tab')])")
        c.pump(1.5)
        c.eval("document.activeElement.blur()")
        scrub()
        c.shot(os.path.join(a.out, f"web-ui-results-{t}.png"))
        c.eval("(q => { q.value = ''; q.dispatchEvent(new Event('input')); })(document.querySelector('#plugins input[type=search]'))")
        c.key("Escape")
        print("readme pictures", t)
    theme("dark")
    c.resize(W, H)


try:
    login()
    for name, fn in STEPS:
        if a.steps and name not in a.steps:
            continue
        t = time.time()
        try:
            fn()
            print(f"ok   {name} ({time.time() - t:.1f}s)")
        except Exception as e:
            failed.append(name)
            print(f"FAIL {name}: {e}")
            try:
                shot("fail-" + name)
            except Exception:
                pass
finally:
    if c.errors:
        print("JS ERRORS:")
        for e in c.errors:
            print("  ", e[:400])
    c.close()
sys.exit(1 if failed or c.errors else 0)
