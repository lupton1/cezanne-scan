#!/usr/bin/env python3
"""Run the C cezanne-server and the Rust one side by side against the simulator and check
that they answer the same and save byte identical files.

Each server gets its own Rust `cezanne-bridge --sim` (a bridge serves one client), so both
see the same simulated scanner. Standard library only.

    python3 compare.py --c ./cezanne-server-host --rust ../target/release/cezanne-server \
        --bridge ../target/release/cezanne-bridge
"""
import argparse
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

TOKEN = "t0k-compare"
FAILS = []
CHECKS = 0


def check(cond, what):
    global CHECKS
    CHECKS += 1
    if not cond:
        FAILS.append(what)
        print("  FAIL", what)


class Server:
    def __init__(self, name, port):
        self.name = name
        self.base = f"http://127.0.0.1:{port}/"
        self.cookie = None

    def call(self, path, body=None, method=None, cookie=True, raw=False):
        data = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request(self.base + path, data=data, method=method or ("POST" if data else "GET"))
        if data is not None:
            req.add_header("Content-Type", "application/json")
        if cookie and self.cookie:
            req.add_header("Cookie", self.cookie)
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                status, headers, payload = r.status, r.headers, r.read()
        except urllib.error.HTTPError as e:
            status, headers, payload = e.code, e.headers, e.read()
        if raw:
            return status, headers, payload
        try:
            return status, headers, json.loads(payload or b"{}")
        except ValueError:
            return status, headers, {"_text": payload.decode("utf-8", "replace")}

    def state(self):
        return self.call("api/state")[2]

    def wait_idle(self, timeout=180):
        end = time.time() + timeout
        while time.time() < end:
            st = self.state()
            if not st.get("busy"):
                return st
            time.sleep(0.2)
        raise SystemExit(f"{self.name}: still busy after {timeout}s")

    def wait_connected(self, timeout=30):
        end = time.time() + timeout
        while time.time() < end:
            st = self.state()
            if st.get("connected"):
                return st
            time.sleep(0.2)
        raise SystemExit(f"{self.name}: did not connect: {self.state().get('log')}")


def start(cmd, log):
    return subprocess.Popen(cmd, stdout=log, stderr=subprocess.STDOUT)


def wait_port(url, timeout=20):
    end = time.time() + timeout
    while time.time() < end:
        try:
            urllib.request.urlopen(url + "api/info", timeout=2).read()
            return
        except Exception:
            time.sleep(0.2)
    raise SystemExit(f"{url} did not start")


def adjustments(meta, film):
    return json.loads(json.dumps(next(f for f in meta["films"] if f["id"] == film)["adjustments"]))


def as_f32(v):
    """The Rust server writes f32 settings widened to f64 (0.15000000596046448) in some
    answers and in shortest form (0.15) in others. Both read back as the same f32, so
    compare them that way."""
    if isinstance(v, float):
        return struct.unpack("f", struct.pack("f", v))[0]
    if isinstance(v, list):
        return [as_f32(x) for x in v]
    if isinstance(v, dict):
        return {k: as_f32(x) for k, x in v.items()}
    return v


