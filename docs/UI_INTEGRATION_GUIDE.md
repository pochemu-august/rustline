# Руководство по разработке UI для Rustline

Благодаря модульной архитектуре Rustline, интерфейс для телефона можно построить **двумя способами**:

1. **Через WebSocket Control API** (`ws://127.0.0.1:7890`) — **универсальный способ**.
   - Интерфейс может быть написан на **любом языке и фреймворке**: React / Vue / Svelte, Electron, Tauri, Python (PyQt), Flutter, C# (.NET/WPF), Swift, Kotlin.
   - UI работает как отдельный процесс: если интерфейс закрывается или перезагружается, демон в фоне **не сбрасывает SIP-регистрацию и звонки**.
2. **Прямое встраивание ядра (`rustline-core`) в Rust-приложение** — для монолитных Rust GUI (например, `egui`, `Slint`, `Iced`).
   - Нет накладных расходов на сериализацию JSON и сеть.
   - Прямой вызов через Tokio-каналы (`EngineHandle`).

---

## Способ 1: Подключение через WebSocket (JSON API)

### 1. Жизненный цикл подключения

1. Клиент открывает соединение `ws://127.0.0.1:7890`.
2. Если в `rustline.json` включен токен авторизации, клиент отправляет:
   ```json
   { "command": "auth", "token": "ВАШ_ТОКЕН" }
   ```
3. Клиент запрашивает текущее состояние для первичной инициализации UI:
   ```json
   { "command": "get_status", "id": "req-1" }
   ```
4. Клиент слушает входящие сообщения в сокете:
   - **Ответы на команды**: содержат `id`, `ok: true/false`, `error`, `data`.
   - **Асинхронные события (push-уведомления)**: содержат поле `"event"`.

---

### 2. Спецификация JSON-протокола

#### Команды (Клиент → Демон)

##### `register` — Регистрация на SIP-сервере
```json
{
  "command": "register",
  "id": "1",
  "server": "192.168.0.104",
  "port": 5060,
  "username": "100",
  "password": "100password",
  "transport": "udp"
}
```
*Ответ успеха:*
```json
{ "id": "1", "ok": true }
```
*Ответ ошибки:*
```json
{ "id": "1", "ok": false, "error": "authentication failed: server returned 403" }
```

##### `unregister` — Отмена регистрации
```json
{
  "command": "unregister",
  "id": "2"
}
```
*Ответ:*
```json
{ "id": "2", "ok": true }
```

##### `get_status` — Запрос состояния
```json
{
  "command": "get_status",
  "id": "3"
}
```
*Ответ:*
```json
{
  "id": "3",
  "ok": true,
  "data": {
    "registration_state": "registered",
    "active_calls": []
  }
}
```

---

#### События (Демон → Клиент)

События приходят в любой момент, когда в ядре что-то меняется.

##### `registration_state_changed`
```json
// В процессе регистрации:
{
  "event": "registration_state_changed",
  "state": "registering"
}

// Успешно зарегистрирован:
{
  "event": "registration_state_changed",
  "state": "registered",
  "expires": 3599
}

// Ошибка регистрации:
{
  "event": "registration_state_changed",
  "state": "failed",
  "reason": "authentication failed: server returned 403"
}

// Дерегистрирован:
{
  "event": "registration_state_changed",
  "state": "unregistered"
}
```

##### `error`
```json
{
  "event": "error",
  "call_id": null,
  "message": "DNS resolution failed"
}
```

---

### 3. Готовый пример UI на JavaScript / TypeScript / React / Tauri

Стандартный браузерный `WebSocket` API:

```javascript
class RustlineClient {
  constructor(url = "ws://127.0.0.1:7890", authToken = "") {
    this.url = url;
    this.token = authToken;
    this.ws = null;
    this.callbacks = new Map();
    this.reqId = 1;
    this.onStateChange = null;
  }

  connect() {
    this.ws = new WebSocket(this.url);

    this.ws.onopen = () => {
      console.log("Подключено к Rustline Daemon");
      if (this.token) {
        this.send({ command: "auth", token: this.token });
      }
      // Синхронизируем состояние при входе
      this.getStatus();
    };

    this.ws.onmessage = (event) => {
      const msg = JSON.parse(event.data);

      // 1. Если это push-событие
      if (msg.event) {
        this.handleEvent(msg);
        return;
      }

      // 2. Если это ответ на команду
      if (msg.id && this.callbacks.has(msg.id)) {
        const resolve = this.callbacks.get(msg.id);
        this.callbacks.delete(msg.id);
        resolve(msg);
      }
    };

    this.ws.onclose = () => {
      console.warn("Соединение с демоном потеряно. Реконнект через 2с...");
      setTimeout(() => this.connect(), 2000);
    };
  }

  send(data) {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(data));
    }
  }

  callCommand(cmd, params = {}) {
    return new Promise((resolve) => {
      const id = String(this.reqId++);
      this.callbacks.set(id, resolve);
      this.send({ command: cmd, id, ...params });
    });
  }

  handleEvent(eventMsg) {
    console.log("Событие от ядра:", eventMsg);
    if (eventMsg.event === "registration_state_changed" && this.onStateChange) {
      this.onStateChange(eventMsg.state, eventMsg);
    }
  }

  // Публичные методы для UI
  register(server, port, username, password, transport = "udp") {
    return this.callCommand("register", { server, port: Number(port), username, password, transport });
  }

  unregister() {
    return this.callCommand("unregister");
  }

  getStatus() {
    return this.callCommand("get_status");
  }
}

// ── Пример использования в интерфейсе: ──────────────────────────────────────
const phone = new RustlineClient();

// Подписка на обновление статуса (перерисовка бейджа в UI)
phone.onStateChange = (state, details) => {
  const badge = document.getElementById("status-badge");
  badge.textContent = state; // "registered" | "registering" | "failed"
  badge.className = `status-${state}`;
};

phone.connect();

// Реакция на кнопку «Зарегистрироваться»
document.getElementById("btn-register").onclick = async () => {
  const res = await phone.register("192.168.0.104", 5060, "100", "100password");
  if (!res.ok) {
    alert("Ошибка: " + res.error);
  }
};
```

