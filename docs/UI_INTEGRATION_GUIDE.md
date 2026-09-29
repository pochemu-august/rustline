# UI Integration Guide

> **rustline** — Pure Rust Headless SIP Softphone

This guide explains how to connect any UI client (web, desktop, mobile) to the
`rustline-daemon` via its WebSocket JSON-RPC API.

---

## Table of Contents

- [Architecture Overview](#architecture-overview)
- [Connection Details](#connection-details)
- [Wire Protocol (JSON-RPC 2.0)](#wire-protocol-json-rpc-20)
- [Commands (Client → Daemon)](#commands-client--daemon)
  - [register](#register)
  - [unregister](#unregister)
  - [dial](#dial)
  - [answer](#answer)
  - [hangup](#hangup)
  - [hold](#hold)
  - [dtmf](#dtmf)
  - [get_status](#get_status)
- [Events (Daemon → Client)](#events-daemon--client)
  - [registration_state_changed](#registration_state_changed)
  - [call_state_changed](#call_state_changed)
  - [incoming_call](#incoming_call)
- [State Machines](#state-machines)
  - [Registration States](#registration-states)
  - [Call States](#call-states)
- [Error Handling](#error-handling)
- [Integration Examples](#integration-examples)
  - [JavaScript / TypeScript (Browser)](#javascript--typescript-browser)
  - [Python (Tkinter + asyncio)](#python-tkinter--asyncio)

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                        rustline System                          │
│                                                                 │
│  ┌──────────────┐     WebSocket      ┌───────────────────────┐  │
│  │  Any UI App  │◄══════════════════►│   rustline-daemon     │  │
│  │              │  ws://127.0.0.1:   │                       │  │
│  │  - Web (JS)  │       7890         │  ┌─────────────────┐  │  │
│  │  - Desktop   │   JSON-RPC 2.0    │  │  rustline-core  │  │  │
│  │  - Mobile    │                    │  │  (SIP Engine)   │  │  │
│  │  - CLI       │                    │  └────────┬────────┘  │  │
│  └──────────────┘                    │           │           │  │
│                                      │  ┌────────▼────────┐  │  │
│                                      │  │ rustline-media  │  │  │
│                                      │  │ (Audio / RTP)   │  │  │
│                                      │  └─────────────────┘  │  │
│                                      └───────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

**Key Concepts:**

- **Headless Architecture**: The SIP engine runs as a background daemon with no
  built-in UI. Any frontend connects via a standard WebSocket.
- **UI Agnostic**: Build your UI in any language/framework — React, Qt, Electron,
  Tkinter, SwiftUI, or even a CLI.
- **Multiple Clients**: Multiple UI clients can connect simultaneously. All receive
  the same event broadcasts.

---

## Connection Details

| Property | Value |
|----------|-------|
| **Protocol** | WebSocket (RFC 6455) |
| **Default URL** | `ws://127.0.0.1:7890` |
| **Message Format** | JSON-RPC 2.0 (text frames) |
| **Authentication** | None (localhost only) |

---

## Wire Protocol (JSON-RPC 2.0)

All messages use the [JSON-RPC 2.0](https://www.jsonrpc.org/specification) format.

### Request (Client → Daemon)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "register",
  "params": {
    "server": "sip.example.com",
    "username": "100",
    "password": "secret"
  }
}
```

### Success Response (Daemon → Client)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "status": "registered",
    "message": "SIP account registered successfully"
  }
}
```

### Error Response (Daemon → Client)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "error": {
    "code": -32602,
    "message": "Invalid params: missing field `server`"
  }
}
```

### Event Notification (Daemon → Client, no `id`)

```json
{
  "jsonrpc": "2.0",
  "method": "event",
  "params": {
    "event": "registration_state_changed",
    "data": {
      "state": "registered",
      "code": 200,
      "reason": "OK"
    }
  }
}
```

---

## Commands (Client → Daemon)

### `register`

Register a SIP account with the server.

**Parameters:**

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `server` | string | ✅ | — | SIP server hostname or IP |
| `username` | string | ✅ | — | SIP username / extension |
| `password` | string | ✅ | — | SIP password |
| `domain` | string | ❌ | `server` | SIP domain |
| `display_name` | string | ❌ | `""` | Display name for From header |
| `transport` | string | ❌ | `"udp"` | Transport: `"udp"`, `"tcp"`, `"tls"` |
| `port` | number | ❌ | 5060/5061 | SIP server port |
| `register_refresh` | number | ❌ | 300 | REGISTER refresh interval (seconds) |
| `keep_alive` | number | ❌ | 15 | UDP keep-alive interval (seconds) |

**Example:**

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "register",
  "params": {
    "server": "sip.example.com",
    "username": "100",
    "password": "secret",
    "display_name": "John Doe"
  }
}
```

### `unregister`

Unregister the current SIP account.

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "unregister",
  "params": {}
}
```

### `dial`

Initiate an outgoing call.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `target` | string | ✅ | SIP URI or phone number |

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "dial",
  "params": {
    "target": "sip:200@sip.example.com"
  }
}
```

### `answer`

Answer an incoming call.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `call_id` | string | ✅ | The call ID from `incoming_call` event |

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "answer",
  "params": {
    "call_id": "a1b2c3d4-..."
  }
}
```

### `hangup`

End an active or ringing call.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `call_id` | string | ✅ | The call ID |

```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "method": "hangup",
  "params": {
    "call_id": "a1b2c3d4-..."
  }
}
```

### `hold`

Toggle hold on an active call.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `call_id` | string | ✅ | The call ID |

```json
{
  "jsonrpc": "2.0",
  "id": 6,
  "method": "hold",
  "params": {
    "call_id": "a1b2c3d4-..."
  }
}
```

### `dtmf`

Send DTMF tones during an active call.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `call_id` | string | ✅ | The call ID |
| `digits` | string | ✅ | DTMF digits (e.g. `"123#"`) |

```json
{
  "jsonrpc": "2.0",
  "id": 7,
  "method": "dtmf",
  "params": {
    "call_id": "a1b2c3d4-...",
    "digits": "1234#"
  }
}
```

### `get_status`

Query the current daemon status.

```json
{
  "jsonrpc": "2.0",
  "id": 8,
  "method": "get_status",
  "params": {}
}
```

**Response:**

```json
{
  "jsonrpc": "2.0",
  "id": 8,
  "result": {
    "registration": "registered",
    "active_calls": 1,
    "calls": [...]
  }
}
```

---

## Events (Daemon → Client)

Events are pushed as JSON-RPC notifications (no `id` field).

### `registration_state_changed`

```json
{
  "jsonrpc": "2.0",
  "method": "event",
  "params": {
    "event": "registration_state_changed",
    "data": {
      "state": "registered",
      "code": 200,
      "reason": "OK"
    }
  }
}
```

**Possible `state` values:** `"unregistered"`, `"registering"`, `"registered"`, `"failed"`

### `call_state_changed`

```json
{
  "jsonrpc": "2.0",
  "method": "event",
  "params": {
    "event": "call_state_changed",
    "data": {
      "call_id": "a1b2c3d4-...",
      "state": "confirmed",
      "direction": "outbound",
      "remote_name": "Alice",
      "remote_uri": "sip:200@example.com",
      "duration_secs": 45,
      "code": 200,
      "reason": "OK"
    }
  }
}
```

**Possible `state` values:** `"calling"`, `"incoming"`, `"early"`, `"connecting"`, `"confirmed"`, `"held"`, `"disconnected"`

### `incoming_call`

```json
{
  "jsonrpc": "2.0",
  "method": "event",
  "params": {
    "event": "incoming_call",
    "data": {
      "call_id": "a1b2c3d4-...",
      "caller_name": "Bob",
      "caller_uri": "sip:300@example.com"
    }
  }
}
```

---

## State Machines

### Registration States

```
┌──────────────┐  register   ┌─────────────┐  200 OK  ┌────────────┐
│ Unregistered │────────────►│ Registering  │────────►│ Registered │
└──────┬───────┘             └──────┬──────┘          └─────┬──────┘
       ▲                            │ 4xx/5xx               │
       │                            ▼                       │ unregister
       │                     ┌──────────┐                   │
       │◄────────────────────│  Failed  │                   │
       │                     └──────────┘                   │
       ▲                                                    │
       └────────────────────────────────────────────────────┘
```

### Call States

```
Outgoing:  Idle → Calling → Early → Connecting → Confirmed ⇄ Held → Disconnected
Incoming:  Idle → Incoming → Connecting → Confirmed ⇄ Held → Disconnected
```

---

## Error Handling

Standard JSON-RPC 2.0 error codes:

| Code | Meaning |
|------|---------|
| -32700 | Parse error (malformed JSON) |
| -32600 | Invalid request |
| -32601 | Method not found |
| -32602 | Invalid params |
| -32603 | Internal error |

---

## Integration Examples

### JavaScript / TypeScript (Browser)

```javascript
// Connect to rustline-daemon
const ws = new WebSocket('ws://127.0.0.1:7890');
let requestId = 0;

// Helper: send a JSON-RPC request
function sendRequest(method, params = {}) {
  const id = ++requestId;
  const request = {
    jsonrpc: '2.0',
    id,
    method,
    params,
  };
  ws.send(JSON.stringify(request));
  return id;
}

// Handle incoming messages
ws.onmessage = (event) => {
  const msg = JSON.parse(event.data);

  // Event notification (no id)
  if (msg.method === 'event') {
    const { event: eventName, data } = msg.params;
    switch (eventName) {
      case 'registration_state_changed':
        console.log(`Registration: ${data.state}`);
        updateRegistrationUI(data.state);
        break;
      case 'incoming_call':
        console.log(`Incoming call from ${data.caller_name} (${data.caller_uri})`);
        showIncomingCallDialog(data);
        break;
      case 'call_state_changed':
        console.log(`Call ${data.call_id}: ${data.state}`);
        updateCallUI(data);
        break;
    }
    return;
  }

  // Response to our request
  if (msg.result) {
    console.log(`Response [${msg.id}]:`, msg.result);
  } else if (msg.error) {
    console.error(`Error [${msg.id}]:`, msg.error.message);
  }
};

ws.onopen = () => {
  console.log('Connected to rustline-daemon');

  // Register a SIP account
  sendRequest('register', {
    server: 'sip.example.com',
    username: '100',
    password: 'secret',
    display_name: 'John Doe',
  });
};

// Make a call
function makeCall(target) {
  sendRequest('dial', { target });
}

// Answer an incoming call
function answerCall(callId) {
  sendRequest('answer', { call_id: callId });
}

// Hang up
function hangUp(callId) {
  sendRequest('hangup', { call_id: callId });
}
```

### Python (Tkinter + asyncio)

```python
"""
rustline UI client example using Tkinter + asyncio + websockets.

Requirements:
    pip install websockets

Usage:
    python rustline_ui.py
"""

import asyncio
import json
import threading
import tkinter as tk
from tkinter import ttk, messagebox

try:
    import websockets
except ImportError:
    print("Install websockets: pip install websockets")
    raise


class RustlineClient:
    """Async WebSocket client for rustline-daemon."""

    def __init__(self, url: str = "ws://127.0.0.1:7890"):
        self.url = url
        self.ws = None
        self.request_id = 0
        self.on_event = None  # callback: (event_name, data) -> None
        self.on_response = None  # callback: (id, result_or_error) -> None

    async def connect(self):
        self.ws = await websockets.connect(self.url)
        asyncio.create_task(self._listen())

    async def _listen(self):
        try:
            async for message in self.ws:
                msg = json.loads(message)
                if msg.get("method") == "event":
                    params = msg["params"]
                    if self.on_event:
                        self.on_event(params["event"], params["data"])
                elif "result" in msg:
                    if self.on_response:
                        self.on_response(msg["id"], msg["result"])
                elif "error" in msg:
                    if self.on_response:
                        self.on_response(msg["id"], msg["error"])
        except websockets.ConnectionClosed:
            pass

    async def send(self, method: str, params: dict = None) -> int:
        self.request_id += 1
        request = {
            "jsonrpc": "2.0",
            "id": self.request_id,
            "method": method,
            "params": params or {},
        }
        await self.ws.send(json.dumps(request))
        return self.request_id

    async def register(self, server: str, username: str, password: str, **kwargs):
        return await self.send("register", {
            "server": server,
            "username": username,
            "password": password,
            **kwargs,
        })

    async def dial(self, target: str):
        return await self.send("dial", {"target": target})

    async def answer(self, call_id: str):
        return await self.send("answer", {"call_id": call_id})

    async def hangup(self, call_id: str):
        return await self.send("hangup", {"call_id": call_id})


class App:
    """Tkinter GUI for rustline."""

    def __init__(self):
        self.root = tk.Tk()
        self.root.title("rustline — SIP Softphone")
        self.root.geometry("400x500")

        self.client = RustlineClient()
        self.loop = None
        self.current_call_id = None

        self._build_ui()
        self._start_async()

    def _build_ui(self):
        # -- Status --
        frame_status = ttk.LabelFrame(self.root, text="Status", padding=10)
        frame_status.pack(fill="x", padx=10, pady=5)
        self.lbl_status = ttk.Label(frame_status, text="Disconnected", font=("", 12))
        self.lbl_status.pack()

        # -- Account --
        frame_account = ttk.LabelFrame(self.root, text="SIP Account", padding=10)
        frame_account.pack(fill="x", padx=10, pady=5)

        ttk.Label(frame_account, text="Server:").grid(row=0, column=0, sticky="w")
        self.ent_server = ttk.Entry(frame_account, width=30)
        self.ent_server.grid(row=0, column=1)
        self.ent_server.insert(0, "sip.example.com")

        ttk.Label(frame_account, text="Username:").grid(row=1, column=0, sticky="w")
        self.ent_user = ttk.Entry(frame_account, width=30)
        self.ent_user.grid(row=1, column=1)

        ttk.Label(frame_account, text="Password:").grid(row=2, column=0, sticky="w")
        self.ent_pass = ttk.Entry(frame_account, width=30, show="*")
        self.ent_pass.grid(row=2, column=1)

        self.btn_register = ttk.Button(
            frame_account, text="Register", command=self._on_register
        )
        self.btn_register.grid(row=3, column=0, columnspan=2, pady=5)

        # -- Dialer --
        frame_dialer = ttk.LabelFrame(self.root, text="Dialer", padding=10)
        frame_dialer.pack(fill="x", padx=10, pady=5)

        self.ent_target = ttk.Entry(frame_dialer, width=30)
        self.ent_target.pack(side="left", padx=5)
        self.btn_call = ttk.Button(
            frame_dialer, text="Call", command=self._on_call
        )
        self.btn_call.pack(side="left")
        self.btn_hangup = ttk.Button(
            frame_dialer, text="Hangup", command=self._on_hangup, state="disabled"
        )
        self.btn_hangup.pack(side="left", padx=5)

        # -- Log --
        frame_log = ttk.LabelFrame(self.root, text="Log", padding=10)
        frame_log.pack(fill="both", expand=True, padx=10, pady=5)
        self.txt_log = tk.Text(frame_log, height=10, state="disabled", font=("Consolas", 9))
        self.txt_log.pack(fill="both", expand=True)

    def _log(self, text: str):
        self.txt_log.config(state="normal")
        self.txt_log.insert("end", text + "\n")
        self.txt_log.see("end")
        self.txt_log.config(state="disabled")

    def _start_async(self):
        self.loop = asyncio.new_event_loop()
        thread = threading.Thread(target=self._run_loop, daemon=True)
        thread.start()
        self.loop.call_soon_threadsafe(
            asyncio.ensure_future, self._connect(), 
        )

    def _run_loop(self):
        asyncio.set_event_loop(self.loop)
        self.loop.run_forever()

    async def _connect(self):
        try:
            self.client.on_event = self._on_event
            self.client.on_response = self._on_response
            await self.client.connect()
            self.root.after(0, lambda: self.lbl_status.config(text="Connected"))
            self.root.after(0, lambda: self._log("Connected to daemon"))
        except Exception as e:
            self.root.after(0, lambda: self._log(f"Connection failed: {e}"))

    def _on_event(self, event_name, data):
        self.root.after(0, lambda: self._handle_event(event_name, data))

    def _on_response(self, req_id, result):
        self.root.after(0, lambda: self._log(f"Response [{req_id}]: {result}"))

    def _handle_event(self, event_name, data):
        self._log(f"Event: {event_name} → {json.dumps(data, indent=2)}")
        if event_name == "registration_state_changed":
            self.lbl_status.config(text=data["state"].capitalize())
        elif event_name == "incoming_call":
            name = data.get("caller_name", data["caller_uri"])
            if messagebox.askyesno("Incoming Call", f"Call from {name}. Answer?"):
                self.current_call_id = data["call_id"]
                asyncio.run_coroutine_threadsafe(
                    self.client.answer(data["call_id"]), self.loop
                )
        elif event_name == "call_state_changed":
            if data["state"] == "disconnected":
                self.btn_hangup.config(state="disabled")
                self.current_call_id = None

    def _on_register(self):
        asyncio.run_coroutine_threadsafe(
            self.client.register(
                self.ent_server.get(),
                self.ent_user.get(),
                self.ent_pass.get(),
            ),
            self.loop,
        )

    def _on_call(self):
        target = self.ent_target.get().strip()
        if target:
            self.btn_hangup.config(state="normal")
            asyncio.run_coroutine_threadsafe(
                self._do_call(target), self.loop
            )

    async def _do_call(self, target):
        await self.client.dial(target)

    def _on_hangup(self):
        if self.current_call_id:
            asyncio.run_coroutine_threadsafe(
                self.client.hangup(self.current_call_id), self.loop
            )

    def run(self):
        self.root.mainloop()


if __name__ == "__main__":
    App().run()
```

---

## Next Steps

- [ ] Implement real SIP REGISTER via `rsip` transport
- [ ] Add audio pipeline with `cpal`
- [ ] Add SRTP/DTLS support
- [ ] Add SIP presence / BLF subscriptions
- [ ] Add TLS transport support
