<p align="center">
  <h1 align="center">🔊 rustline</h1>
  <p align="center"><b>Pure Rust Headless SIP Softphone</b></p>
  <p align="center">
    A cross-platform SIP softphone engine written entirely in Rust 2024.<br/>
    No C/C++ FFI. No PJSIP. Just Rust.
  </p>
</p>

---

## What is rustline?

**rustline** is a programmatic SIP softphone designed as a headless daemon. It runs in the background and exposes a WebSocket JSON-RPC API that any UI can connect to — be it a web app, a desktop GUI, a mobile app, or a CLI tool.

### Why this architecture?

| Traditional Softphone | rustline |
|---|---|
| UI tightly coupled to SIP engine | **Engine and UI are completely separate** |
| Single platform (e.g. Windows-only MFC) | **Any UI on any platform** |
| C/C++ bindings (PJSIP, oSIP, etc.) | **Pure Rust — no FFI** |
| Monolithic binary | **Modular Cargo workspace** |
| Hard to embed in other apps | **JSON-RPC API — trivial to integrate** |

```
┌──────────────┐     WebSocket      ┌───────────────────────┐
│  Any UI App  │◄═══════════════════►│   rustline-daemon     │
│              │  ws://127.0.0.1:   │                       │
│  - React     │       7890         │  ┌─────────────────┐  │
│  - Electron  │   JSON-RPC 2.0    │  │  rustline-core  │  │
│  - Qt / GTK  │                    │  │  (SIP Engine)   │  │
│  - Tkinter   │                    │  └────────┬────────┘  │
│  - CLI       │                    │  ┌────────▼────────┐  │
│              │                    │  │ rustline-media  │  │
│              │                    │  │ (Audio / RTP)   │  │
└──────────────┘                    │  └─────────────────┘  │
                                    └───────────────────────┘
```

## Project Status

🚧 **Active Development — MVP Phase**

| Component | Status |
|---|---|
| WebSocket JSON-RPC server | ✅ Working |
| Protocol types (commands, events) | ✅ Working |
| Account state machine | ✅ Working (mock SIP) |
| Call state machine | ✅ Working (mock SIP) |
| Real SIP REGISTER/INVITE | 🔧 Next milestone |
| Audio I/O (cpal) | 📋 Planned |
| RTP / SRTP | 📋 Planned |
| NAT traversal (STUN/ICE) | 📋 Planned |

## Workspace Structure

```
rustline/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── rustline-proto/           # JSON-RPC protocol types (serde)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── commands.rs       # Client → Daemon commands
│   │       ├── events.rs         # Daemon → Client events
│   │       └── messages.rs       # JSON-RPC 2.0 wire format
│   ├── rustline-core/            # SIP engine & state machines
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── account.rs        # Registration state machine
│   │       ├── call.rs           # Call state machine (6 states)
│   │       └── engine.rs         # Command dispatcher
│   ├── rustline-media/           # Audio & RTP stubs
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── audio.rs          # cpal I/O (planned)
│   │       └── rtp.rs            # RTP pipeline (planned)
│   └── rustline-daemon/          # WebSocket server (main binary)
│       └── src/
│           └── main.rs           # Tokio + tungstenite server
├── docs/
│   └── UI_INTEGRATION_GUIDE.md   # Full API documentation
└── msc/                          # MicroSIP sources (reference only)
```

## Building

**Prerequisites:** Rust 1.85+ (edition 2024)

```bash
# Build all crates
cargo build --workspace

# Run tests
cargo test --workspace

# Run the daemon
cargo run -p rustline-daemon
```

## Quick Start

### 1. Start the daemon

```bash
cargo run -p rustline-daemon
```

You'll see:
```
INFO rustline_daemon: rustline-daemon v0.1.0
INFO rustline_daemon: WebSocket server listening on ws://127.0.0.1:7890
```

### 2. Connect from any WebSocket client

**Using [websocat](https://github.com/vi/websocat):**

```bash
websocat ws://127.0.0.1:7890
```

Then send a register command:

```json
{"jsonrpc":"2.0","id":1,"method":"register","params":{"server":"sip.example.com","username":"100","password":"secret"}}
```

**Response:**

```json
{"jsonrpc":"2.0","id":1,"result":{"status":"registered","message":"SIP account registered successfully (mock)"}}
```

### 3. Build your own UI

See [`docs/UI_INTEGRATION_GUIDE.md`](docs/UI_INTEGRATION_GUIDE.md) for the full
API reference, state machine diagrams, and integration examples in JavaScript
and Python.

## Design Decisions

### Pure Rust (No C/C++ FFI)

We intentionally avoid PJSIP, oSIP, and other C libraries. The SIP stack is
being implemented from scratch using the [`rsip`](https://crates.io/crates/rsip)
crate for SIP message parsing. This gives us:

- **Memory safety** by default
- **No build complexity** (no CMake, no vcpkg, no pkg-config)
- **Cross-compilation** works out of the box
- **Audit-friendly** codebase

### MicroSIP as Reference

The `msc/` directory contains MicroSIP (C++/PJSIP) source code used
**exclusively as reference documentation** for reverse-engineering:

- SIP registration timers (`registerRefresh=300s`, `keepAlive=15s`)
- Codec priority defaults (`PCMA/PCMU` by default)
- Call state machine (6 states matching `PJSIP_INV_STATE_*`)
- NAT traversal patterns (STUN, Via rewrite, Contact rewrite)

We do **not** use any MicroSIP code at runtime.

## API Overview

| Command | Description |
|---------|-------------|
| `register` | Register a SIP account |
| `unregister` | Unregister the current account |
| `dial` | Make an outgoing call |
| `answer` | Answer an incoming call |
| `hangup` | End a call |
| `hold` | Toggle hold |
| `dtmf` | Send DTMF tones |
| `get_status` | Query daemon status |

| Event | Description |
|-------|-------------|
| `registration_state_changed` | Account state changed |
| `call_state_changed` | Call state transition |
| `incoming_call` | New incoming call |

## License

This project is licensed under the [GNU General Public License v3.0](LICENSE).

## Contributing

Contributions are welcome! Please check the existing issues or open a new one
to discuss your idea before submitting a PR.
