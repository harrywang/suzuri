#!/usr/bin/env python3
"""A minimal stand-in for zed cloud, used by `script/collab-dev`.

The collab server validates every RPC connection by asking the cloud for the
user behind the `Authorization: <user_id> <token>` header. In development that
endpoint is `http://localhost:8787`; this server answers it for two fixed
users, so a local collab stack needs no zed.dev account and no network.

Endpoints:

  POST /internal/users/impersonate   client sign-in (Bearer admin token)
  GET  /client/users/me              collab connection validation
  GET  /healthz                      liveness
"""

import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ADMIN_TOKEN = "internal-api-key-secret"

# github_login -> (legacy_user_id, username, access_token)
USERS = {
    "alice": (1, "alice", "dev-token-alice"),
    "bob": (2, "bob", "dev-token-bob"),
}
BY_ID = {user_id: (login, token) for login, (user_id, _, token) in USERS.items()}


def authenticated_user(login, user_id):
    return {
        "user": {
            "id_v2": f"user_{login}",
            "legacy_user_id": user_id,
            "metrics_id": f"00000000-0000-0000-0000-{user_id:012d}",
            "username": login,
            "avatar_url": "",
            "github_login": login,
            "name": login.capitalize(),
            # The client only connects to collab automatically for staff
            # accounts (`sign_in_with_optional_connect`). Dev users are staff.
            "is_staff": True,
            "accepted_tos_at": None,
            "has_connected_to_collab_once": False,
        },
        "feature_flags": [],
        "organizations": [],
        "default_organization_id": None,
        "plans_by_organization": {},
        "configuration_by_organization": {},
        "plan": {
            "plan_v3": "zed_free",
            "subscription_period": None,
            "usage": {"edit_predictions": {"used": 0, "limit": "unlimited"}},
            "trial_started_at": None,
            "is_account_too_young": False,
            "has_overdue_invoices": False,
        },
    }


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        sys.stderr.write("dev-cloud: " + (fmt % args) + "\n")

    def send_json(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length) if length else b"{}"
        auth = self.headers.get("Authorization", "")
        if auth != f"Bearer {ADMIN_TOKEN}":
            self.send_json(401, {"error": "bad admin token"})
            return
        try:
            payload = json.loads(body)
        except Exception:
            self.send_json(400, {"error": "bad body"})
            return

        if self.path == "/internal/users/impersonate":
            login = payload.get("github_login")
            if login not in USERS:
                self.send_json(404, {"error": f"unknown user {login}"})
                return
            user_id, _, token = USERS[login]
            self.send_json(200, {"user_id": user_id, "access_token": token})
            return

        if self.path == "/internal/users/look_up_by_legacy_id":
            wanted = set(payload.get("legacy_user_ids", []))
            users = []
            for login, (user_id, _, _) in USERS.items():
                if user_id in wanted:
                    users.append({
                        "id": f"user_{login}",
                        "legacy_user_id": user_id,
                        "username": login,
                        "github_login": login,
                        "avatar_url": "",
                        "name": login.capitalize(),
                        "admin": True,
                        "connected_once": False,
                    })
            self.send_json(200, {"users": users})
            return

        self.send_json(404, {"error": "not found"})

    def do_GET(self):
        if self.path == "/healthz":
            self.send_json(200, {"ok": True})
            return
        if self.path != "/client/users/me":
            self.send_json(404, {"error": "not found"})
            return
        parts = self.headers.get("Authorization", "").split()
        if len(parts) != 2:
            self.send_json(401, {"error": "missing credentials"})
            return
        user_id_text, token = parts
        try:
            user_id = int(user_id_text)
        except ValueError:
            self.send_json(401, {"error": "bad user id"})
            return
        entry = BY_ID.get(user_id)
        if entry is None or entry[1] != token:
            self.send_json(401, {"error": "bad token"})
            return
        self.send_json(200, authenticated_user(entry[0], user_id))


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8787
    server = ThreadingHTTPServer(("0.0.0.0", port), Handler)
    print(f"dev-cloud listening on 0.0.0.0:{port}", flush=True)
    server.serve_forever()
