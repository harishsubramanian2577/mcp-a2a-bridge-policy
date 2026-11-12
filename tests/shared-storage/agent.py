# Copyright 2026 Salesforce, Inc. All rights reserved.
"""Mock A2A v1 agent. Records every SendMessage and answers with a completed task.

A message without a contextId starts a new context ("ctx-1", "ctx-2", ...); a message with one
continues it. A message whose text starts with "slow" is answered after SLOW_SECONDS, so a test can
end the session while the call is in flight. GET /requests returns what was received; DELETE
/requests clears it.
"""
import itertools
import json
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

received = []
contexts = itertools.count(1)
SLOW_SECONDS = 4


class Agent(BaseHTTPRequestHandler):
    def _send(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        self._send(200, received)

    def do_DELETE(self):
        received.clear()
        self._send(200, [])

    def _body(self):
        if "content-length" in self.headers:
            return self.rfile.read(int(self.headers["content-length"]))
        # The policy drops content-length when it rewrites the body, so Envoy sends it chunked.
        body = b""
        while (size := int(self.rfile.readline().split(b";")[0], 16)) > 0:
            body += self.rfile.read(size)
            self.rfile.readline()
        self.rfile.readline()
        return body

    def do_POST(self):
        request = json.loads(self._body())
        message = request.get("params", {}).get("message", {})
        received.append({"path": self.path, "message": message})
        context_id = message.get("contextId") or f"ctx-{next(contexts)}"
        text = message.get("parts", [{}])[0].get("text", "")
        if text.startswith("slow"):
            time.sleep(SLOW_SECONDS)
        self._send(200, {
            "jsonrpc": "2.0",
            "id": request.get("id"),
            "result": {"task": {
                "id": f"task-{len(received)}",
                "contextId": context_id,
                "status": {
                    "state": "TASK_STATE_COMPLETED",
                    "message": {"messageId": f"m-{len(received)}", "role": "ROLE_AGENT",
                                "parts": [{"text": f"echo: {text}"}]},
                },
            }},
        })

    def log_message(self, *args):
        pass


ThreadingHTTPServer(("0.0.0.0", 8080), Agent).serve_forever()