def same_json(a, b, what, f32=False):
    if f32:
        a, b = as_f32(a), as_f32(b)
    check(a == b, f"{what}: {json.dumps(a)[:300]} != {json.dumps(b)[:300]}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--c", required=True)
    ap.add_argument("--rust", required=True)
    ap.add_argument("--bridge", required=True)
    ap.add_argument("--keep", action="store_true", help="keep the output folders")
    a = ap.parse_args()

    work = tempfile.mkdtemp(prefix="cz-compare-")
    out_r, out_c = os.path.join(work, "rust"), os.path.join(work, "c")
    log = open(os.path.join(work, "servers.log"), "w")
    procs = []
    try:
        procs.append(start([a.bridge, "--sim", "--listen", "127.0.0.1:7911"], log))
        procs.append(start([a.bridge, "--sim", "--listen", "127.0.0.1:7912"], log))
        time.sleep(1.0)
        env = dict(os.environ, CEZANNE_SERVER_TOKEN=TOKEN)
        procs.append(subprocess.Popen([a.rust, "--listen", "127.0.0.1:7921", "--output-dir", out_r,
                                       "--connect", "bridge:127.0.0.1:7911"], stdout=log, stderr=subprocess.STDOUT, env=env))
        cproc = subprocess.Popen([a.c, "--listen", "127.0.0.1:7922", "--output-dir", out_c,
                                  "--connect", "bridge:127.0.0.1:7912"], stdout=log, stderr=subprocess.STDOUT, env=env)
        procs.append(cproc)
        R, C = Server("rust", 7921), Server("c", 7922)
        for s in (R, C):
            wait_port(s.base)

        print("access token")
        for s in (R, C):
            st, _, j = s.call("api/info", cookie=False)
            check(st == 200 and j["needs_token"] and not j["authorised"], f"{s.name} info before login {j}")
            st, _, j = s.call("api/state", cookie=False)
            check(st == 401 and j.get("error") == "Access token needed", f"{s.name} state without token {st} {j}")
            st, _, j = s.call("api/login", {"token": "wrong"}, cookie=False)
            check(st == 401 and j.get("error") == "Wrong access token", f"{s.name} wrong token {st} {j}")
            st, h, j = s.call("api/login", {"token": " " + TOKEN + " "}, cookie=False)
            check(st == 200 and j == {"ok": True}, f"{s.name} login {st} {j}")
            sc = h.get("Set-Cookie", "")
            check(sc == f"cezanne_token={TOKEN}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000",
                  f"{s.name} cookie {sc}")
            s.cookie = sc.split(";")[0]
            st, _, j = s.call("api/info")
            check(j["authorised"], f"{s.name} authorised after login")
            req = urllib.request.Request(s.base + "api/state", headers={"X-Cezanne-Token": TOKEN})
            check(urllib.request.urlopen(req).status == 200, f"{s.name} header token")

        print("connect")
        sr, sc_ = R.wait_connected(), C.wait_connected()
        check(sr["connected"] == sc_["connected"].replace("7912", "7911"), f"connected {sr['connected']} / {sc_['connected']}")
        for k in ("bed_mm", "max_pixels_per_line", "prescan_seq", "prescan_bed_mm", "busy", "start_kind", "os"):
            same_json(sr[k], sc_[k], f"state {k}")
        check(set(sr) == set(sc_), f"state keys {sorted(set(sr) ^ set(sc_))}")
        check([l["text"] for l in sr["log"]] == [l["text"].replace("7912", "7911") for l in sc_["log"]],
              f"log {sr['log']} / {sc_['log']}")

        print("static files")
        for p in ("", "app.js", "style.css", "icon.png"):
            r1, r2 = R.call(p, raw=True, cookie=False), C.call(p, raw=True, cookie=False)
            check(r1[0] == r2[0] == 200 and r1[2] == r2[2], f"static {p!r}")
            check(r1[1].get("Content-Type") == r2[1].get("Content-Type"), f"content type {p!r}")

        print("meta and presets")
        meta_r, meta_c = R.call("api/meta")[2], C.call("api/meta")[2]
        same_json(meta_r, meta_c, "meta", f32=True)
        same_json(R.call("api/presets")[2], C.call("api/presets")[2], "presets", f32=True)

        print("errors before a prescan")
        for path, body in [("api/preview", {"area": {"x_mm": 1, "y_mm": 1, "width_mm": 5, "height_mm": 5},
                                            "adjustments": adjustments(meta_r, "Positive")}),
                           ("api/prescan", {"dpi": 10, "film": "Positive"}),
                           ("api/prescan", {"dpi": 400, "film": "Positive"}),
                           ("api/scan", {"frames": [], "mode": "Rgb48", "dpi": 300}),
                           ("api/scan", {"frames": [{"name": "x", "area": {"x_mm": 0, "y_mm": 0, "width_mm": 5, "height_mm": 5},
                                                     "adjustments": adjustments(meta_r, "Positive")}], "mode": "Rgb48", "dpi": 13000})]:
            r1, r2 = R.call(path, body), C.call(path, body)
            check(r1[0] == r2[0] and r1[2] == r2[2], f"{path} {body.get('dpi')}: {r1[0]} {r1[2]} / {r2[0]} {r2[2]}")
        r1, r2 = R.call("api/density?x=10&y=10"), C.call("api/density?x=10&y=10")
        same_json(r1[2], r2[2], "density before prescan")
        r1, r2 = R.call("api/files/99", raw=True), C.call("api/files/99", raw=True)
        check(r1[0] == r2[0] == 404, "missing file 404")

        print("prescan")
        for s in (R, C):
            st, _, j = s.call("api/prescan", {"dpi": 50, "film": "ColorNegative",
                                              "advanced": {"area_unit": "TenthMm", "res_scale": 10}})
            check(st == 200, f"{s.name} prescan {st} {j}")
        # a second request while busy
        for s in (R, C):
            st, _, j = s.call("api/prescan", {"dpi": 50, "film": "ColorNegative"})
            check(st == 409, f"{s.name} busy prescan {st} {j}")
        sr, sc_ = R.wait_idle(), C.wait_idle()
        same_json(sr["prescan_bed_mm"], sc_["prescan_bed_mm"], "prescan bed")
        p1, p2 = R.call("api/prescan.png", raw=True), C.call("api/prescan.png", raw=True)
        check(p1[0] == p2[0] == 200 and len(p1[2]) > 1000 and p1[2] == p2[2], f"prescan png {len(p1[2])} {len(p2[2])}")

        print("densitometer")
        for x, y in [(10, 10), (0, 0), (170.5, 260.25), (341.9, 530.9), (342, 10), (-1, 5), (100.123, 3.3)]:
            q = f"api/density?x={x}&y={y}"
            same_json(R.call(q)[2], C.call(q)[2], f"density {x},{y}")

        print("previews")
        cases = []
        base_area = {"x_mm": 30.0, "y_mm": 40.0, "width_mm": 36.0, "height_mm": 24.0}
        for film in ("Positive", "ColorNegative", "BwNegative", "Reflective"):
            cases.append((film, base_area, adjustments(meta_r, film), True))
            cases.append((film, base_area, adjustments(meta_r, film), False))
        neg = adjustments(meta_r, "ColorNegative")
        neg["curve"] = {"points": [[0, 0], [0.25, 0.19], [0.75, 0.81], [1, 1]]}
        neg["hd"], neg["sd"], neg["gamma"] = [2.1, 2.3, 2.45], [0.12, 0.3, 0.41], 1.75
        cases.append(("curve", base_area, neg, False))
        s2 = json.loads(json.dumps(neg))
        s2["sharpen"] = {"enabled": True, "amount": 1.3, "radius": 2.4}
        s2["rotate"], s2["mirror"], s2["output_min"], s2["output_max"] = 1, True, 12, 240
        cases.append(("sharpen rotate mirror", base_area, s2, False))
        s3 = json.loads(json.dumps(s2))
        s3["gray"], s3["rotate"] = "Luminance", 3
        s3["curve"] = {"points": [[0, 0.05], [0.4, 0.3], [0.41, 0.31], [0.8, 0.9], [1, 0.97], [0.6, 0.62]]}
        cases.append(("luminance odd curve", {"x_mm": 0.0, "y_mm": 0.0, "width_mm": 400.0, "height_mm": 9.9}, s3, False))
        s4 = adjustments(meta_r, "BwNegative")
        s4["gray"], s4["rotate"] = {"Channel": 2}, 2
        cases.append(("channel 2", {"x_mm": 300.7, "y_mm": 500.3, "width_mm": 41.0, "height_mm": 30.0}, s4, True))
        for name, area, adj, auto in cases:
            body = {"area": area, "adjustments": adj, "auto_levels": auto}
            r1, r2 = R.call("api/preview", body, raw=True), C.call("api/preview", body, raw=True)
            check(r1[0] == r2[0] == 200, f"preview {name} status {r1[0]} {r2[0]} {r2[2][:200]}")
            check(r1[2] == r2[2], f"preview {name} png differs ({len(r1[2])} / {len(r2[2])} bytes)")
            a1, a2 = json.loads(r1[1].get("X-Adjustments", "null")), json.loads(r2[1].get("X-Adjustments", "null"))
            same_json(a1, a2, f"preview {name} adjustments")
        body = {"area": {"x_mm": 1, "y_mm": 1, "width_mm": 0, "height_mm": 5}, "adjustments": adjustments(meta_r, "Positive")}
        r1, r2 = R.call("api/preview", body), C.call("api/preview", body)
        check(r1[0] == r2[0] == 400 and r1[2] == r2[2], f"empty frame {r1} {r2}")

        print("scans")
        frames = []
        for i, (name, area, adj, auto) in enumerate(cases):
            frames.append({"name": f"Frame {i + 1} {name}", "area": area, "adjustments": adj, "auto_levels": auto})
        big = dict(frames[9], name="Big frame", area={"x_mm": 20.0, "y_mm": 30.0, "width_mm": 36.0, "height_mm": 24.0})
        big["adjustments"] = json.loads(json.dumps(s2))
        jobs = [
            ("Rgb48", 150, True, frames[:4], "roll_"),
            ("Rgb48", 150, False, frames[4:7], "../../evil"),
            ("Rgb24", 150, True, frames[7:9], ""),
            ("Gray16", 150, True, frames[9:11], "grey "),
            ("Gray8", 150, False, frames[11:], ".hidden"),
            # many TIFF strips and many READ commands, sharpening on 26 MB of 16 bit RGB
            ("Rgb48", 1800, True, [big], "big_"),
        ]
        seen = set()
        for mode, dpi, sixteen, fr, prefix in jobs:
            body = {"frames": fr, "mode": mode, "dpi": dpi, "sixteen_bit": sixteen, "prefix": prefix,
                    "advanced": {"area_unit": "TenthMm", "res_scale": 10}}
            r1, r2 = R.call("api/scan", body), C.call("api/scan", body)
            check(r1[0] == r2[0] == 200 and r1[2] == r2[2], f"scan {mode} {r1} {r2}")
            sr, sc_ = R.wait_idle(), C.wait_idle()
            check(len(sr["files"]) == len(sc_["files"]), f"file count {len(sr['files'])} {len(sc_['files'])}")
            for f1, f2 in zip(sr["files"], sc_["files"]):
                if f1["id"] in seen:
                    continue
                seen.add(f1["id"])
                k1 = {k: v for k, v in f1.items() if k != "created"}
                k2 = {k: v for k, v in f2.items() if k != "created"}
                same_json(k1, k2, f"file entry {f1['file_name']}")
                d1, d2 = R.call(f"api/files/{f1['id']}", raw=True), C.call(f"api/files/{f2['id']}", raw=True)
                check(d1[0] == d2[0] == 200, f"download {f1['file_name']}")
                check(d1[2] == d2[2] and len(d1[2]) == f1["bytes"],
                      f"TIFF {f1['file_name']} {mode} differs ({len(d1[2])} / {len(d2[2])} bytes)")
                check(d1[1].get("Content-Disposition") == d2[1].get("Content-Disposition"), "content disposition")
        check([l["text"] for l in sr["log"]][-12:] == [l["text"] for l in sc_["log"]][-12:],
              f"scan log\n{[l['text'] for l in sr['log']][-12:]}\n{[l['text'] for l in sc_['log']][-12:]}")

        print("probe and log")
        for s in (R, C):
            s.call("api/probe", {})
        time.sleep(1.5)
        l1 = [l["text"] for l in R.state()["log"]][-1]
        l2 = [l["text"] for l in C.state()["log"]][-1]
        check(l1 == l2.replace("7912", "7911"), f"probe report\n{l1}\n---\n{l2}")
        st = C.call("api/state?log_after=3")[2]
        check(all(l["seq"] > 3 for l in st["log"]), "log_after filters")

        print("delete and restart")
        fid = C.state()["files"][0]["id"]
        name = C.state()["files"][0]["file_name"]
        st, _, j = C.call(f"api/files/{fid}", method="DELETE")
        check(st == 200 and not os.path.exists(os.path.join(out_c, name)), f"delete {st} {j}")
        check(all(f["id"] != fid for f in C.state()["files"]), "deleted file gone from the list")
        count = len(C.state()["files"])
        cproc.terminate()
        cproc.wait()
        cproc = subprocess.Popen([a.c, "--listen", "127.0.0.1:7922", "--output-dir", out_c], stdout=log,
                                 stderr=subprocess.STDOUT, env=env)
        procs.append(cproc)
        wait_port(C.base)
        st = C.state()
        check(len(st["files"]) == count and all(f["frame"] == "" for f in st["files"]), f"earlier scans {len(st['files'])} / {count}")
        check(any("earlier scans found" in l["text"] for l in st["log"]), "earlier scans log line")
        check(st["connected"] is None and st["start_kind"] is None, "no connection without --connect")

        print("simulator choice and bad bridge on the C server")
        C.call("api/connect", {"kind": "Simulator"})
        time.sleep(0.5)
        check("simulator is not part of this build" in C.state()["log"][-1]["text"], "sim message")
        C.call("api/connect", {"kind": "Bridge", "host": "127.0.0.1:7999", "token": ""})
        end = time.time() + 10
        while time.time() < end and not C.state()["log"][-1]["text"].startswith("Connection failed"):
            time.sleep(0.2)
        check(C.state()["log"][-1]["text"].startswith("Connection failed"), f"bad bridge fails cleanly {C.state()['log'][-1]}")

        print("saved preset on the C server")
        p = {"name": "zz compare test", "mode": "Gray16", "dpi": 1234.5, "adjustments": s2, "light": "Transmissive",
             "output_depth": "Eight", "auto_levels": False}
        st, _, j = C.call("api/presets", p)
        check(st == 200, f"save preset {st} {j}")
        got = [x for x in C.call("api/presets")[2]["presets"] if x["name"] == "zz compare test"]
        check(len(got) == 1 and not got[0]["builtin"] and got[0]["dpi"] == 1234.5 and got[0]["adjustments"] == s2
              and got[0]["mode"] == "Gray16", f"preset read back {got}")
        rp = [x for x in R.call("api/presets")[2]["presets"] if x["name"] == "zz compare test"]
        check(len(rp) == 1 and as_f32(rp[0]["adjustments"]) == as_f32(s2), f"Rust server reads the C preset {rp}")
    finally:
        for p in procs:
            p.terminate()
        for p in procs:
            try:
                p.wait(5)
            except Exception:
                p.kill()
        log.close()
        for d in (os.environ.get("APPDATA"), os.path.expanduser("~/Library/Application Support"),
                  os.path.expanduser("~/.config")):
            if d:
                for sub in ("Cezanne Scan", "cezanne-scan"):
                    f = os.path.join(d, sub, "presets", "zz compare test.json")
                    if os.path.exists(f):
                        os.remove(f)
        if a.keep or FAILS:
            print("output kept in", work)
        else:
            shutil.rmtree(work, ignore_errors=True)

    print(f"\n{CHECKS - len(FAILS)} of {CHECKS} checks passed")
    sys.exit(1 if FAILS else 0)


if __name__ == "__main__":
    main()
