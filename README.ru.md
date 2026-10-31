# OpacusR

[![Release](https://img.shields.io/github/v/release/opacusio/opacusr)](https://github.com/opacusio/opacusr/releases/latest)
[![CI](https://github.com/opacusio/opacusr/actions/workflows/ci.yml/badge.svg)](https://github.com/opacusio/opacusr/actions/workflows/ci.yml)
[![Release build](https://github.com/opacusio/opacusr/actions/workflows/release.yml/badge.svg)](https://github.com/opacusio/opacusr/actions/workflows/release.yml)
[![License](https://img.shields.io/github/license/opacusio/opacusr)](LICENSE)

**Открытый удалённый рабочий стол** — трансляция экрана, управление, передача файлов, чат и терминал поверх самохостинговой инфраструктуры. Написано на Rust: аналог TeamViewer / AnyDesk, где серверы — *ваши*.

> 🇬🇧 English documentation: [README.md](README.md)

- **Лицензия:** AGPL-3.0
- **Статус:** v0.3.1 — рабочее ядро, активная разработка
- **Платформы:** Linux (X11 / Wayland через PipeWire), Windows (DXGI/GDI), macOS (частично), Android/iOS (каркас)
- **Репозиторий:** [github.com/opacusio](https://github.com/opacusio)

---

## Зачем OpacusR

Коммерческие системы удалённого доступа проводят сессии через облако вендора. OpacusR устроен так, что **signaling- и relay-серверы — ваши**: один небольшой бинарник (или два Docker-контейнера), WebRTC data channels для прямого трафика peer-to-peer и зашифрованный TCP-relay как автоматический запасной вариант, когда прямое соединение невозможно.

### Возможности (MVP, реализовано)

| Функция | Состояние |
| --- | --- |
| Трансляция экрана — H.264 (Cisco OpenH264, настройки под экран) с фолбэком на JPEG; настройка качества / масштаба / fps | ✅ |
| **Адаптивное качество** — тир-контроллер по RTT/FPS: быстрое снижение, осторожное повышение, ручные настройки в приоритете | ✅ |
| **Мульти-монитор** — список мониторов и переключение на лету (X11 RandR, GDI, DXGI) | ✅ |
| **Синхронизация буфера обмена** — в обе стороны с защитой от эха | ✅ |
| **View-only режим + ACL** — полный доступ / только просмотр / отказ — на сессию и на зрителя | ✅ |
| **Аудио** — Windows: системный звук (WASAPI loopback); Linux/macOS: микрофон; Opus 48 кГц моно ~32 кбит/с | ✅ |
| Управление мышью и клавиатурой | ✅ |
| Передача файлов (чанками, с докачкой, проверка SHA-256) | ✅ |
| Чат | ✅ |
| Интерактивный терминал (PTY: ConPTY / openpty) | ✅ |
| Unattended-доступ (пароль, challenge-response) | ✅ |
| Пароль в OS-keychain (Windows Credential Manager / macOS Keychain / Linux Secret Service), фолбэк на файл 0600 | ✅ |
| WebRTC-транспорт (webrtc-rs, DTLS-каналы) | ✅ |
| TCP-relay fallback + прямой TCP в LAN | ✅ |
| Сквозное шифрование на любом транспорте (ChaCha20-Poly1305) | ✅ |
| Самохостинговый сервер signaling + relay | ✅ |
| Docker Compose (signaling + coturn) | ✅ |
| Пакеты: deb (agent + viewer, systemd unit, .desktop) · MSI (WiX) · release-CI | ✅ |
| Захват: DXGI Desktop Duplication (Windows), X11 (Linux), Wayland через PipeWire-портал (эксперим.) | ✅ |

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

Настройки лежат в `~/.config/opacusr/agent.toml` (Linux), `%APPDATA%\opacusr\agent.toml` (Windows), `~/Library/Application Support/opacusr/agent.toml` (macOS). Ключевые поля: `server_url`, `name`, `password` / `password_in_keychain`, `fps`, `jpeg_quality`, `scale_percent`, `auto_accept_files`, `download_dir`, `lan_port`, `ice_urls`, `codec` (`auto` | `h264` | `jpeg`), `clipboard_sync`, `adaptive_quality`, `audio`, `audio_device`, `capture` (`auto` | `gdi` | `dxgi`, Windows), `default_permission` (`full` | `view-only` | `deny`), `acl_rules` (переопределения по зрителям), блокировка подбора пароля: `auth_max_attempts` / `auth_window_secs` / `auth_lockout_secs` (по умолчанию 5 / 300 / 60; `0` — отключить), `turn_api` (получение временных TURN-кредов с `GET /turn` сервера; включено по умолчанию).

### 3. Сторона зрителя (кто управляет)

```bash
cargo run --release -p opacusr-app
```

В GUI: введите URL signaling-сервера, подключитесь, выберите хост из списка (или введите его ID), введите пароль доступа — откроется удалённый рабочий стол. В окне сессии есть видеопоток, чат, панели передачи файлов и терминала.

### Установка (пакеты)

**Скачивание:** [последний релиз](https://github.com/opacusio/opacusr/releases/latest) — `.deb` для Debian/Ubuntu, `.msi` / `.zip` для Windows, `.tar.gz` для macOS и портативного Linux. У каждого файла есть SHA-256 контрольная сумма в `SHA256SUMS` (проверка: `sha256sum -c SHA256SUMS`).

| Платформа | Пакет | Примечания |
| --- | --- | --- |
| Debian/Ubuntu | `opacusr-app_*.deb`, `opacusr-agent_*.deb` | у зрителя — `.desktop` и иконка; у агента — systemd **user**-юнит (`systemctl --user enable --now opacusr-agent`) |
| Windows | `opacusr-*-x64.msi` | ставит viewer + agent + server в `Program Files\OpacusR\bin`, ярлык в меню Пуск, иконка вшита в exe |
| macOS / portable | `.tar.gz` из релизов | распаковать и запустить |

Артефакты собираются автоматически на каждый тег `v*` ([.github/workflows/release.yml](.github/workflows/release.yml): Linux deb + tar.gz, Windows MSI + zip, macOS tar.gz). Локально: `cargo deb -p opacusr-app -p opacusr-agent` (нужен `cargo install cargo-deb`); MSI — см. шапку `wix/main.wxs`.

#### Проверка загрузок (все ОС)

К каждому релизу прилагается `SHA256SUMS` — контрольная сумма для каждого файла. Проверьте загрузку, прежде что-либо запускать:

- Linux: `sha256sum -c SHA256SUMS` (в папке со скачанными файлами).
- macOS: `shasum -a 256 -c SHA256SUMS` (там же).
- Windows (PowerShell): `Get-FileHash -Algorithm SHA256 .\opacusr-<ver>-x64.msi` и сравните хеш со строкой в `SHA256SUMS`; либо `certutil -hashfile opacusr-<ver>-x64.msi SHA256` в cmd.

#### macOS: первый запуск (Gatekeeper)

Бинарники из релиза подписаны **ad-hoc**: перед упаковкой release-workflow
выполняет `codesign --force --sign -` для каждого Mach-O. Без Apple Developer ID
и без нотаризации (это AGPL-проект без платного сертификата подписи). Валидная
подпись — даже ad-hoc — на Apple Silicon обязательна: macOS на arm64 убивает
процесс, чей бинарник не подписан или изменён после подписи (например, strip).
Остаётся стандартная однократная проверка Gatekeeper при первом запуске
скачанного файла: «не удаётся открыть opacusr-agent, так как не удаётся
проверить разработчика». Любой из способов снимает блокировку:

```bash
xattr -cr opacusr-*-macos-aarch64/   # снять карантинную метку с распакованных файлов
```

- Правый клик по бинарнику → **Открыть** → ещё раз **Открыть** в диалоге
  (обход предлагается только при первом запуске).
- Или: Системные настройки → **Конфиденциальность и безопасность** →
  предупреждение о заблокированном файле → **Открыть в любом случае**.
- Или: выполнить команду `xattr -cr` выше — после этого бинарник запускается
  как обычный локально собранный.

Подпись едет внутри `.tar.gz`; предупреждение вызывает только карантинная
метка, которую ставит браузер при скачивании. `sha256sum -c SHA256SUMS`
работает как обычно.

#### Windows: первый запуск (SmartScreen)

MSI и zip не подписаны Authenticode (платный сертификат подписи отсутствует — это AGPL-проект), поэтому первый запуск установщика может вызвать SmartScreen: «Система Windows защитила ваш компьютер». Это ожидаемое поведение для любой неподписанной загрузки, а не признак проблемы. Чтобы продолжить: **Подробнее** → **Выполнить в любом случае**. Якоря доверия, по убыванию силы: сверить контрольную сумму `SHA256SUMS` (см. выше) или собрать из исходников — `cargo build --release`. После первого подтверждённого запуска Windows запоминает выбор для этого файла.

### Сборка

```bash
cargo build --release          # всё
cargo test  --workspace        # 40 тестов, включая полный E2E-цикл (уже с H.264)
```

Rust 1.75+ (edition 2021). Для сборки на Linux нужны клиентские библиотеки X11 (`libx11-dev`, `libxi-dev` на Debian/Ubuntu) и `libasound2-dev` (аудио); NASM опционален (ускоряет встроенный OpenH264). Захват на Windows — DXGI Desktop Duplication с фолбэком на GDI (`--capture gdi` — принудительно); macOS — core-graphics там, где доступно (см. [docs/platforms.md](docs/platforms.md)).

Опциональные сборки:

```bash
cargo build --release --features opacusr-capture/pipewire   # Wayland через портал (нужны libpipewire-0.3-dev, libspa-0.2-dev)
cargo build --release --no-default-features                 # без аудио (например, кросс-сборка windows-gnu)
```

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
| `opacusr-capture` | Захват экрана: X11 (RandR, мультимонитор), DXGI Desktop Duplication + GDI (Windows), PipeWire-портал (Wayland, эксперим.), тестовый источник |
| `opacusr-codec` | JPEG + H.264 (OpenH264) за трейтом `VideoEncoder/Decoder` |
| `opacusr-input` | Ввод (enigo), воркер буфера обмена, тестовые дубли |
| `opacusr-server` | axum WebSocket signaling + TCP relay hub |
| `opacusr-agent` | Хост-демон: сессии, PTY, захват аудио, адаптивное качество, ACL, CLI |
| `opacusr-client` | Машина состояний сессии зрителя (кадры, ввод, аудио, буфер обмена) |
| `opacusr-app` | Десктопный GUI eframe/egui (зритель + панель хоста) |

## Заметки о безопасности (кратко)

- Пароль никогда не передаётся; хост проверяет HMAC-доказательство знания пароля для одноразового nonce.
- Ключи сессии уникальны для каждой сессии (сессионные соли); счётчики nonce защищают от повторов.
- Relay-сервер не видит открытый текст — он пересылает запечатанные кадры.
- Пароль unattended-доступа хранится в OS-keychain (Windows Credential Manager / macOS Keychain / Linux Secret Service); если keychain недоступен — в конфиге с правами `0600`.
- Конфиги с учётными данными записываются с правами `0600`.

Полная модель угроз и планы (TURN-учётные данные, rate limiting) — в [docs/security.md](docs/security.md).

## Документация

| Документ | Содержание |
| --- | --- |
| [docs/architecture.md](docs/architecture.md) | Крейты, потоки данных, транспорты, модель потоков |
| [docs/protocol.md](docs/protocol.md) | Framing, signaling-рукопожатие, сессионное рукопожатие, справочник сообщений |
| [docs/security.md](docs/security.md) | Криптографическая схема, модель угроз, известные ограничения |
| [docs/platforms.md](docs/platforms.md) | Статус захвата/ввода по платформам, заметки о сборке |
| [docs/roadmap.md](docs/roadmap.md) | Планы: VP9/AV1, маршрутизация аудио, мобильный UI… |

## Участие в разработке

PR приветствуются: [github.com/opacusio/opacusr](https://github.com/opacusio/opacusr). Перед отправкой выполните `cargo fmt` и `cargo clippy --workspace`; CI проверяет их и тесты на Linux, Windows и macOS.

## Лицензия

AGPL-3.0 — см. [LICENSE](LICENSE). Отправляя изменения, вы соглашаетесь лицензировать их на тех же условиях.
