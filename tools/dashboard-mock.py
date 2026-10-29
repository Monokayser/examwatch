"""Dashboard mock server: serves the real dashboard/ files with a fake API and
WebSocket, so the UI can be developed and checked visually without ew-server
(no video: tiles show their status overlays).

    python tools/dashboard-mock.py [port]        then open http://127.0.0.1:8765

Sign in with any username and the password "password123" (or use the first-run
screen, which the mock shows first).
"""
import base64, hashlib, json, os, random, socket, struct, sys, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "dashboard")
PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
NOW = lambda: int(time.time() * 1e6)

LABS = [
    {"id": "anim1", "name": "3D Animation Lab 1", "room": "1018", "expected_pcs": 8},
    {"id": "visarts", "name": "Visual Arts & Communication", "room": "1019", "expected_pcs": 8},
    {"id": "game", "name": "Game & AR/VR Development", "room": "1015", "expected_pcs": 6},
    {"id": "anim2", "name": "3D Animation Lab 2", "room": "1018 (AB-04)", "expected_pcs": 6},
    {"id": "profdev", "name": "Professional Development", "room": "1020 (AB-04)", "expected_pcs": 4},
    {"id": "unassigned", "name": "Unassigned", "room": "", "expected_pcs": 0},
]
PREFIX = {"anim1": ("A1", "10.10.18", "hardware"), "visarts": ("VA", "10.10.19", "software"), "game": ("GM", "10.10.15", "hardware"),
          "anim2": ("A2", "10.10.28", "software"), "profdev": ("PD", "10.10.20", "hardware")}

state = {"needs_setup": True, "sessions": set(), "exams": [], "next_exam": 1, "pcs": {}, "clients": [], "lock": threading.Lock()}


def mk_pc(mid, name, lab, ip, enc, status):
    return {"machine_id": mid, "pc_name": name, "lab_id": lab, "ip": ip, "subnet": ".".join(ip.split(".")[:3]) + ".0/24",
            "status": status, "student_id": None, "exam_id": None, "encoder": enc, "gpu": "", "screen": [1920, 1080],
            "monitors": 1, "cpu_pct": round(random.uniform(0.3, 1.2), 1), "rss_mb": random.randint(35, 70), "throttle": 0,
            "since_us": NOW() - random.randint(1, 300) * 1_000_000, "viewers": 0}


for lab in LABS[:-1]:
    pre, net, enc = PREFIX[lab["id"]]
    for i in range(1, lab["expected_pcs"] + 1):
        st = "offline" if i == lab["expected_pcs"] else "idle"
        mid = f"m-{pre}-{i:02d}"
        state["pcs"][mid] = mk_pc(mid, f"{pre}-{i:02d}", lab["id"], f"{net}.{10 + i}", enc, st)
for i in range(1, 4):
    mid = f"m-NEW-{i}"
    state["pcs"][mid] = mk_pc(mid, f"LAB-PC-{i}", "unassigned", f"10.10.99.{20 + i}", "software", "idle")

# One closed exam with recordings, for the Recordings page.
state["exams"].append({"id": 99, "name": "CSE 211 Quiz", "labs": ["anim1"], "opened_us": NOW() - 26 * 3600e6, "closed_us": NOW() - 24 * 3600e6,
                       "opened_by": "admin", "roster_enforced": False, "roster_size": 0, "purged": False})


# ------------------------------------------------------------- websocket
def ws_send(conn, obj):
    data = json.dumps(obj).encode()
    hdr = bytes([0x81])
    n = len(data)
    hdr += bytes([n]) if n < 126 else (bytes([126]) + struct.pack(">H", n) if n < 65536 else bytes([127]) + struct.pack(">Q", n))
    try:
        conn.sendall(hdr + data)
        return True
    except OSError:
        return False


def broadcast(obj):
    with state["lock"]:
        state["clients"] = [c for c in state["clients"] if ws_send(c, obj)]


def open_exams():
    return [e for e in state["exams"] if not e.get("closed_us")]


