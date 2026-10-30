# OpacusR

**Открытый удалённый рабочий стол** — трансляция экрана, управление, передача файлов, чат и терминал поверх самохостинговой инфраструктуры. Написано на Rust: аналог TeamViewer / AnyDesk, где серверы — *ваши*.

> 🇬🇧 English documentation: [README.md](README.md)

- **Лицензия:** AGPL-3.0
- **Статус:** v0.1.0 — рабочее ядро, активная разработка
- **Платформы:** Linux (X11), Windows, macOS (частично), Android/iOS (каркас)
- **Репозиторий:** [github.com/opacusio](https://github.com/opacusio)

---

## Зачем OpacusR

Коммерческие системы удалённого доступа проводят сессии через облако вендора. OpacusR устроен так, что **signaling- и relay-серверы — ваши**: один небольшой бинарник (или два Docker-контейнера), WebRTC data channels для прямого трафика peer-to-peer и зашифрованный TCP-relay как автоматический запасной вариант, когда прямое соединение невозможно.

### Возможности (MVP, реализовано)

| Функция | Состояние |
| --- | --- |
| Трансляция экрана (JPEG, настройка качества / масштаба / fps) | ✅ |
| Управление мышью и клавиатурой | ✅ |
| Передача файлов (чанками, с докачкой, проверка SHA-256) | ✅ |
| Чат | ✅ |
| Интерактивный терминал (PTY: ConPTY / openpty) | ✅ |
| Unattended-доступ (пароль, challenge-response) | ✅ |
| WebRTC-транспорт (webrtc-rs, DTLS-каналы) | ✅ |
| TCP-relay fallback + прямой TCP в LAN | ✅ |
| Сквозное шифрование на любом транспорте (ChaCha20-Poly1305) | ✅ |
| Самохостинговый сервер signaling + relay | ✅ |
| Docker Compose (signaling + coturn) | ✅ |

## Быстрый старт

### 1. Запустите сервер (самохостинг)

```bash
# из исходников
cargo run --release -p opacusr-server
# signaling : ws://0.0.0.0:8080/signal
# health    : http://0.0.0.0:8080/health
# tcp relay : 0.0.0.0:8081

# или через Docker (сервер + coturn STUN/TURN)
cd docker
sed -i "s/CHANGE_ME_opacusr_turn_secret/$(openssl rand -hex 32)/" turnserver.conf
docker compose up -d
```

### 2. Сторона хоста (управляемая машина)

```bash
cargo run --release -p opacusr-agent -- run --server ws://your-server:8080/signal

# unattended-доступ
opacusr-agent passwd            # задать пароль
opacusr-agent id                # показать ID этого хоста
opacusr-agent config            # путь к конфигу и сводка
```

Настройки лежат в `~/.config/opacusr/agent.toml` (Linux), `%APPDATA%\opacusr\agent.toml` (Windows), `~/Library/Application Support/opacusr/agent.toml` (macOS). Ключевые поля: `server_url`, `name`, `password`, `fps`, `jpeg_quality`, `scale_percent`, `auto_accept_files`, `download_dir`, `lan_port`, `ice_urls`.

### 3. Сторона зрителя (кто управляет)

```bash
cargo run --release -p opacusr-app
```

В GUI: введите URL signaling-сервера, подключитесь, выберите хост из списка (или введите его ID), введите пароль доступа — откроется удалённый рабочий стол. В окне сессии есть видеопоток, чат, панели передачи файлов и терминала.

### Сборка

```bash
cargo build --release          # всё
cargo test  --workspace        # 35 тестов, включая полный E2E-цикл сессии
```

Rust 1.75+ (edition 2021). Для сборки на Linux нужны клиентские библиотеки X11 (`libx11-dev`, `libxi-dev` на Debian/Ubuntu). Windows использует GDI; macOS — core-graphics там, где доступно (см. [docs/platforms.md](docs/platforms.md)).

## Архитектура за минуту

```
┌──────────┐  signaling (WS)  ┌────────────┐  signaling (WS)  ┌──────────┐
│  Viewer  │◄────────────────►│  opacusr-  │◄────────────────►│   Host   │
│  (GUI)   │                  │   server   │                  │  (agent) │
│          │   WebRTC data channels (предпочтительно, DTLS)  │          │
│          │◄═══════════════════════════════════════════════►│          │
│          │   TCP напрямую (LAN) или TCP relay :8081 (фолбэк)│         │
└──────────┘◄═══════════════════════════════════════════════►└──────────┘
```

- **Signaling** только знакомит участников: хосты делают `Register`, зрители — `RequestSession`, SDP/ICE обе стороны получают через сервер.
- **WebRTC** несёт два канала данных: `Control` (auth, ввод, чат, файлы, терминал) и `Media` (кадры видео), чтобы всплеск кадров не задерживал ввод.
- **TCP relay** — запасной вариант: тот же формат потока, мультиплексирование 16-байтными тикетами; сервер копирует байты в обе стороны, не читая их (они уже зашифрованы сквозным шифрованием).
- **Всё над транспортом зашифровано сквозным шифрованием**: Argon2id выводит PSK из пароля доступа, зритель доказывает знание пароля через HMAC-SHA256-ответ на challenge, HKDF выводит ключ сессии ChaCha20-Poly1305 со счётчиками nonce и защитой от повторов.

Подробности: [docs/architecture.md](docs/architecture.md) · формат потока: [docs/protocol.md](docs/protocol.md) · модель угроз: [docs/security.md](docs/security.md).

## Состав workspace

| Крейт | Назначение |
| --- | --- |
| `opacusr-proto` | Сообщения, framing, signaling-пакеты (bincode) |
| `opacusr-crypto` | Вывод PSK, challenge-response, сессионная криптография |
| `opacusr-transport` | Транспорты WebRTC / TCP / relay, чанкинг, защищённая сессия, движок передачи файлов, signaling-клиент |
| `opacusr-capture` | Захват экрана: X11, Win32 GDI, тестовый источник |
| `opacusr-codec` | JPEG-кодек за трейтом `VideoEncoder/Decoder` |
| `opacusr-input` | Ввод (enigo) с тестовыми дублями |
| `opacusr-server` | axum WebSocket signaling + TCP relay hub |
| `opacusr-agent` | Хост-демон: сессии, PTY, конфиг, CLI |
| `opacusr-client` | Машина состояний сессии зрителя (не зависит от транспорта) |
| `opacusr-app` | Десктопный GUI eframe/egui (зритель + панель хоста) |

## Заметки о безопасности (кратко)

- Пароль никогда не передаётся; хост проверяет HMAC-доказательство знания пароля для одноразового nonce.
- Ключи сессии уникальны для каждой сессии (сессионные соли); счётчики nonce защищают от повторов.
- Relay-сервер не видит открытый текст — он пересылает запечатанные кадры.
- Конфиги с учётными данными записываются с правами `0600`.

Полная модель угроз и планы (OS-keychain, TURN-учётные данные, rate limiting) — в [docs/security.md](docs/security.md).

## Документация

| Документ | Содержание |
| --- | --- |
| [docs/architecture.md](docs/architecture.md) | Крейты, потоки данных, транспорты, модель потоков |
| [docs/protocol.md](docs/protocol.md) | Framing, signaling-рукопожатие, сессионное рукопожатие, справочник сообщений |
| [docs/security.md](docs/security.md) | Криптографическая схема, модель угроз, известные ограничения |
| [docs/platforms.md](docs/platforms.md) | Статус захвата/ввода по платформам, заметки о сборке |
| [docs/roadmap.md](docs/roadmap.md) | Планы: H.264/VP9, аудио, keychain, мобильный UI… |

## Участие в разработке

PR приветствуются: [github.com/opacusio/opacusr](https://github.com/opacusio/opacusr). Перед отправкой выполните `cargo fmt` и `cargo clippy --workspace`; CI проверяет их и тесты на Linux, Windows и macOS.

## Лицензия

AGPL-3.0 — см. [LICENSE](LICENSE). Отправляя изменения, вы соглашаетесь лицензировать их на тех же условиях.
