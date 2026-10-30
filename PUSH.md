# Публикация OpacusR на GitHub (github.com/opacusio)

У вас в руках готовый git-репозиторий с одним коммитом (`OpacusR v0.1.0`).
Залить его на ваш GitHub нужно из-под вашей учётной записи — программа-ассистент
не имеет доступа к вашему аккаунту. Это делается один раз, тремя командами.

## 1. Создайте пустой репозиторий на GitHub

Откройте <https://github.com/new> (вы должны быть залогинены как **opacusio**) и заполните:

- **Repository name:** `opacusr`
- **Description:** `Open-source remote desktop in Rust — self-hostable TeamViewer alternative`
- **Видимость:** Public (AGPL-3.0 требует открытых исходников)
- ⚠️ **Не** добавляйте README, .gitignore и LICENSE галочками — репозиторий должен
  быть полностью пустым, иначе первый push придётся мержить.

## 2. Привяжите remote и запушьте

Если у вас настроен SSH-ключ на GitHub:

```bash
cd opacusr
git remote add origin git@github.com:opacusio/opacusr.git
git push -u origin main
```

Или по HTTPS (попросит логин `opacusio` и Personal Access Token вместо пароля —
создаётся на <https://github.com/settings/tokens>, права `repo`):

```bash
cd opacusr
git remote add origin https://github.com/opacusio/opacusr.git
git push -u origin main
```

## 3. Проверьте

- Код появится на <https://github.com/opacusio/opacusr>.
- На вкладке **Actions** через минуту запустится CI (`ci.yml`): fmt + clippy + тесты
  на Linux, Windows и macOS.
- GitHub сам определит лицензию AGPL-3.0 и язык Rust.

## Если репозиторий уже создан с README/LICENSE

```bash
cd opacusr
git remote add origin git@github.com:opacusio/opacusr.git
git pull origin main --allow-unrelated-histories --no-edit  # если там что-то есть
git push -u origin main
```

## Дальнейшие изменения

Работайте как обычно:

```bash
git add -A
git commit -m "что изменилось"
git push
```

---

### Что вошло в коммит (67 файлов)

| Блок | Содержимое |
| --- | --- |
| `crates/` | 10 крейтов workspace: proto, crypto, transport, capture, codec, input, server, agent, client, app (GUI) |
| `docker/` | `docker-compose.yml` (signaling + coturn), `server.Dockerfile`, `turnserver.conf` |
| `docs/` | architecture, protocol, security, platforms, roadmap |
| `.github/workflows/ci.yml` | CI: fmt, clippy (`-D warnings`), тесты на трёх ОС |
| `README.md` / `README.ru.md` | Двуязычный README |
| `LICENSE` | AGPL-3.0 |
| `Cargo.toml` / `Cargo.lock` | Манифест workspace + зафиксированные зависимости |