def pc_event(pc):
    broadcast({"ev": "pc", "pc": pc})


def simulator():
    """Students press V1 over time; one PC drops offline and comes back."""
    n = 0
    while True:
        time.sleep(2)
        n += 1
        waiting = [p for p in state["pcs"].values() if p["status"] == "waiting"]
        if waiting:
            p = random.choice(waiting)
            p["status"], p["student_id"], p["since_us"] = "live", f"221-15-{random.randint(4000, 4999)}", NOW()
            pc_event(p)
            broadcast({"ev": "toast", "level": "join", "text": f"{p['student_id']} · {p['pc_name']} started", "lab_id": p["lab_id"]})
        if n % 15 == 0:
            live = [p for p in state["pcs"].values() if p["status"] == "live"]
            if live:
                p = random.choice(live)
                p["status"], p["since_us"] = "offline", NOW()
                pc_event(p)
                broadcast({"ev": "toast", "level": "alert", "text": f"{p['pc_name']} ({p['student_id']}) went OFFLINE", "lab_id": p["lab_id"]})


# ------------------------------------------------------------------ http
class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def authed(self):
        c = self.headers.get("Cookie", "")
        return any(part.strip() == f"ew_session={t}" for t in state["sessions"] for part in c.split(";"))

    def send_json(self, obj, code=200, cookie=None):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        if cookie:
            self.send_header("Set-Cookie", cookie)
        self.end_headers()
        self.wfile.write(body)

    def body(self):
        n = int(self.headers.get("Content-Length") or 0)
        return json.loads(self.rfile.read(n) or b"{}")

    def new_session(self, user):
        tok = os.urandom(16).hex()
        state["sessions"].add(tok)
        self.send_json({"user": user}, cookie=f"ew_session={tok}; HttpOnly; SameSite=Strict; Path=/")

    def do_GET(self):
        p = self.path.split("?")[0]
        if p in ("/", "/app.js", "/app.css"):
            f = {"/": "index.html", "/app.js": "app.js", "/app.css": "app.css"}[p]
            data = open(os.path.join(ROOT, f), "rb").read()
            ct = {"index.html": "text/html", "app.js": "text/javascript", "app.css": "text/css"}[f]
            self.send_response(200)
            self.send_header("Content-Type", ct + "; charset=utf-8")
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Cache-Control", "no-cache")
            self.end_headers()
            self.wfile.write(data)
            return
        if p == "/api/setup":
            return self.send_json({"needs_setup": state["needs_setup"]})
        if not self.authed():
            return self.send_json({"error": "Please sign in"}, 401)
        if p == "/api/me":
            return self.send_json({"user": "admin"})
        if p == "/api/pcs":
            return self.send_json({"pcs": list(state["pcs"].values())})
        if p == "/api/exams/history":
            return self.send_json({"exams": sorted(state["exams"], key=lambda e: -e["id"]), "retention_hours": 48})
        if p.startswith("/api/exams/") and p.endswith("/recordings"):
            eid = int(p.split("/")[3])
            e = next(x for x in state["exams"] if x["id"] == eid)
            t0 = e["opened_us"]
            students = [{"student_id": f"221-15-{4000 + i}", "lab_id": "anim1", "start_us": t0 + 60e6, "end_us": t0 + 3600e6,
                         "segments": 60, "bytes": random.randint(60, 120) * 1_000_000, "machine_id": f"m-A1-{i:02d}"} for i in range(1, 7)]
            claims = [{"student_id": s["student_id"], "machine_id": s["machine_id"], "pc_name": s["machine_id"][2:], "lab_id": "anim1", "claimed_us": t0} for s in students]
            return self.send_json({"exam": e, "students": students, "claims": claims})
        if p == "/api/audit":
            return self.send_json({"entries": []})
        if p == "/ws":
            return self.websocket()
        self.send_json({"error": "not found"}, 404)

    def do_POST(self):
        p = self.path.split("?")[0]
        if p == "/api/setup":
            state["needs_setup"] = False
            return self.new_session(self.body().get("username", "admin"))
        if p == "/api/login":
            b = self.body()
            if b.get("password") == "password123":
                return self.new_session(b.get("username"))
            return self.send_json({"error": "Wrong username or password"}, 401)
        if not self.authed():
            return self.send_json({"error": "Please sign in"}, 401)
        if p == "/api/logout":
            return self.send_json({"ok": True})
        if p == "/api/exams":
            b = self.body()
            busy = {l for e in open_exams() for l in e["labs"]}
            if set(b["labs"]) & busy:
                return self.send_json({"error": "A chosen lab already has an exam open"}, 400)
            e = {"id": state["next_exam"], "name": b["name"], "labs": b["labs"], "opened_us": NOW(), "closed_us": None, "opened_by": "admin",
                 "roster_enforced": bool(b.get("roster", "").strip()), "roster_size": 0, "purged": False}
            state["next_exam"] += 1
            state["exams"].append(e)
            for pc in state["pcs"].values():
                if pc["lab_id"] in e["labs"]:
                    pc["exam_id"] = e["id"]
                    if pc["status"] != "offline":
                        pc["status"] = "waiting"
                    pc_event(pc)
            broadcast({"ev": "exams", "exams": open_exams()})
            return self.send_json(e)
        if p.startswith("/api/exams/") and p.endswith("/close"):
            eid = int(p.split("/")[3])
            for e in state["exams"]:
                if e["id"] == eid:
                    e["closed_us"] = NOW()
            for pc in state["pcs"].values():
                if pc["exam_id"] == eid:
                    pc.update(exam_id=None, student_id=None)
                    if pc["status"] != "offline":
                        pc["status"] = "idle"
                    pc_event(pc)
            broadcast({"ev": "exams", "exams": open_exams()})
            return self.send_json({"ok": True})
        if p.startswith("/api/pcs/") and p.endswith("/assign"):
            mid = p.split("/")[3]
            lab = self.body()["lab_id"]
            subnet = state["pcs"][mid]["subnet"]
            moved = 0
            for pc in state["pcs"].values():
                if pc["subnet"] == subnet and pc["lab_id"] != lab:
                    pc["lab_id"] = lab
                    moved += 1
                    pc_event(pc)
            name = next(l["name"] for l in LABS if l["id"] == lab)
            broadcast({"ev": "toast", "level": "info", "text": f"Network {subnet} assigned to {name}: {moved} PC(s) moved", "lab_id": lab})
            return self.send_json({"moved": moved})
        if p.startswith("/api/pcs/") and p.endswith("/release"):
            pc = state["pcs"][p.split("/")[3]]
            pc.update(student_id=None, status="waiting")
            pc_event(pc)
            return self.send_json({"ok": True})
        self.send_json({"error": "not found"}, 404)

    def websocket(self):
        key = self.headers["Sec-WebSocket-Key"]
        accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
        self.send_response(101)
        self.send_header("Upgrade", "websocket")
        self.send_header("Connection", "Upgrade")
        self.send_header("Sec-WebSocket-Accept", accept)
        self.end_headers()
        conn = self.connection
        ws_send(conn, {"ev": "snapshot", "me": "admin", "labs": LABS, "pcs": list(state["pcs"].values()), "exams": open_exams()})
        with state["lock"]:
            state["clients"].append(conn)
        try:
            while True:  # read and discard client frames until it closes
                h = conn.recv(2)
                if len(h) < 2 or h[0] & 0x0F == 8:
                    break
                n = h[1] & 0x7F
                if n == 126:
                    n = struct.unpack(">H", conn.recv(2))[0]
                elif n == 127:
                    n = struct.unpack(">Q", conn.recv(8))[0]
                conn.recv(4 + n)
        except OSError:
            pass


if __name__ == "__main__":
    threading.Thread(target=simulator, daemon=True).start()
    print(f"dashboard mock on http://127.0.0.1:{PORT}  (password: password123)")
    ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
