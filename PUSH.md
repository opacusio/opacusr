# Публикация OpacusR на GitHub (github.com/opacusio)

У вас в руках готовый git-репозиторий. Залить его на ваш GitHub нужно
из-под вашей учётной записи — программа-ассистент не имеет доступа к вашему
аккаунту.

> **Обновление v0.2.0 / v0.3.0:** если репозиторий уже создан и v0.1.0
> запушен, вам нужен только раздел «Пушим новую версию» в конце файла.

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

## Пушим новую версию (репозиторий уже создан, история запушена)

Этот архив содержит всю историю до текущего коммита включительно:
`bd0deeb` (v0.1.0) → `13e4cd0` (v0.2.0: H.264, keychain, deb/MSI) →
`77f9ced` (фикс CI) → … → **v0.3.0** (clipboard sync, view-only + ACL,
адаптивное качество, DXGI + PipeWire, мульти-монитор, аудио). Пушьте так:

1. Распакуйте архив в **новую** папку, например `D:\opacusr-0.3.0`.
2. В PowerShell 5.1 выполняйте команды **по одной** (без `&&`):

```powershell
cd D:\opacusr-0.3.0
git remote set-url origin https://github.com/opacusio/opacusr.git
git push origin main
```

3. Git Credential Manager спросит логин (`opacusio`) — тот же, под которым
   вы уже пушили v0.1.0. Откроется окно браузера или запросят PAT
   (<https://github.com/settings/tokens>, права `repo`).

4. На вкладке **Actions** появится новый запуск CI — все три ОС должны
   быть зелёными. Затем опубликуйте релиз тегом:

   ```powershell
   git tag v0.3.0
   git push origin v0.3.0
   ```

   Release workflow соберёт deb/msi/tar.gz и опубликует GitHub Release.
   Вкладка «Релизы» заполнится автоматически — но только когда ВСЕ задачи
   релиза пройдут (при любой ошибке шаг публикации пропускается, и релиз
   не создаётся — это не «удаление», а незапущенная публикация).
   MSI-шаг использует WiX v3-совместимый `wix/main.wxs` (версия продукта
   берётся из тега автоматически), macOS получает системный Opus из brew.

5. **Если тег уже ставился на старый коммит** (v0.3.0 ставился на
   `d7deabe` — без фиксов; v0.2.0 — на релиз со сломанным MSI),
   переставьте его на новый код. Порядок важен: сначала удалить тег на
   GitHub, затем выставить заново — так Release гарантированно
   перезапустится:

   ```powershell
   git push origin :refs/tags/v0.3.0
   git tag -f v0.3.0
   git push origin v0.3.0
   ```

   Сломанный тег `v0.2.0` можно просто удалить — его релиз никогда не
   публиковался:

   ```powershell
   git push origin :refs/tags/v0.2.0
   ```

   Важно: удаление тега не удаляет ни коммиты, ни историю — весь код
   остаётся в `main`, история версий сохраняется целиком.

Старую папку с предыдущей версией после этого можно удалить — вся история
git в новом архиве.

---

### Что вошло в v0.1.0 (67 файлов)

| Блок | Содержимое |
| --- | --- |
| `crates/` | 10 крейтов workspace: proto, crypto, transport, capture, codec, input, server, agent, client, app (GUI) |
| `docker/` | `docker-compose.yml` (signaling + coturn), `server.Dockerfile`, `turnserver.conf` |
| `docs/` | architecture, protocol, security, platforms, roadmap |
| `.github/workflows/ci.yml` | CI: fmt, clippy (`-D warnings`), тесты на трёх ОС |
| `README.md` / `README.ru.md` | Двуязычный README |
| `LICENSE` | AGPL-3.0 |
| `Cargo.toml` / `Cargo.lock` | Манифест workspace + зафиксированные зависимости |
