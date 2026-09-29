# Rustline

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Rust](https://img.shields.io/badge/rust-2021%20edition-orange.svg)](https://www.rust-lang.org)
[![Status: WIP](https://img.shields.io/badge/status-work--in--progress-yellow.svg)]()

**Rustline** — модульный асинхронный SIP/VoIP софтфон и фоновый демон на **Rust**.

Проект спроектирован с упором на строгое разделение ответственности: телефонное ядро (**Core**) не знает ничего про графический интерфейс или формат сериализации. Поверх ядра можно строить любой клиент — CLI, TUI, веб-интерфейс, десктопное приложение (Tauri, egui, Slint) или мобильный клиент.

---

## 🏛 Архитектура

```text
 ┌─────────────────────────────────────────────────────────────┐
 │                    Любой пользовательский интерфейс         │
 │           CLI / TUI / Tauri / egui / Web / Mobile           │
 └──────────────────────────────┬──────────────────────────────┘
                                │
        WebSocket (JSON)        │  (или прямое встраивание как библиотеки)
                                ▼
 ┌─────────────────────────────────────────────────────────────┐
 │                      rustline-daemon                        │
 │    Фоновый сервис (Control API, WebSocket: 127.0.0.1:7890)  │
 └──────────────────────────────┬──────────────────────────────┘
                                │  tokio::sync::mpsc (CoreCommand)
                                │  tokio::sync::broadcast (CoreEvent)
                                ▼
 ┌─────────────────────────────────────────────────────────────┐
 │                      rustline-core                          │
 │         SIP-сигнализация (RFC 3261), Digest Auth,           │
 │       RTP-потоки, G.711 / Opus кодеки, управление звонками   │
 ```

> 📖 **Подробное руководство по созданию интерфейса:** см. [Руководство по разработке UI (docs/UI_INTEGRATION_GUIDE.md)](docs/UI_INTEGRATION_GUIDE.md) — готовые примеры на JS/TS/React/Tauri, Python и Rust (egui).


1. **`rustline-core`** (ядро):
   - Отвечает только за SIP/RTP-логику.
   - Никаких зависимостей от WebSocket, JSON или GUI.
   - Взаимодействие через каналы Tokio (`mpsc` для команд, `broadcast` для событий, `oneshot` для ответов).
   - Может подключаться как независимая Rust-библиотека в любой процесс.
2. **`rustline-daemon`** (Control API, в разработке):
   - Управляющий слой поверх ядра.
   - Поднимает локальный WebSocket на `127.0.0.1:7890`.
   - Принимает команды в JSON, рассылает события всем подключённым клиентам в реальном времени.
   - Простая защита по токену авторизации.
3. **Клиенты / UI**:
   - Любое внешнее приложение может управлять телефоном, отправляя простые JSON-команды в WebSocket.

---

## 🚀 Текущее состояние (WIP)

Проект находится в активной разработке. На данный момент реализован и протестирован базовый этап SIP-сигнализации:

- [x] **SIP REGISTER**:
  - Полный цикл транзакции: `initial REGISTER` → `401 Unauthorized` → `authenticated REGISTER` → `200 OK`.
  - Корректная процедура отмены регистрации (`UNREGISTER` с `Expires: 0`).
  - Проверено и протестировано на боевом **Asterisk PBX 22.x**.
- [x] **Digest-аутентификация (RFC 2617)**:
  - Реализация MD5 без внешних тяжелых C-библиотек.
  - Поддержка современного режима с защитой от повторов (`qop="auth"`, `cnonce`, `nc=00000001`).
  - Поддержка legacy-режима (без `qop`).
- [x] **Парсер и сериализатор SIP (RFC 3261)**:
  - Полностью собственный легковесный парсер строк/байтов.
  - Регистронезависимые заголовки, разбор многострочных заголовков (folding).
  - Автоматический расчет `Content-Length`.
  - Генерация корректных `Call-ID`, `branch` (`z9hG4bK...`) и `tag`.
- [x] **Сетевой транспорт (UDP)**:
  - Автоматическое определение локального сетевого интерфейса (`discover_local_ip`) для корректных заголовков `Via` и `Contact` на многосетевых хостах.
  - Асинхронный резолвинг DNS.
- [x] **Control API (WebSocket + JSON)**:
  - Фоновый сервис `rustlined` на `127.0.0.1:7890`.
  - Двусторонний JSON-протокол: команды (`register`, `unregister`, `get_status`) и мгновенные push-события.
  - Поддержка аутентификации по токену (`{"command": "auth", "token": "..."}`).
- [x] **Референсный CLI-клиент**:
  - Интерактивная консоль `rustline-cli` для управления демоном по WebSocket.
- [x] **Исходящие вызовы (Outgoing INVITE, ACK, BYE)**:
  - Диалоговый автомат: `INVITE` → `401/407 Challenge` → `authenticated INVITE` → `180 Ringing` → `200 OK` → `ACK` → `Active`.
  - Завершение сессии: `BYE` → переход состояния в `Ended` и освобождение медиапортов.
- [x] **SDP (Session Description Protocol, RFC 4566)**:
  - Формирование локального SDP Offer (PCMU 0, PCMA 8, telephone-event 101).
  - Парсинг удаленного SDP Answer с автоматическим определением IP, медиапорта и кодека.
- [x] **RTP-стек (RFC 3550) и кодеки G.711 (PCMU / PCMA)**:
  - Сериализация и разбор RTP-пакетов (12-байтный заголовок, sequence number, timestamp, SSRC).
  - Асинхронные задачи отправки 20 мс аудиофреймов (160 сэмплов при 8 кГц) и приема входящего потока.
  - Генератор тестового тона 440 Гц для сквозной проверки эхо-тестов PBX (*43) и других пиров.
- [ ] **Кроссплатформенный захват и воспроизведение звука со звуковой карты (cpal)**
- [ ] **Входящие вызовы (Incoming INVITE / Ringing / Answer)**

---

## 📂 Структура репозитория

```
rustline/
├── Cargo.toml                  # Манифест Cargo-воркспейса
├── LICENSE                     # GNU General Public License v3.0
├── README.md                   # Документация проекта
└── crates/
    └── rustline-core/          # Ядро SIP/VoIP
        ├── Cargo.toml
        ├── examples/
        │   └── test_register.rs# Консольный тест регистрации
        └── src/
            ├── lib.rs          # Публичный API ядра
            ├── types.rs        # CoreCommand, CoreEvent, RegistrationState
            ├── engine.rs       # Асинхронный цикл Engine и EngineHandle
            └── sip/
                ├── mod.rs
                ├── message.rs  # Парсинг и сборка SIP-сообщений
                ├── auth.rs     # RFC 2617 Digest-аутентификация
                ├── transport.rs# UDP-транспорт и автоопределение IP
                └── register.rs # Логика транзакции REGISTER
```

---

## 🛠 Быстрый старт

### Требования
- [Rust](https://www.rust-lang.org/tools/install) (версия 1.75 или новее)

### Сборка и запуск тестов

```bash
# Клонировать репозиторий
git clone https://github.com/your-username/rustline.git
cd rustline

# Запустить unit-тесты
cargo test
```

### Запуск фонового демона (`rustlined`)

Демон запускает SIP-движок в фоне и открывает управляющий WebSocket API:

```bash
cargo run -p rustline-daemon
# или с указанием своего конфига:
cargo run -p rustline-daemon -- path/to/rustline.json
```

Параметры сервера задаются в `rustline.json` (порт, адрес, токен авторизации).

### Запуск интерактивного CLI-клиента (`rustline-cli`)

В отдельном терминале запустите клиент:

```bash
cargo run -p rustline-cli
# или с указанием адреса демона:
cargo run -p rustline-cli -- ws://127.0.0.1:7890
```

Команды в интерактивной консоли CLI:
- `register <server> <port> <username> <password> [transport]` — зарегистрироваться на SIP-сервере
- `unregister` — отменить регистрацию
- `status` — получить статус регистрации и активных звонков
- `auth <token>` — авторизоваться (если в конфиге демона задан токен)
- `help` — список доступных команд
- `exit` — выход

Все события от сервера (например, изменение статуса регистрации) выводятся в консоль мгновенно.

### Прямой тест регистрации (без WebSocket)

Для проверки работы ядра подготовлен пример `test_register`:

```bash
cargo run --example test_register -- <SERVER_IP> <PORT> <USERNAME> <PASSWORD>
```

Пример для локального Asterisk на порту 5060:

```bash
cargo run --example test_register -- 192.168.0.104 5060 100 100password
```

С подробным логированием SIP-пакетов:

```bash
# Linux / macOS:
RUST_LOG=debug cargo run --example test_register -- 192.168.0.104 5060 100 100password

# Windows (PowerShell):
$env:RUST_LOG="debug"; cargo run --example test_register -- 192.168.0.104 5060 100 100password
```

---

## 💻 Пример использования ядра в коде

Ядро можно использовать как обычную Rust-библиотеку внутри любого приложения без промежуточных сетевых слоёв:

```rust
use rustline_core::engine::Engine;
use rustline_core::types::{CoreCommand, TransportType};
use tokio::sync::oneshot;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Создаем ядро и дескриптор управления
    let (engine, handle) = Engine::new();
    tokio::spawn(engine.run());

    // 2. Подписываемся на события (изменения состояний, звонки и т.д.)
    let mut events = handle.subscribe_events();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            println!("Событие ядра: {:?}", event);
        }
    });

    // 3. Отправляем команду регистрации
    let (tx, rx) = oneshot::channel();
    handle.send_command(CoreCommand::Register {
        server: "192.168.0.104".into(),
        port: 5060,
        username: "100".into(),
        password: "secretpassword".into(),
        transport: TransportType::Udp,
        response_tx: tx,
    }).await?;

    // Ждем результат регистрации
    match rx.await? {
        Ok(()) => println!("Успешно зарегистрирован!"),
        Err(err) => eprintln!("Ошибка регистрации: {}", err),
    }

    Ok(())
}
```

---

## 📄 Лицензия

Проект распространяется под лицензией **GNU General Public License v3.0 (GPL-3.0)**.  
Подробности см. в файле [LICENSE](LICENSE).
