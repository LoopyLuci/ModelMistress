#!/usr/bin/env python3
"""ModelMistress's local CI/CD pipeline: everything a change must pass before it is pushed, on this machine.

    python ci/pipeline.py              every stage
    python ci/pipeline.py --fast       preflight, static, tests
    python ci/pipeline.py --only smoke
    python ci/pipeline.py --install-hook   run it on every `git push` (skip once: MM_SKIP_PIPELINE=1)

Stages:
  preflight  cargo is new enough, disk space, the git state
  static     rustfmt on the core's files; clippy -D warnings on the core and CLI (the legacy modules' lints are
             allowed in src/lib.rs); no secret-shaped strings or files over 5 MB in the push
  tests      cargo test on the core and CLI
  build      the release binary
  smoke      a real hub in a throwaway home: health, a refused caller without the token, 13+ operations, the
             catalog, the OpenAI model list, errors for unknown operations, models and bad input, the MCP bridge, a
             clean stop. With llama-server and a model of 2 GB or less on this machine (the settings in the user's
             ModelMistress home, or MM_SMOKE_CONFIG, are reused), also a real chat, a streamed chat, and no
             llama-server left after the stop.

Only one run at a time (a lock left by a crashed run is taken over); every command has a timeout that kills its
whole process tree; each run writes ci/logs/<time>.log and ci/reports/latest.json.
Set CARGO_TARGET_DIR to build somewhere else (fast storage).
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LOGS, REPORTS = ROOT / "ci" / "logs", ROOT / "ci" / "reports"
LOCK = ROOT / "ci" / ".pipeline.lock"
STAGES = ["preflight", "static", "tests", "build", "smoke"]
FMT_CLEAN = ["src/catalog.rs", "src/engine.rs", "src/hub.rs", "src/client.rs", "src/lib.rs",
             "src/bin/model-mistress/main.rs"]                    # grow this as legacy modules are rebuilt
SECRET = re.compile(r"(sk-[A-Za-z0-9_-]{20,}|ghp_[A-Za-z0-9]{30,}|github_pat_|AKIA[0-9A-Z]{16}|"
                    r"-----BEGIN [A-Z ]*PRIVATE KEY|xox[baprs]-[A-Za-z0-9-]{10,}|AIza[0-9A-Za-z_-]{30,})")
EXE = ".exe" if os.name == "nt" else ""
NO_WINDOW = 0x08000000 if os.name == "nt" else 0
log_lines: list[str] = []


def say(text: str = "") -> None:
    print(text, flush=True)
    log_lines.append(text)


def kill_tree(pid: int) -> None:
    if os.name == "nt":
        subprocess.run(["taskkill", "/PID", str(pid), "/T", "/F"], capture_output=True)
    else:
        try:
            os.killpg(os.getpgid(pid), 9)
        except OSError:
            pass


def run(cmd: list[str], timeout: float = 1800, env: dict | None = None) -> tuple[int, str]:
    p = subprocess.Popen(cmd, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding="utf-8",
                         errors="replace", env={**os.environ, **(env or {})}, creationflags=NO_WINDOW,
                         start_new_session=os.name != "nt")
    try:
        out, _ = p.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        kill_tree(p.pid)
        out = (p.communicate()[0] or "") + f"\n[timed out after {timeout:.0f}s]"
        return 124, out
    return p.returncode, out


def target_dir() -> Path:
    return Path(os.environ.get("CARGO_TARGET_DIR") or ROOT / "target")


# ---- stages ------------------------------------------------------------------------------------------------------------
def preflight() -> tuple[bool, str]:
    code, out = run(["cargo", "--version"], 60)
    if code != 0:
        return False, "cargo is not installed (https://rustup.rs)"
    ver = tuple(int(x) for x in re.findall(r"(\d+)\.(\d+)", out)[0])
    if ver < (1, 85):
        return False, f"cargo {out.split()[1]} is older than 1.85 (rustup update)"
    free = shutil.disk_usage(target_dir().parent if target_dir().exists() else ROOT).free / 1e9
    if free < 5:
        return False, f"only {free:.1f} GB free where the build goes"
    say(f"  ->     {out.strip()}; {free:.0f} GB free for the build")
    return True, ""


def changed_files() -> list[str]:
    code, out = run(["git", "diff", "--name-only", "--diff-filter=AM", "@{upstream}...HEAD"], 60)
    if code != 0:
        code, out = run(["git", "ls-files"], 60)
    return [f for f in out.splitlines() if f.strip()]


def static() -> tuple[bool, str]:
    code, out = run(["rustfmt", "--check", "--edition", "2021", *FMT_CLEAN], 300)
    if code != 0:
        return False, "rustfmt: these files need formatting (rustfmt --edition 2021 <file>):\n" + "\n".join(
            sorted({ln.split(" at line")[0] for ln in out.splitlines() if ln.startswith("Diff in")})[:20])
    code, out = run(["cargo", "clippy", "-p", "model-mistress", "-p", "model-mistress-cli", "--all-targets", "--",
                     "-D", "warnings"], 1800)
    if code != 0:
        errors = [ln for ln in out.splitlines() if ln.startswith("error") or ln.lstrip().startswith("-->")]
        return False, "clippy:\n" + "\n".join(errors[:30])
    say("  ->     rustfmt clean; clippy -D warnings clean")
    bad = []
    for f in changed_files():
        p = ROOT / f
        if not p.is_file() or p.resolve() == Path(__file__).resolve():     # this file holds the patterns themselves
            continue
        if p.stat().st_size > 5_000_000:
            bad.append(f"{f}: over 5 MB")
            continue
        try:
            if SECRET.search(p.read_text(encoding="utf-8", errors="ignore")):
                bad.append(f"{f}: a secret-shaped string")
        except OSError:
            pass
    if bad:
        return False, "the push would add:\n" + "\n".join(bad)
    return True, ""


def tests() -> tuple[bool, str]:
    code, out = run(["cargo", "test", "-p", "model-mistress", "-p", "model-mistress-cli", "--quiet"], 3600)
    results = re.findall(r"test result: (\w+)\. (\d+) passed; (\d+) failed", out)
    passed, failed = sum(int(r[1]) for r in results), sum(int(r[2]) for r in results)
    if code != 0:
        fails = [ln for ln in out.splitlines() if "FAILED" in ln or "panicked" in ln][:15]
        return False, f"{failed} failed, {passed} passed:\n" + "\n".join(fails or out.splitlines()[-25:])
    say(f"  ->     {passed} tests passed")
    return True, ""


def build() -> tuple[bool, str]:
    code, out = run(["cargo", "build", "--release", "-p", "model-mistress"], 3600)
    if code != 0:
        return False, "\n".join(out.splitlines()[-25:])
    if not (target_dir() / "release" / f"model-mistress{EXE}").is_file():
        return False, f"model-mistress{EXE} was not built"
    return True, ""


def http(method: str, url: str, token: str = "", body: bytes | None = None, timeout: float = 20) -> tuple[int, bytes]:
    headers = {"Content-Type": "application/json", **({"Authorization": f"Bearer {token}"} if token else {})}
    req = urllib.request.Request(url, data=body, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def user_home() -> Path:
    if os.name == "nt":
        return Path(os.environ.get("LOCALAPPDATA", Path.home())) / "ModelMistress"
    return Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local" / "share")) / "ModelMistress"


def pids_alive(pids: list[int]) -> list[int]:
    alive = []
    for pid in pids:
        if os.name == "nt":
            out = subprocess.run(["tasklist", "/FI", f"PID eq {pid}", "/NH"], capture_output=True, text=True).stdout
            if str(pid) in out:
                alive.append(pid)
        else:
            try:
                os.kill(pid, 0)
                alive.append(pid)
            except OSError:
                pass
    return alive


def smoke() -> tuple[bool, str]:
    exe = target_dir() / "release" / f"model-mistress{EXE}"
    home = Path(tempfile.mkdtemp(prefix="mm-smoke-"))
    settings = Path(os.environ.get("MM_SMOKE_CONFIG") or user_home() / "config.toml")
    if settings.is_file():
        shutil.copy(settings, home / "config.toml")
    proc = subprocess.Popen([str(exe), "--home", str(home), "serve"], stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL, creationflags=NO_WINDOW, start_new_session=os.name != "nt")
    children: list[int] = []
    try:
        control = home / "control.json"
        for _ in range(300):
            if control.is_file():
                break
            time.sleep(0.1)
        else:
            return False, "the hub wrote no control file within 30 s"
        c = json.loads(control.read_text(encoding="utf-8"))
        url, tok = c["url"], c["token"]

        def call(op: str, args: dict | None = None, timeout: float = 20) -> tuple[int, dict]:
            status, body = http("POST", f"{url}/v1/call/{op}", tok, json.dumps(args or {}).encode(), timeout)
            return status, json.loads(body or b"{}")

        checks = []
        checks.append(("health", http("GET", url + "/v1/health")[0] == 200))
        checks.append(("a caller without the token is refused", http("GET", url + "/v1/models")[0] == 401))
        status, body = http("GET", url + "/v1/operations", tok)
        checks.append(("13+ operations", status == 200 and len(json.loads(body)["operations"]) >= 13))
        status, models = call("model.list")
        checks.append(("the catalog lists", status == 200 and isinstance(models.get("result"), list)))
        status, body = http("GET", url + "/v1/models", tok)
        checks.append(("the OpenAI model list", status == 200 and json.loads(body)["object"] == "list"))
        checks.append(("an unknown operation is a 404", call("no.such")[0] == 404))
        checks.append(("an unknown model is a 404", call("model.info", {"model": "no-such-model:x"})[0] == 404))
        checks.append(("a malformed input is a 400", call("model.load", {"model": 5})[0] == 400))
        status, st = call("service.status")
        live = st.get("result", {}).get("llama_server")
        small = sorted((m for m in models.get("result", []) if m.get("size_gb", 99) <= 2.0 and not m.get("vision")),
                       key=lambda m: m["size_gb"])
        if live and small:
            model = small[0]["id"]
            say(f"  ->     live: {model} through {live}")
            status, r = call("chat.complete", {"model": model, "prompt": "Say hello.", "max_tokens": 64}, 300)
            res = r.get("result", {})   # a thinking model may spend a short budget on reasoning alone
            checks.append(("a real chat answers", status == 200 and bool(res.get("text") or res.get("reasoning"))
                           and res.get("usage", {}).get("completion_tokens", 0) > 0))
            children = [m["pid"] for m in call("model.loaded")[1].get("result", [])]
            body = json.dumps({"model": model, "stream": True, "max_tokens": 8,
                               "messages": [{"role": "user", "content": "Hi"}]}).encode()
            status, raw = http("POST", url + "/v1/chat/completions", tok, body, 120)
            checks.append(("a streamed chat sends events", status == 200 and raw.count(b"data: ") >= 2))
        else:
            say("  [--]   live chat skipped: " + ("no llama-server configured" if not live else "no model of 2 GB or less"))
        mcp = subprocess.run([str(exe), "--home", str(home), "mcp"], input=
                             '{"jsonrpc":"2.0","id":1,"method":"tools/list"}\n', capture_output=True, text=True,
                             timeout=30, creationflags=NO_WINDOW)
        try:
            n_tools = len(json.loads(mcp.stdout.splitlines()[0])["result"]["tools"])
        except (ValueError, IndexError, KeyError):
            n_tools = 0
        checks.append(("the MCP bridge lists the tools", n_tools >= 13))
        http("POST", url + "/v1/service/stop", tok, b"{}")
        try:
            proc.wait(timeout=20)
            stopped = not control.exists()
        except subprocess.TimeoutExpired:
            stopped = False
        checks.append(("stops cleanly and removes its control file", stopped))
        if children:
            time.sleep(1)
            checks.append(("no llama-server is left behind", not pids_alive(children)))
        for name, ok in checks:
            say(f"  {'->  ' if ok else '[ERR]'}   {name}")
        failed = [n for n, ok in checks if not ok]
        return (not failed), ("failed: " + ", ".join(failed)) if failed else ""
    finally:
        if proc.poll() is None:
            kill_tree(proc.pid)
        for pid in pids_alive(children):
            kill_tree(pid)
        shutil.rmtree(home, ignore_errors=True)


FUNCS = {"preflight": preflight, "static": static, "tests": tests, "build": build, "smoke": smoke}


def install_hook() -> int:
    hook = ROOT / ".git" / "hooks" / "pre-push"
    hook.write_text("#!/bin/sh\n# ModelMistress: run the local pipeline before every push (skip once: MM_SKIP_PIPELINE=1)\n"
                    '[ -n "$MM_SKIP_PIPELINE" ] && exit 0\nexec python ci/pipeline.py\n', encoding="utf-8")
    hook.chmod(0o755)
    print(f"installed {hook}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--fast", action="store_true")
    ap.add_argument("--only", action="append", choices=STAGES)
    ap.add_argument("--install-hook", action="store_true")
    a = ap.parse_args()
    if a.install_hook:
        return install_hook()
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(errors="replace")
        except (AttributeError, ValueError):
            pass
    if LOCK.exists():
        try:
            pid = int(LOCK.read_text().strip() or 0)
            import psutil  # noqa: PLC0415
            alive = psutil.pid_exists(pid)
        except Exception:  # noqa: BLE001
            alive = False
        if alive:
            print(f"another pipeline run (pid {pid}) is going; wait for it", file=sys.stderr)
            return 1
    LOCK.write_text(str(os.getpid()))
    stages = a.only or (STAGES[:3] if a.fast else STAGES)
    results, t0 = [], time.time()
    try:
        for name in STAGES:
            say(f"\n=== {name} ===")
            if name not in stages:
                say("  [--]   not asked for")
                results.append((name, None, 0.0, ""))
                continue
            t = time.time()
            ok, msg = FUNCS[name]()
            dt = time.time() - t
            say(f"  {'[ok]' if ok else '[ERR]'}   {name} ({dt:.1f}s){' ' + msg if msg and ok else ''}")
            if not ok:
                say("  " + msg.replace("\n", "\n  "))
            results.append((name, ok, dt, msg))
            if not ok:
                break
    finally:
        LOCK.unlink(missing_ok=True)
    passed = all(ok is not False for _, ok, _, _ in results)
    say(f"\n=== Summary ===\n" + "\n".join(f"  {'[ok]' if ok else '[--]' if ok is None else '[ERR]'}   {n:<10} {dt:6.1f}s"
                                         for n, ok, dt, _ in results))
    say(f"\n  {'PASSED' if passed else 'FAILED'} in {time.time() - t0:.1f}s")
    LOGS.mkdir(parents=True, exist_ok=True)
    REPORTS.mkdir(parents=True, exist_ok=True)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    (LOGS / f"{stamp}.log").write_text("\n".join(log_lines), encoding="utf-8")
    report = {"passed": passed, "at": stamp, "seconds": round(time.time() - t0, 1),
              "stages": [{"stage": n, "ok": ok, "seconds": round(dt, 1), "message": m} for n, ok, dt, m in results]}
    (REPORTS / "latest.json").write_text(json.dumps(report, indent=1), encoding="utf-8")
    with open(REPORTS / "history.jsonl", "a", encoding="utf-8") as f:
        f.write(json.dumps(report) + "\n")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