---

### 4. Готовый пример на Python (PyQt / Tkinter / скрипты)

```python
import asyncio
import json
import websockets

class RustlineBridge:
    def __init__(self, uri="ws://127.0.0.1:7890"):
        self.uri = uri
        self.websocket = None

    async def run(self):
        async with websockets.connect(self.uri) as ws:
            self.websocket = ws
            print("Connected to Rustline daemon")

            # Слушаем входящие сообщения и события
            async for raw in ws:
                msg = json.loads(raw)
                if "event" in msg:
                    print(f"EVENT: {msg['event']} -> {msg}")
                elif "ok" in msg:
                    print(f"RESPONSE: {msg}")

    async def register(self, server, port, user, password):
        req = {
            "command": "register",
            "id": "py-reg",
            "server": server,
            "port": port,
            "username": user,
            "password": password,
            "transport": "udp"
        }
        await self.websocket.send(json.dumps(req))

# Запуск
# asyncio.run(bridge.run())
```

---

## Способ 2: Прямое встраивание ядра в Rust GUI (egui / Slint)

Если вы пишете нативный интерфейс на **Rust**, можно подключить `rustline-core` напрямую через `Cargo.toml`:

```toml
[dependencies]
rustline-core = { path = "../rustline-core" }
tokio = { version = "1", features = ["full"] }
eframe = "0.29" # если используете egui
```

### Архитектурный паттерн для Rust GUI:

GUI-потоки (egui/Slint) обычно синхронные или работают в UI-лупе с постоянной перерисовкой кадров.  
Для взаимодействия с Tokio-ядром используется фоновый таск и `std::sync::mpsc` (или `tokio::sync`):

```rust
use rustline_core::engine::{Engine, EngineHandle};
use rustline_core::types::{CoreCommand, CoreEvent, RegistrationState, TransportType};
use tokio::sync::oneshot;

pub struct SoftphoneApp {
    handle: EngineHandle,
    state: RegistrationState,
    event_rx: tokio::sync::broadcast::Receiver<CoreEvent>,
}

impl SoftphoneApp {
    pub fn new() -> Self {
        // 1. Создаем движок ядра
        let (engine, handle) = Engine::new();
        let event_rx = handle.subscribe_events();

        // 2. Запускаем в Tokio runtime
        tokio::spawn(engine.run());

        Self {
            handle,
            state: RegistrationState::Unregistered,
            event_rx,
        }
    }

    pub fn update(&mut self) {
        // Опрашиваем события ядра без блокировки UI-потока
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                CoreEvent::RegistrationStateChanged { state } => {
                    self.state = state;
                }
                CoreEvent::Error { message, .. } => {
                    eprintln!("Ошибка: {message}");
                }
            }
        }
    }

    pub fn on_click_register(&self, server: String, user: String, pass: String) {
        let handle = self.handle.clone();
        tokio::spawn(async move {
            let (tx, rx) = oneshot::channel();
            let _ = handle.send_command(CoreCommand::Register {
                server,
                port: 5060,
                username: user,
                password: pass,
                transport: TransportType::Udp,
                response_tx: tx,
            }).await;

            let result = rx.await;
            println!("Результат регистрации: {result:?}");
        });
    }
}
```

---

## Резюме: что выбрать?

| Критерий | Способ 1: WebSocket Control API | Способ 2: Embedded `rustline-core` |
|---|---|---|
| **Язык разработки UI** | Любой (JS/TS, Python, C#, Dart, Swift) | Только Rust |
| **Выживаемость звонков** | Демон живёт независимо от падений/перезапусков GUI | Закрытие UI завершает процесс |
| **Сложность развёртывания**| Два процесса (демон + клиент) или запуск демона из GUI | Один неделимый бинарник |
| **Для чего идеально подходит**| Web, Electron, Tauri, системные треи, мобильные клиенты | Компактные утилиты на egui / Slint |
