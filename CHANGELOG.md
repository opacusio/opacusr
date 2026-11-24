# Changelog

All notable changes to OpacusR are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
SemVer-ish (breaking protocol changes bump the minor until 1.0).

## [0.11.19] — 2026-11-24

### Бренд: логотип-призрак и кнопки из фирменного набора

- Иконка приложения везде заменена на неонового «призрака» из
  фирменного набора: лаунчер Android (`res/mipmap-*/ic_launcher.png`,
  раньше был дефолтный cargo-apk), иконка окна и трей на десктопе,
  MSI/Linux-ассеты (`assets/opacusr.png/.ico/-32/-128`).
- Логотип в шапке приложения («● OpacusR» → призрак + OpacusR) и в
  «О программе».
- Кнопки из набора ui-icons: «Подключиться» — иконка send; «Пароль
  доступа» в «Безопасности» — щит; строка unattended-доступа на главном
  экране — stealth (перечёркнутый глаз). Иконки swap/bridge/wallet
  зарезервированы: пока нет функциональности, к которой они честно
  привязываются.

### Мобильный UX: пароль и закрытие приложения

- **Задать пароль на телефоне теперь можно**: панель «Настройки» на
  узких экранах (<560 pt) разворачивается на ВСЮ ширину, навигация —
  чипами с переносом, поля ввода сжимаются под панель. Раньше доковая
  панель минимум 520 pt вытесняла левую навигацию за экран, и раздел
  «Безопасность» (пароль, 2FA) был физически недостижим на телефоне.
- **Кнопка «⏻ Закрыть»** в шапке (только Android): системный жест лишь
  сворачивает activity — теперь есть явный полный выход из приложения.
- Экран сессии до первого кадра и сообщения пароля русифицированы в
  0.11.18; здесь добавлены брендовые иконки к этим элементам.

## [0.11.18] — 2026-11-23

### Добавлено: разрешение для mDNS-обнаружения на Android

- `android.permission.CHANGE_WIFI_MULTICAST_STATE` — захват
  `WifiManager.MulticastLock` (исправленный в 0.11.17) требует именно его:
  без разрешения `acquire()` бросает SecurityException, наш best-effort
  хелпер понижается до строки в логе, и mDNS-обнаружение в «Контактах»
  не работает. Разрешение обычное (normal), выдаётся автоматически при
  установке.

### Полировка UI: пароль и ожидание первого кадра

- Сообщения при задании пароля доступа переведены на русский и
  объясняют, куда лег пароль: «сохранён в связке ключей ОС» либо
  «связка ключей недоступна — сохранён в конфиг с правами 0600» (на
  Android это штатный путь). Ошибка «слишком короткий пароль» теперь
  подсвечивается как ошибка.
- Подсказки в «Настройки → Безопасность» уточнены: пароль запрашивается
  у подключающихся вместо ручного подтверждения сессии.
- Экран сессии до первого кадра: вместо англоязычного
  «waiting for the first frame…» — «Ожидание первого кадра…» с живой
  строкой статуса подключения; если кадра нет дольше 10 секунд,
  показываются две известные причины (телефон-хост: трансляция ещё в
  разработке; компьютер-хост: проверьте разблокированный экран и работу
  OpacusR). Статус «Печать: кадр ещё не получен» тоже переведён.

## [0.11.17] — 2026-11-23

### Исправлено: белое окно при запуске на Android (критический краш)

- **Симптом**: после установки 0.11.15/0.11.16 приложение показывало белое
  (пустое) окно и никогда не переходило к интерфейсу; переустановка не
  помогала. Версии до 0.11.15 на тех же устройствах запускались нормально.
- **Причина**: в 0.11.15 в `android_main` добавили best-effort захват
  `WifiManager.MulticastLock` (нужен для mDNS-обнаружения в «Контактах»)
  через сырые vtable-вызовы jni-sys. В **jni-sys 0.4** типы `JavaVM`/
  `JNIEnv` — это сами указатели на vtable, а хэндл, который возвращает
  `AndroidApp::vm_as_ptr()` (NDK `JavaVM*` = `struct _JavaVM*`), — это
  указатель на структуру, **первым полем** которой является vtable; функции
  `JNIInvokeInterface` получают этот хэндл обратно первым аргументом.
  Вызов `AttachCurrentThread` делал ровно наоборот: разыменовывал хэндл
  как будто это vtable (читал указатель функции из памяти за пределами
  8-байтовой `_JavaVM` — то есть вызывал мусор) и передавал первым
  аргументом адрес локальной копии вместо хэндла. Итог — SIGSEGV до
  первого кадра UI на каждом реальном устройстве. Тест с null-хэндлами
  такое не видит в принципе (ранний return), а типы — сырые указатели,
  поэтому компилятор молчал.
- **Исправление**: хэндл разыменовывается один раз для загрузки vtable,
  сам хэндл передаётся как первый аргумент; после attach добавлена
  самопроверка `GetVersion ≥ JNI_VERSION_1_6` — при любой будущей ошибке
  обвязки фича молча деактивируется (строка в лог) вместо прыжка по
  мусорному указателю.
- **Регресс-тест на десктопе**: фейковые vtable с реальной раскладкой
  памяти (NDK-хэндл, у которого первое поле — адрес vtable; заглушки
  JNI-функций) прогоняют хелпер от `AttachCurrentThread` до успешного
  `acquire()` и проверяют, что первым аргументом передан именно хэндл.
  Старый код на этом тесте умирал, новый — проходит.
- mDNS-фича по-прежнему best-effort: при любой проблеме с JNI приложение
  запускается, «Контакты» просто не получат multicast-обнаружение.

## [0.11.16] — 2026-11-23

### Прототип децентрализованного режима (фаза 1): rust-libp2p

- **Новый крейт `opacusr-libp2p`** — исследование «AnyDesk без центрального
  сервера» на проверенном стеке rust-libp2p: **Kademlia DHT** (публикация и
  поиск адресов устройства по PeerId — подключение по ID без сигналинг-сервера),
  **AutoNAT** (определение внешнего адреса и достижимости), **Relay v2**
  (seed-узлы несут трафик для недостижимых извне узлов — роль TURN), **DCUtR**
  (координированный пробой NAT). Транспорт — QUIC (+TCP/noise/yamux),
  управляющий канал — request-response поверх мультиплексированного
  соединения.
- **Интеграционные тесты на loopback**: «диалер находит цель по PeerId через
  DHT и пингует её» и «дозвон строго через релей, резервации и
  circuit-адреса». Известный пробел фазы: DCUtR-апгрейд не срабатывает на
  loopback-стенде — обе стороны продолжают работать через релей (что само по
  себе работоспособно); пробой нужно проверить полевым тестом с двумя
  настоящими NAT.
- **Честные ограничения**: «без серверов» всё ещё означает несколько
  bootstrap/relay-узлов (мало, взаимозаменяемы, self-host); записи DHT
  делают онлайность устройства видимой сварму (авторизация остаётся на
  уровне протокола OpacusR, приватный swarm включается своим списком
  bootstrap-узлов); симметричные NAT (5G) пробиваются не всегда — для них
  и существует Relay v2.
- Крейт пока ни в один бинарник не вшит — режим опционален и не влияет на
  релизные сборки.

### Фаза 2: узел встроен в приложение (эксперимент)

- **Новая вкладка «Децентрализованный» в настройках (десктоп)**: тумблер
  «запускать узел вместе с OpacusR»; собственный **PeerId** и
  **пригласительный адрес** (`/ip4/lan/udp/4001/quic-v1/p2p/<PeerId>`)
  с копированием; редактор bootstrap-узлов (пусто = узел сам себе seed:
  kad-сервер + релей — этого хватает двум машинам одной LAN);
  доступность (AutoNAT/релей, слушающие адреса); **«Проверка связи»** —
  поиск партнёра в DHT, дозвон и контрольный пинг с вердиктом
  «прямое соединение (пробой NAT)» или «через релей»; строка событий.
- **Идентичность узла стабильна**: ключ хранится в
  `config_root()/p2p_identity.key` (0600 на Unix) — PeerId не меняется
  между запусками; повреждённый файл пересоздаётся.
- **Новый CLI `p2p-node`** для полевых тестов: `--seed` (узел =
  Kademlia-сервер + Relay v2 для других), `--bootstrap <multiaddr>` +
  `--ping <PeerId>` (дозвон по ID через DHT с отчётом), печать
  bootstrap-адреса для вставки в другие узлы.
- **Публикация в DHT сразу после kad-bootstrap** — раньше первый
  publish ждал до `publish_interval` (120 с) и поиск по ID простаивал;
  теперь запись появляется через секунды.
- **Только десктоп**: зависимости libp2p подключены в
  `cfg(not(android/ios))`-секцию — мобильные сборки не тянут aws-lc-rs,
  вкладка на телефоне честно поясняет, что узел пока не встроен.
- Проверено локальным смоук-тестом на loopback: seed + два клиента —
  клиент публикуется через ~2 с после старта, второй находит его в DHT
  (прямой + circuit-адрес), дозванивается (резервация релея, слушатель
  `/p2p-circuit`), пинг проходит; пробой DCUtR на loopback не
  срабатывает (нет NAT) — полевой тест на двух реальных сетях остаётся
  следующим шагом.

### CI: конфликт криптопровайдеров rustls

- Тесты `--workspace` падали на всех десктопных ОС: при объединении фич
  workspace rustls собирался сразу с ring (стек webrtc) и aws-lc-rs
  (rust-libp2p) и паниковал на первом `builder()` («no process-level
  CryptoProvider available»). Исправление: новый
  `opacusr_transport::ensure_rustls_crypto_provider()` — однократная явная
  установка ring при первом использовании транспорта (webrtc, сигналинг
  `wss://`, проверка обновлений).

## [0.11.15] — 2026-11-23

### Сценарий «в одной Wi-Fi сети» работает сам: находка хостов и файрвол

- **Подключение по ID автоматически пробует найденные в LAN серверы**:
  лестница сигналинга включает все хосты, обнаруженные по mDNS, — в одной
  Wi-Fi сети ID работает, даже когда оба устройства держат свои локальные
  серверы по умолчанию (первый сервер, в чьём реестре есть запрошенный ID,
  завершает рукопожатие). Настраивать «общий адрес» вручную больше не
  требуется — хотя это по-прежнему возможно в Настройки → Подключение.
- **Android: телефон видит компьютеры в сети.** Получение multicast-пакетов
  на Wi-Fi требует `WifiManager.MulticastLock` — без него драйвер молча
  выбрасывал mDNS-трафик: «Контакты» оставались пустыми, автопоиск не
  работал. Lock захватывается через JNI при старте приложения
  (best-effort: сбой — просто строка в логе).
- **Android: пароль и настройки наконец сохраняются.** `dirs::config_dir()`
  на Android возвращает `None`, и всё состояние молча писалось в
  недоступную директорию `./opacusr/` — отсюда «невозможно задать пароль».
  Теперь путь задаёт `$OPACUSR_CONFIG_DIR` (внутренняя директория
  приложения): пароль, история подключений, контакты, настройки.
- **Windows-установщик открывает входящие соединения.** MSI добавляет
  правила брандмауэра для opacusr.exe и opacusr-agent.exe (TCP+UDP, все
  профили) — раньше брандмауэр молча ронял первый же входящий пакет с
  телефона, и телефон не мог достучаться до компьютера даже в одной сети.
- **Android: честная пометка** в карточке «Этот компьютер»: трансляция
  экрана самого телефона (хост-режим) пока в разработке — телефон работает
  как пульт и смотрит чужие экраны. Сессия «ноутбук → телефон» остаётся
  пустой до появления MediaProjection-захвата.

## [0.11.14] — 2026-11-23

### Android — стабильная подпись релизов: обновления ставятся поверх

- **Причина ошибки «Приложение не установлено, так как оно конфликтует с
  другим пакетом» найдена**: релизный APK подписывался ключом, который
  жила в CI-кэше. Кэш вытеснялся, каждый релиз получал НОВУЮ подпись, и
  Android отказывался ставить новый APK поверх старого — подпись не
  совпадает ни с одной из установленных версий.
- **release.keystore теперь хранится в репозитории** (это публичный ключ
  дистрибуции: пароль и так лежит в workflow по замыслу проекта —
  стабильность идентичности важнее её секретности). Все будущие сборки
  подписываются одним и тем же ключом и обновляются поверх друг друга
  без удаления.
- Отпечаток ключа (SHA256):
  `894F15878F15D6B850A44F299528A378A20BE4CB6653CF6B67008EAEC0EF1944`.
- **Единоразовый переход**: версию 0.11.14 ставят поверх установленных
  0.11.10–0.11.13 НЕ получится — их подписи одноразовые. Один раз
  удалите старое приложение, поставьте 0.11.14 — дальше обновления
  будут накатываться штатно. ID и настройки после переустановки
  пересоздаются автоматически при первом запуске агента.

## [0.11.13] — 2026-11-23

### GUI — адрес для подключения показан прямо в приложении

- **Карточка «Этот компьютер» отвечает на главный вопрос «что вводить на
  телефоне?»**: под ID — два адреса с копированием по клику: прямое
  подключение `ip:порт` (Wi-Fi/LAN) и адрес встроенного сигнального сервера
  `ws://IP:8080/signal`. IP берётся из таблицы маршрутизации ОС без отправки
  пакетов; если IPv4-маршрута нет, блок просто не показывается.
- **Настройки → Подключение: «Сигнальный сервер» сохраняется сразу при
  вводе**. Раньше поле жило только в памяти и молча откатывалось на
  локальный адрес после перезапуска — на телефоне это выглядело как вечное
  «host unavailable» при каждом подключении по ID.
- Подсказка у «Сигнальный сервер» переписана: прежний текст «по умолчанию
  публичный» был ложью — по умолчанию каждое устройство держит свой
  локальный сервер и видит только само себя.
- Подсказки ошибок стали конкретными: «host unavailable» теперь указывает,
  где взять адрес компьютера (карточка «Этот компьютер») и куда его вписать
  на втором устройстве; отказ прямого `ip:порт`-подключения — про
  «Прямые LAN-подключения», перезапуск агента и брандмауэр.

### Сценарий «телефон → компьютер» в одной Wi-Fi сети

- Прямое подключение: на компьютере скопировать `IP:54000` из карточки
  «Этот компьютер», включить «Прямые LAN-подключения» (Настройки → Хост),
  на телефоне вставить адрес в поле ID.
- Через сигналинг: на телефоне в Настройки → Подключение вписать
  `ws://IP_компьютера:8080/signal` и подключаться по ID — сервер компьютера
  увидит оба устройства.

## [0.11.12] — 2026-11-22

### GUI — «Подключиться» больше не молчит: ошибки видны и объяснены

- **Ошибки подключения показываются золотым текстом** прямо на карточке
  «Подключиться к компьютеру» (раньше — слабая серая строка, которую на
  телефоне легко не заметить и решить, что «ничего не происходит»).
  Касается и мгновенных проверок: пустой пароль, неверный формат ID,
  некорректный прокси.
- **Пустой пароль**: кроме сообщения поле «Пароль» сразу получает фокус —
  на телефоне клавиатура открывается на нужном поле.
- **Причина неудачи возвращается на главный экран**: после закрытия
  неудавшейся сессии карточка показывает «Подключение не удалось: …» с
  подсказкой под ситуацию — «peer is offline» (проверьте ID, агент и
  сигнальный сервер; в одной Wi-Fi сети — подключение по ip:порту),
  отказ авторизации (пароль/2FA), таймаут сервера.
- **Новая галочка «Прямые LAN-подключения»** (Настройки → Хост): включает
  приём прямых сессий `ip:порт` в локальной сети без signaling-сервера
  (порт по умолчанию 54000; настройка и раньше была в конфиге, но не в
  интерфейсе; вступает в силу после перезапуска агента).
- Чистое закрытие сессии хостом по-прежнему нейтрально («Сессия закрыта»),
  «Отключено.» не затирает объяснение ошибки.

## [0.11.11] — 2026-11-22

### Android — агент теперь может выходить в сеть (права APK)

- **Ключевая причина «Идёт регистрация на сервере…» на Android найдена**:
  APK собирался вообще без разрешений Android. Без
  `android.permission.INTERNET` система запрещает создание любых
  сетевых сокетов — даже к самому себе на 127.0.0.1:8080, поэтому
  встроенный сигнальный сервер не поднимался и регистрация никогда
  не завершалась.
- В манифест добавлены **`INTERNET`** (сокеты: подключение к сигнальному
  серверу, встроенный листенер, WebRTC-соединения) и
  **`ACCESS_NETWORK_STATE`** (диагностика connectivity).
- Добавлен **`usesCleartextTraffic`** — сервер по умолчанию и LAN-серверы
  работают по `ws://` (не-TLS); нативные Rust-сокеты этим флагом не
  проверяются, но он снимает будущие блокировки JVM-стека на API 28+.
- Приложение можно ставить **поверх** 0.11.10 (тот же package и ключ
  подписи): обновление подхватит новые права без удаления.

### Все сборки — диагностика регистрации на главном экране

- В блоке «Этот компьютер», пока ID ещё не выдан, теперь **прямо на
  главном экране показывается последняя ошибка подключения** (например
  «connection lost: Permission denied; retrying in 4s» на золотом фоне).
  Раньше она была только в Настройки → Диагностика, и причина вечной
  «регистрации на сервере…» оставалась невидимой. Работает во всех
  сборках: Windows, Linux, macOS, Android.

## [0.11.10] — 2026-11-22

### GUI — закрытие окна больше не убирает приложение из трея

- **Крестик окна теперь сворачивает OpacusR в трей** (модель
  TeamViewer/AnyDesk): окно скрывается, а приложение продолжает работать —
  агент принимает подключения, запросы подтверждения и передачу файлов;
  иконка в трее остаётся на месте. Раньше закрытие окна по умолчанию
  полностью завершало процесс, и иконка исчезала из трея.
- **Выход по-прежнему настоящий** — через меню иконки трея («Выход») либо
  галочку «при закрытии окна сворачивать в трей» в Настройки → Хост можно
  снять, тогда крестик завершает приложение как раньше.
- **Миграция старых настроек**: файлы agent.toml, записанные сборками
  ≤ 0.11.9 (с `close_to_tray = false`), один раз переключаются на новое
  поведение при первом запуске; остальные настройки (сервер, имя, пароль,
  служба) сохраняются без изменений. Явно снятая после обновления галочка
  запоминается.
- Тексты подсказки в Настройки → Хост обновлены под новое поведение.

## [0.11.9] — 2026-11-22

### GUI — главное окно открывается сразу с историей

- **Главное окно больше не нужно: приложение открывается сразу с окном
  истории** (AnyDesk-стиль). Список «История подключений» — основное
  содержимое окна: занимает всю правую часть и всю высоту, строки
  прокручиваются, подпись «последние 100 попыток подключений» закреплена
  внизу.
- **Кнопка «Очистить» в истории убрана** — очистка истории недоступна из
  интерфейса; список последних 100 попыток сохраняется.
- **Большая TeamViewer-карточка удалена**: слева компактная карточка
  «Подключиться к компьютеру» (ID партнёра, пароль, violet-кнопка
  «Подключиться», код 2FA по запросу) и блок «Этот компьютер» (свой ID
  с копированием по клику, статус агента/службы, запуск/остановка
  агента, показ пароля доступа без присмотра). Пароль доступа, 2FA и
  служба — в Настройках (Хост / Безопасность), как и раньше.
- **Кнопка «🕘 История» в верхней панели убрана** — история теперь
  всегда на главном экране; в панели остались «⚙ Настройки» и
  «👥 Контакты».
- Адаптивность сохранена: при ширине окна ниже 860 px (узкое окно,
  открытая панель, телефон в портретной ориентации) карточка и история
  складываются в один прокручиваемый столбец.
- Проверено визуально на Linux (Xvfb): широкое окно 1564×1100 и
  минимальное 760×560 — с заполненной и с пустой историей.

## [0.11.8] — 2026-11-22

### GUI — выравнивание и адаптивность

- **Домашняя карточка заполняет окно и центрируется**: ширина до 840 px,
  содержимое центрируется по вертикали — на широких окнах больше нет
  пустых полей слева/справа и «мёртвой» полосы под карточкой.
- **Адаптивные колонки**: при открытой боковой панели или узком окне
  блок «Разрешить управление этим компьютером» складывается в один
  столбец (ID над паролем) — раньше колонки сжимались, и содержимое
  обрезалось краем панели.
- **ID больше никогда не переносится на несколько строк**: размер
  моноширинных цифр «123 456 789» масштабируется под ширину столбца
  (кнопка ID растягивается на всю колонку).
- **Все поля заполняют свой столбец**: пароль доступа, «Задать»,
  новый пароль и код 2FA больше не оставляют висящих половинных полей;
  статус соединения выровнен по левому краю под полями.
- Проверено визуально на Linux (Xvfb): широкое окно, узкое окно,
  узкое окно с открытой панелью «Настройки» (крайний случай, при
  котором ID ранее рассыпался на четыре строки).

### Мобильные приложения (начало)

- Новый крейт **`opacusr-android`**: NativeActivity-обвязка вокруг того
  же egui-интерфейса (`android_main` через
  `EventLoopBuilderExtAndroid::with_android_app`), манифест cargo-apk
  (`io.opacusr.app`, arm64-v8a, min SDK 24, полноэкранная тема).
  Телефон работает зрителем: те же поля ID/пароля, подключение по ID
  партнёра или `ip:port`.
- Release-CI собирает **`opacusr-<версия>-android-arm64.apk`**
  (cargo-apk + NDK r27b) и публикует его в релизе; SHA256SUMS теперь
  считается и по APK.
- CI: постоянные кросс-проверки на каждый push — `aarch64-linux-android`
  (GUI-обвязка целиком) и `aarch64-apple-ios` (чистое ядро: proto + crypto).
- Мобильная гигиена зависимостей: файловые диалоги вынесены в
  cfg-гейтированные хелперы `pick_file`/`pick_files`/`pick_folder`
  (rfd больше не собирается на мобильных целях), аудио-поток (Opus)
  на мобильных пока отключён — NDK-сборка Opus в следующей итерации.
- `keychain.rs`: на Android/iOS — заглушка с прежней сигнатурой
  (ошибка «keychain unavailable», прозрачный фолбэк в конфиг-файл,
  как на headless-Linux без Secret Service); `lock.rs` (захват
  физического ввода через evdev/LL-hooks) — только для десктопа,
  у телефона нет физического ввода, который нужно блокировать; ioctl-
  cast в evdev-захвате сделан переносимым между юникс-архитектурами.
- iOS: постоянный CI-чек пока ограничен чистым ядром (proto + crypto) —
  полный `opacusr-client` для aarch64-apple-ios блокирует upstream:
  webrtc 0.14 → webrtc-util 0.12 → nix 0.26 (sockaddr_ctl не объявлен
  для iOS в libc). Когда стек webrtc поднимет nix — расширим чек.
- Захват экрана Android (режим хоста с телефона, MediaProjection) —
  следующая итерация; сейчас телефон = зритель.
- Детали сборки APK (проверены на релизном прогоне): target SDK 34
  (на раннерах GitHub нет platform 30), релизная подпись —
  самогенерируемый кейстор, закэшированный между прогонами
  (`CARGO_APK_RELEASE_KEYSTORE` передаётся абсолютным путём — cargo-apk
  делает `cd target/release/apk` перед apksigner).

### Прочее

- README (RU/EN): таблица пакетов дополнена Android-APK и командой
  локальной сборки APK.
- Чистка workspace-манифеста: убраны неиспользуемые записи
  (`rpassword` в `[workspace.dependencies]`, `authors`/`description`
  в `[workspace.package]`).

## [0.11.7] — 2026-11-22

### Changed

- **Меню больше не плавающие окна**: «Настройки», «Контакты» и «История»
  превратились в боковые панели-шторки, пристыкованные к правому краю
  окна на всю высоту (AnyDesk-стиль). Раньше каждое «меню» открывалось
  свободным окном в случайном месте: окна накладывались друг на друга
  и на главный экран, растягивались за границы приложения и терялись.
- Панели взаимоисключающие: открыта ровно одна за раз — кнопка в
  верхней панели переключает шторки, повторное нажатие закрывает.
- Esc и кнопка «✕» в заголовке закрывают открытую шторку (Esc во время
  переименования контакта сначала отменяет переименование).
- Кнопка меню подсвечивается фиолетовым, пока её шторка открыта.
- В «Истории подключений» подвал («последние 100 попыток…» + «Очистить»)
  прибит к низу панели, а не висит посреди пустого окна; в «Контактах»
  поиск прибит сверху, «+ Добавить контакт» — снизу.
- Кнопка закрытия «✕» в шторках рисуется линиями (как крестик у
  собственных окон egui) — вместо глифа, которого нет в шрифте и
  который рендерился квадратиком.

### Fixed

- clippy: `map_or(true, …)` → `is_none_or` в опросе ID службы.

## [0.11.6] — 2026-11-22

### Fixed

- **A fresh install could never show an ID** («регистрация на
  сервере…» forever): the default config registers on
  `ws://127.0.0.1:8080/signal`, and the embedded signaling server that
  should own that address was OFF by default — so the registration
  failed forever and `last_peer_id` stayed empty. The embedded
  signaling server is now ON by default (`signal_listen = true`): the
  agent serves its own registry + relay on 0.0.0.0:8080 (+1), registers
  on it, and the ID appears within seconds of the service starting,
  with zero setup. Hand-set `signal_listen = false` in agent.toml still
  wins.
- **The agent never persisted its ID**: `last_peer_id` was written only
  by the GUI's in-process agent, so while the Windows service hosted
  the machine the GUI could never learn the ID. Every agent run
  (service child, CLI) now persists the ID assigned by the primary
  signaling endpoint to the shared config (read-modify-write, fan-out
  endpoints other than the primary never touch it).
- **The GUI now re-reads the persisted ID** every ~3 s while it hosts
  no agent itself, so «регистрация на сервере…» turns into the real ID
  without a restart, and the card explains the state («ID назначен
  службой · машина доступна»).

## [0.11.5] — 2026-11-21

### Fixed

- **The Windows service process died before the dispatcher ever ran** —
  the actual root cause of the MSI «Service 'OpacusR Agent' failed to
  start» dialog (v0.11.4 fixed the missing `Running` report, but the
  process never got that far): the SCM launches a service process with
  the ImagePath as-is and does NOT append the service name to the
  command line (it goes to `ServiceMain` through the dispatcher), so
  the old `argv[1] == "OpacusRAgent"` routing never fired, the process
  fell into the CLI parser and clap exited with «requires a
  subcommand». The agent now tries `StartServiceCtrlDispatcher` before
  the CLI parser (`win_service::try_dispatch`): as a service process
  it connects and runs; anywhere else it fails fast with 1063 and the
  normal CLI continues.
- **MSI hardening**: the service's `ErrorControl` is now `ignore`, so a
  service start failure can never block or roll back the installation
  with the Retry/Cancel dialog; and `ServiceControl Remove='both'`
  stops and deletes any stale `OpacusRAgent` registration right before
  the fresh one is created, so a leftover manual install can no longer
  make the setup fail.
- The WiX download step in the release workflow now retries (a
  transient download failure aborted the first v0.11.5 run).

## [0.11.4] — 2026-11-21

### Fixed

- **The Windows service could never reach `Running`** — the real cause
  of «Service 'OpacusR Agent' failed to start» in the MSI (and of
  `sc start` timing out with error 1053): the supervisor reported
  `StartPending`, entered its poll loop and never reported
  `ServiceState::Running`, so the SCM killed the process ~30 s after
  every start attempt. The service now reports `Running` immediately
  after the handshake (nothing blocks before the poll loop), and the
  per-session agent spawn / stop logic is unchanged.
- **The service left no diagnostics behind**: a Windows service has no
  console, so every lifecycle message was lost. The supervisor now
  appends a timestamped support log to
  `C:\ProgramData\OpacusR\service.log` (SCM handshake, Running,
  per-session child spawns and their failures, stop requests) —
  writable by LocalSystem, readable by administrators, best-effort
  (a failed log write can never crash the service).

## [0.11.3] — 2026-11-17

### Fixed

- **The tray icon could linger after «Выход»** («весит в трее»): the
  quit watchdog `process::exit` bypasses every destructor, so a
  force-exited process left a dead tray icon on screen until the mouse
  touched it. The icon is now dropped explicitly (both on tray «Выход»
  and on the real-exit close path) before the process ends.
- **Logon autostart no longer launches the window hidden** — the MSI
  autostart entry dropped the `--tray` flag: at logon the OpacusR
  window OPENS (TeamViewer opens its window at logon too). A hidden
  autostarted instance read as «запустилось и висит в трее, не
  выходит»; the flag is still honored when something launches the exe
  with it explicitly.
- **Show-from-tray / second-launch reveal is more robust**: the
  window is un-minimized (`Minimized(false)`) before `Visible(true)`
  on every reveal path (tray «Показать», tray icon click, second
  `opacusr.exe` launch, incoming-consent popup) — a hidden+minimized
  window on Windows can ignore a bare `Visible(true)`.

### Changed

- **The GUI defers to a running service** (TeamViewer model): when the
  `OpacusRAgent` Windows service is running, the GUI no longer starts
  its own in-process agent on top of it — that registered a SECOND ID
  for the same machine on the signaling server. The home card shows
  «● служба работает» with a hint, the tray «Запустить агента» stays a
  no-op until the service is stopped, and Настройки → Хост keeps the
  service row for install/status control.
- **The build version is visible in the UI** (top-right of the window,
  e.g. `v0.11.3`) so it is always clear which binary actually runs —
  an MSI upgrade never silently leaves an old-looking install.

## [0.11.2] — 2026-11-05

### Fixed

- **The Windows agent service could never start** — the SCM launches a
  service process as `<exe> <ServiceName> [args]`, so the registered
  `ImagePath = exe service run` made clap see `OpacusRAgent` (the
  service name) as an unknown subcommand and the process died before
  the dispatcher ran (SCM error 1053). The service is now registered
  with a bare-exe ImagePath (both `win_service::install` and the WiX
  `ServiceInstall`), and `opacusr-agent` routes the SCM invocation
  (argv[1] = service name) straight to the dispatcher before the CLI
  parser. This is what «не запускаются службы» was.
- **The app could not be closed** («весит в трее»): closing the window
  now QUITS the app by default (the in-process agent stops with it; an
  installed OS service keeps the machine reachable). Hide-to-tray on
  close became an opt-in setting (Настройки → Хост → «Окно»), and the
  tray «Выход» got a watchdog `process::exit` so it can never leave a
  zombie in the tray. Without a tray icon the X always quits.
- Second `opacusr.exe` launches no longer spawn a duplicate process,
  tray icon and agent (Windows single-instance guard via a named
  mutex + auto-reset event): the second launch activates and focuses
  the running window and exits.

### Added

- **Windows service row in Настройки → Хост**: live SCM status
  («Служба Windows: работает / остановлена / не установлена») and a
  one-click «Установить службу» button with a UAC elevation for zip
  installs — no admin console needed.
- The MSI now launches OpacusR right after setup (checkbox on the
  final page, on by default) so a fresh install visibly starts instead
  of looking like «nothing runs».

## [0.11.1] — 2026-11-04

### Changed

- **One-card home screen** — the two side-by-side cards merged into a
  single TeamViewer-style card: «Разрешить управление…» on top (ID on
  the left, access password on the right, one status row with the
  start/stop control), «Управлять удалённым компьютером» beneath the
  separator.

### Fixed

- **White background on Windows** — egui's system-theme sync repainted
  the UI light on machines with a light OS theme, wiping the Rev-3
  palette. The app theme is now pinned to dark and re-asserted every
  frame.
- **White native chrome + startup flash** — Windows drew a light title
  bar and border around the dark client area, and the stock white
  background flashed between window creation and the first painted
  frame. The window is now created hidden, the first frame paints the
  dark UI, `apply_native_chrome()` switches the DWM caption/border/text
  to the palette (immersive dark mode on Windows 10, caption color on
  Windows 11; best-effort), and only then the window is revealed —
  already dark.
- **The ID is always visible** — the home card now shows the last
  signaling-assigned ID (persisted in `agent.toml` as `last_peer_id`)
  while the agent is offline, dimmed, instead of an empty «Агент не
  запущен» stub.
- **Services do not start automatically** — the MSI now registers AND
  starts the `OpacusRAgent` service during install (native WiX
  `ServiceInstall`/`ServiceControl`, binary + `service run` — the same
  line the agent's CLI registration uses) and stops+removes it on
  uninstall; `opacusr-agent service install` also starts the service
  immediately instead of waiting for a reboot. The MSI adds a per-machine HKLM Run autostart entry that
  launches the GUI hidden to the tray (`--tray`). The «Автозапуск»
  checkbox (Настройки → Хост) makes the GUI start the agent itself.

## [0.11.0] — 2026-11-04

_Version note: the wire protocol bumped to v9 (bincode 2.x codec) —
peers with different protocol versions are rejected at the handshake
and on the LAN mDNS record, so update the agent and the viewer
together. `opacusr-agent update` / the viewer's "Update now" button
deliver 0.11.0 binaries as one unit._

### Added

- **TeamViewer-style home screen** — the window is now the compact
  TeamViewer pair: «Разрешить управление этим компьютером» (big
  click-to-copy ID, access password with reveal/change/remove, agent
  start/stop, session status) and «Управлять удалённым компьютером»
  (partner ID, password, violet Connect CTA, on-demand 2FA field). No
  settings remain on the home screen; all options moved into a hidden
  settings window (gear in the toolbar) with a TeamViewer-Options left
  nav: Общие / Подключение / Безопасность / Хост / О программе — changes
  still apply instantly.
- **AnyDesk-style contacts and history windows** — «Контакты» is a
  separate device panel: search on top, Избранные / Все контакты /
  Найденные в этой сети sections, round colored avatars, hover actions
  (connect, rename, star, remove) and an add-contact form; «История
  подключений» shows timestamped attempts with outcome dots and
  durations. Connection attempts are recorded automatically.
- **System tray (Windows, macOS)** — TeamViewer-style background
  operation: a tray icon with a menu (Показать / Скрыть в трей /
  Запустить агента / Остановить агента / Выход), left click opens the
  window, and closing the window hides the app to the tray — the agent
  keeps serving. An incoming-session request pops the window back up.
  Linux keeps window-only behavior (tray-icon's Linux backends need GTK
  system packages that are not part of the CI image).

- **Rev-3 UI design in the desktop app** — the approved mockup theme is
  now the real `opacusr` look: dark navy background with rounded cards,
  violet accent buttons, teal status, Carlito typeface (embedded, SIL
  OFL 1.1) with a real bold face for section labels and CTAs. The
  Connect/Host tabs are merged into a single home screen: the connect
  card on top (remote ID + password, everything secondary — signaling
  server, proxy, transport, quality, network name — behind a
  collapsible «Доп. настройки»), the «Этот компьютер» host card (big
  ID, unattended password, 2FA, host policies and the activity log in
  collapsibles), an always-visible «Найдено в этой сети» card with the
  LAN-discovery explainer, and the contacts/history card. The network
  name now defaults to the computer's hostname instead of the login
  name. The incoming-session approval stays a centered popup and is now
  visible during viewer sessions too. UI strings are Russian (the
  approved mockup language); no wire/protocol changes.

- **Printing v1 (current frame → local printer)** — the viewer toolbar
  gains a 🖨 button: it encodes the last decoded remote frame to a
  lossless PNG (kept in the platform temp dir as
  `opacusr-print-<secs>.png`) and hands it to the local print system —
  CUPS `lp` on Linux/macOS, PowerShell `Start-Process -Verb Print` on
  Windows (association's print dialog). Pure viewer-side action: no
  protocol change, no host involvement; the frame copy is refreshed at
  most twice a second. The larger "redirect remote documents" epic
  stays on the roadmap.

- **Privacy mode v1 (display blanking)** — new opt-in `privacy_mode`
  key in `agent.toml` (plus `opacusr-agent run --privacy`): while a
  session is active the host's physical displays are put to sleep, so a
  bystander sees a dark monitor instead of the shared desktop while the
  remote viewer keeps working. Capture keeps running — the viewer still
  sees the desktop — and it composes with `lock_input` (which stops
  someone from waking the machine by touching it). Platforms: Windows
  `SC_MONITORPOWER` broadcast (wake on drop), Linux/X11 `xset dpms
  force off` (`force on` on drop), macOS `pmset displaysleepnow`
  (`caffeinate -u` wake). Fail-open like the input lock: when the
  platform cannot blank (Wayland, headless, no `xset`) the session
  continues unblanked with a notice; two concurrent sessions share one
  blank guard (wake happens only when the last session ends). The
  `opacusr-input::privacy` module is unit-tested on the command
  shaping; known v1 limitation documented: on some GPU/driver stacks
  DXGI duplication may stop producing frames while the display sleeps
  — the viewer then sees the last frame.
- **macOS launchd agent** — `opacusr-agent service install` now works
  on macOS: it writes a per-user LaunchAgent
  (`~/Library/LaunchAgents/io.opacusr.agent.plist`, `RunAtLoad` +
  `KeepAlive` + `ProcessType: Interactive`, logs in
  `~/Library/Logs/opacusr-agent.log`) and loads it via `launchctl`;
  `service status` reports the loaded state and `service uninstall`
  unloads and removes it. Unlike the Windows SCM service there is no
  session-0-style topology: the LaunchAgent already lives in the
  interactive GUI session, so capture and input run directly — the only
  platform-specific step is the one-time TCC grant (Screen Recording +
  Accessibility) that macOS asks for on first use (install prints the
  reminder). The plist generation is pure and unit-tested on every
  platform; `service` help texts now describe both mechanisms.

### Changed

- **Wire codec: bincode 1.x → bincode 2.x (proto v9)** — session and
  signaling payloads are now encoded with bincode 2.0
  (`bincode::serde` layer over the same serde types,
  `config::standard()`): integers are varint-encoded, so small messages
  shrink from a dozen of fixed-width bytes to 2–4 bytes (`Ping(7)` is
  2 bytes vs 12 in 1.x). The framing (`[channel][len][payload]`), the
  handshake shape and every `Message`/`SignalMessage` type are
  unchanged — only the byte encoding differs, so **proto v9 is not
  wire-compatible with ≤ v8**: the handshake (and the mDNS TXT `proto`
  record on LAN) refuses mismatched peers before any payload is
  decoded. Update both sides together (the self-updater delivers
  0.11.0+ binaries as one unit). The configuration lives in one place,
  `opacusr_proto::framing::bincode_config()`; the bincode 1.3.3 that
  remains in the lockfile is pulled only transitively by `webrtc →
  dtls` and no longer touches the wire.

### Fixed

- **Empty console window next to the GUI on Windows** — the GUI binary
  kept the console subsystem, so double-clicking `opacusr.exe` opened an
  extra empty black console alongside the app (it looked like a dead
  "server window"). Release builds are now `windows_subsystem =
  "windows"` (no console; debug builds keep it for logs).
- Chat lines in the viewer were prefixed with the mangled string
  `ost]` instead of `[host]`.
- **windows-gnu builds are self-contained** — the cross-built exes
  linked `libstdc++-6.dll` dynamically (pulled in by OpenH264's C++
  runtime) and failed to start on machines without MinGW ("the code
  execution cannot proceed because libstdc++-6.dll is missing"). The
  windows-gnu target now links `libgcc`/`libstdc++` statically
  (`.cargo/config.toml`), so only system Windows DLLs are imported.
  Additionally, the windows-gnu artifact now carries the full set
  incl. the GUI `opacusr.exe`: Opus is cross-built as a static
  library (prebuilt via `OPUS_LIB_DIR`/`OPUS_STATIC`, see
  `scripts/build_opus_mingw.sh` in the maintainer sandbox) instead of
  the broken in-crate autogen path, so the agent builds with audio and
  the GUI builds at all under mingw. Future CI tag builds (MSVC) link
  the CRT statically (`+crt-static`), removing the
  VC++ redist requirement there too.

## [0.10.0] — 2026-11-02

### Added

- **Self-update (opt-in, SHA-256 verified)** — `opacusr-agent update`
  (CLI, `--yes` for scripts) and an "Update now" button on the viewer's
  update banner: downloads the release asset for the running platform
  (`opacusr-<ver>-windows-x86_64.zip` / `opacusr-<ver>-linux-x86_64.tar.gz`
  / `opacusr-<ver>-macos-aarch64.tar.gz`, matching the release pipeline
  names), streams it while hashing, verifies the SHA-256 against the
  release's `SHA256SUMS`, extracts the binary and swaps the running
  executable aside (`<name>.opacusr-old` = rollback copy); the new
  version activates on the next start. Refuses `/usr`/`/opt` installs
  (use the package manager), verifies the running binary name before
  touching anything, and rolls back if the install step fails. The
  checksum comes over HTTPS from the same release channel — corruption
  protection, not a code signature. `OPACUSR_NO_UPDATE_CHECK=1` still
  disables the background check; an explicit update command works.
- **Outbound proxy support (SOCKS5 / HTTP CONNECT)** — new `proxy` key
  in `agent.toml` (agent + viewer) and a Proxy field in the viewer's
  connect form, plus `opacusr-agent run --proxy`. Formats:
  `socks5://[user:pass@]host:port` (default port 1080, hostnames are
  resolved by the proxy) and `http://[user:pass@]host:port` (explicit
  port). The proxy carries the internet-facing paths: the signaling
  WebSocket (TLS included — the tunnel is established first) and the
  TCP relay, which is exactly the fallback path strict corporate
  firewalls leave open. LAN/direct connections bypass the proxy, and
  WebRTC ICE/UDP traffic cannot traverse a TCP proxy — the transport
  ladder lands on the relay instead. Loopback endpoints (the embedded
  signaling server) are never proxied. Hand-rolled minimal SOCKS5
  (RFC 1928 + RFC 1929 auth, CONNECT only) and HTTP CONNECT in
  `opacusr-transport/src/proxy.rs`, unit-tested against in-process fake
  proxies (method negotiation, auth, error surfacing, round-trips).
- **Windows service mode for the agent** — `opacusr-agent service
  install|uninstall|status|run` (SCM name `OpacusRAgent`, auto-start).
  The service is a session-0 supervisor that spawns a regular agent
  child inside the active console session (user token + environment +
  `winsta0\default` desktop, kill-on-close job object), restarts it
  after a crash and stops it at logoff or service stop — so capture and
  input always run in the interactive session, never in session 0. The
  child uses the user's own config and OS-keychain unattended password;
  `service install` warns when no unattended password is set (the child
  has no console for the attended consent prompt). CI now cross-checks
  the agent for `x86_64-pc-windows-gnu` (mingw-w64) so the
  `cfg(windows)` service code is compiled on every push.
- **`allow_lan` is a config key now** — the direct-LAN listener and the
  mDNS `lan` TXT record are driven by `allow_lan` in `agent.toml`
  (default off) instead of the `--allow-lan` CLI flag only; the flag
  still overrides for one run. When it is on, the agent advertises the
  direct-LAN port in its mDNS advertisement, so the viewer's
  "Discovered on this network" list offers the `ip:port` target without
  any signaling server; sessions still require the password.
  `opacusr-agent config` reports `lan: on/off (port N)`.

## [0.9.0] — 2026-11-02

### Added

- **LAN discovery over mDNS (zero-infra Phase 3)** — hosts announce
  themselves as `_opacusr._tcp.local.` while the agent runs (TXT records:
  the signaling path, the protocol version, and — when direct LAN is
  accepted — the `ip:port` target). The viewer's connect tab lists the
  hosts found on this network with a Use button that fills the server
  URL, and a direct-LAN target when the ID field is empty. Best-effort
  end to end: no multicast on the network simply means an empty list;
  hosts running a different protocol version are filtered out. New
  config key `mdns_advertise` (default on) and CLI flag `--no-mdns`;
  `opacusr-agent config` reports the state.
- **Prometheus `/metrics` on the server** — the signaling server (and
  the agent's embedded one) now exposes counters and gauges in the text
  exposition format on `GET /metrics`: WebSocket upgrades, successful
  registrations, currently registered peers, relay tickets / completed
  pairs / active pipes / relayed bytes (both directions of every relay
  pipe counted at the read half), TURN credentials minted, per-IP rate
  limiting hits, and a `build_info` line with the version. Hand-rolled
  on purpose — no metrics dependency for the self-hosted binary.
- **Dependabot** — weekly cargo and github-actions updates, minor+patch
  grouped per ecosystem so breaking majors arrive as reviewable
  singletons.

## [0.8.0] — 2026-11-02

_Version note: the protocol bumped once more (7 → 8); peers with
different protocol versions are rejected at the handshake, so update
the agent and the viewer together._

### Added

- **Wayland cursor via the portal** — native Wayland hosts no longer
  degrade to a cursorless stream (the gap 0.7.0 left open): the
  ScreenCast session is now negotiated with `CursorMode::Metadata`, and
  the PipeWire worker parses every buffer's `spa_meta_cursor` — hotspot
  position plus the inline sprite bitmap — into a shared cursor state
  that the existing poller streams to viewers exactly like the
  X11/Win32 paths. Semantics follow PipeWire's `spa/buffer/meta.h`:
  a buffer without new cursor data leaves the state untouched, a
  position-only update moves the overlay without re-sending the sprite,
  and an empty sprite hides it. Sprite conversion supports the six
  usual DIB formats (BGRA/BGRx/RGBA/RGBx/ARGB/ABGR); the serial the
  viewer dedupes on is our own counter, bumped when the converted RGBA
  actually changes, so compositors that rescale bitmaps per frame don't
  flood the stream. The byte-level parser (`cursor_meta.rs`) reads the
  documented C layout directly — no bindgen structs, testable without a
  PipeWire build — and ships ten unit tests over synthetic metadata
  fixtures.
- **Chunked clipboard** — payloads above the single-message caps no
  longer get dropped at the 8 MiB frame limit: text over 512 KiB and
  PNG images over 6 MiB are split into 256 KiB `ClipboardChunk` pieces
  announced by a `ClipboardBegin` header (transfer id, payload kind,
  dimensions, total length, chunk size). The receiver reassembles with
  strict validation — chunk order and size, a 64 MiB reassembly cap, a
  UTF-8 check for text, and a fresh transfer silently replacing a stale
  one — so a clipboard several tens of MiB large syncs where it
  previously produced a warning and nothing else. Works in both
  directions, including the viewer's manual send-clipboard action.
- **Quality tier in the viewer UI** — the viewer no longer guesses why
  the stream looks softer: the host reports the adaptive-quality
  controller's live state in a new `QualityTier` message (tier 0..3,
  fps, JPEG quality, scale percent, adaptive on/off) right after the
  session is accepted, on every link-driven tier change, and when the
  user pins a manual quality. The session toolbar shows a colored badge
  (`⚡ best` … `tier 3`) with the live parameters on hover, and the
  stats panel gains a "Quality tier" row (adaptive / manual).

`PROTO_VERSION` bumps to 8 (`QualityTier`, `ClipboardBegin`,
`ClipboardChunk` are new messages); old peers are rejected at the
handshake, as with every protocol bump.

## [0.7.0] — 2026-11-02

_Version note: 0.6.0 was skipped — everything in this section landed
straight after 0.5.0. The protocol bumped twice (6 → 7); peers with
different protocol versions are rejected at the handshake, so update
the agent and the viewer together._

### Added

- **Image clipboard sync** — screenshots copy-paste across the session:
  both sides poll the clipboard for text first and fall back to images
  (screenshot copies usually carry no text), encode them as PNG on the
  sender and place them back on the receiver's system clipboard. Echo
  suppression now works on content hashes, so repeated polls of the
  same image cost one hash instead of one PNG encode. Guardrails: 6 MiB
  per image (transport frames cap at 8 MiB), a 64 MiB decoded-RGBA cap
  as a decompression-bomb guard, and a malformed payload is dropped
  with a log line while the session keeps running (covered by an e2e
  test). `PROTO_VERSION` bumps to 7 — old peers are rejected at
  handshake, as with every protocol bump.
- **Wayland-native capture ships for real** — the experimental
  xdg-desktop-portal ScreenCast + PipeWire backend is now compiled and
  lint-checked (`-D warnings`) on every CI run, so it can no longer rot
  silently, and releases gain a dedicated
  `opacusr-<ver>-linux-wayland-x86_64.tar.gz` artifact with it (runtime
  requirement: `libpipewire-0.3`, preinstalled on desktop distros; the
  default artifacts stay self-contained X11 builds). Hosts on a native
  Wayland session get a portal consent dialog on the first share and
  then full-frame capture; X11 hosts are unaffected.
- **Remote cursor overlay** — viewers now see the host's pointer:
  screen capture APIs drop the cursor, so the host polls it separately
  (Win32 `GetCursorInfo` + `GetIconInfo`/`GetDIBits` on Windows, XFixes
  on X11) and streams the pointer as its own messages — `CursorShape`
  (bitmap + hotspot, once per appearance change) and `CursorPos`
  (hotspot position, only when it moves; positions are native streamed-
  screen pixels and may sit outside the frame when the pointer is on
  another monitor, in which case the viewer hides the overlay). The
  viewer caches shapes (bounded) and draws the cursor on top of the
  video with the hotspot aligned to the reported position. Native
  Wayland sessions have no cursor backend yet and degrade silently;
  `--demo` ships a synthetic moving cursor. `PROTO_VERSION` bumps to 6.
- **TCP tunneling (SSH-style local forwarding)** — a viewer can forward
  a local port to a host-side target (`🔗` panel in the viewer sidebar:
  local port → target host:port; apps connect to `127.0.0.1:<local
  port>` and land on the target as seen from the host). The host gates
  every open with a `tunnel` policy in `agent.toml` / `--tunnel`:
  `off` (refuse everything), `loopback` (default — targets must resolve
  to the host's loopback interface, so local services are reachable but
  the session cannot be used to scan the network) and `any`
  (unrestricted, with a console warning). View-only sessions never get
  tunnels; both halves clean up on either side's EOF via a half-close
  handshake (`TunnelOpen`/`TunnelOpened`/`TunnelData`/`TunnelClose`).
  `SessionAccept` gained a `tunnel` flag, `PROTO_VERSION` bumps to 5
  (old peers refuse to talk to the new one).
- **Opt-in session recording (host-side)** — a host checkbox
  (`Host tab → Recording`) or `opacusr-agent --record` muxes every
  accepted session to a timestamped MP4
  (`<record_dir>/<date>-<time>-<peer>.mp4`, default
  `~/OpacusR-Recordings`): the encoded H.264 bitstream is repackaged
  Annex-B → AVCC and written sample-by-sample with real frame timings
  (90 kHz timescale, no transcoding). Streams rotate to `-partN` files
  on monitor-switch resolution changes; recording starts at the first
  keyframe and stops fail-open (the session itself never breaks).
  Viewers see a `● REC` indicator — `SessionAccept` gained a
  `recording` flag, `PROTO_VERSION` bumps to 4. JPEG-codec sessions are
  announced as unrecordable; audio is not muxed yet.
- **TOTP two-factor authentication (optional)** — hosts can enable a
  second factor (`Host tab → Two-factor (TOTP) → Enable`): a random
  base32 secret is generated and copied to the clipboard
  (`otpauth://totp/OpacusR?secret=…` for authenticator apps; RFC 6238,
  HMAC-SHA1, 30 s step, 6 digits, ±1 window for clock drift). Viewers
  then type a 6-digit code in the Connect tab; hosts verify it *in
  addition* to the password and feed wrong codes into the same
  lockout as wrong passwords. `PROTO_VERSION` bumps to 3 (old peers are
  refused cleanly). Verified against the RFC 6238 test vectors.
- **Viewer address book** — save contacts (label, 9-digit ID or `ip:port`,
  optional per-contact signaling endpoint tried before the global list),
  favorites, one-click connect from the list. Stored locally in
  `addressbook.toml` next to `agent.toml` (atomic tmp+rename writes;
  passwords are never stored).
- **Connection history** — the last 100 connection attempts with UTC
  timestamp, target, outcome (connected/failed) and session duration;
  viewable and clearable in the Connect tab.
- **Contact renaming** — every saved contact gets a `✎` button: edit the
  name inline (Enter or ✔ to apply, ✖ to cancel) and give the remote your
  own label instead of the ID-derived default. Target, endpoint and
  favorite are untouched.
- **Session statistics panel** — toggle `📊` in the session toolbar (or
  `Ctrl+Shift+S`): remote name, stream resolution, negotiated codec,
  actually received fps vs the host's fps hint, compressed wire bitrate
  (2 s sliding window) and RTT.
- **Session hotkeys** — `F11` fullscreen, `Ctrl+Shift+P` pause,
  `Ctrl+Shift+D` disconnect, `Ctrl+Shift+C/F/T/S` toggle the
  chat/files/terminal/stats panels. Letter shortcuts are ignored while a
  text field has focus. `ClientEvent::Connected` now carries the negotiated
  codec + fps hint; `ClientEvent::Frame` carries the compressed wire size.
- **Host → viewer file send** — while a session is live the Host tab offers
  `⬆ Send file to viewer`: a per-session command channel feeds
  `HostCommand::SendFile` into the agent session loop and the same FtEngine
  drives the push (sha256-verified on arrival). The viewer's Files panel
  gains a `⬆ Send files…` picker next to drag & drop.

### Changed

- **Adaptive quality considers arrival jitter** — the viewer now measures
  frame-arrival jitter (RFC 3550-style EWMA of |Δ − Δ̄|) and reports it in
  `LinkStats.jitter_ms` alongside RTT and received FPS. The host takes the
  worse of the RTT tier and the jitter tier (≥120 ms → at least tier 1,
  ≥250 ms → at least tier 2; jitter alone never reaches tier 3), so a link
  that bursts instead of streaming steadily now gets a gentler stream even
  when its RTT looks fine. Covered by five new controller unit tests.

## [0.5.0] — 2026-11-01

### Added

- **Host physical-input lock** (`--lock-input` / `lock_input` in `agent.toml` / GUI
  checkbox) — TeamViewer-style: while a viewer has full control the host's
  physical keyboard/mouse is swallowed while input injected through the session
  still passes. Windows uses low-level hooks (only non-injected events are
  dropped; **Ctrl+Alt+Del stays available**), Linux grabs `/dev/input/event*`
  (`EVIOCGRAB`, needs the `input` group). View-only sessions never lock; the
  lock is fail-open (a platform that cannot take it warns and continues) and
  crash-safe (released on drop and on process death). macOS: not yet.
- **Audio-free builds** — `cpal` is optional behind the existing `audio`
  feature on both the agent and the client, so `cargo build
  --no-default-features` and the test suite work on hosts without ALSA dev
  headers (the GUI opts back in via `features = ["audio"]`).
- **Zero-infra networking, phases 1–2** ([docs/serverless.md](docs/serverless.md)):
  - **Multi-endpoint signaling fan-out on the agent** — `signaling_urls` in
    `agent.toml`: the agent registers on every endpoint simultaneously, one
    supervised reconnect loop per endpoint (capped exponential backoff), so a
    flapping endpoint never takes the others down. Each endpoint assigns its
    own ID; `opacusr-agent id` prints all of them. The signaling accept path
    shares one auth-failure budget across all endpoints (lockout unchanged).
  - **Embedded signaling server in the agent** — `signal_listen = true` (or
    `opacusr-agent run --signal-listen`) serves the peer registry, `/health`,
    `GET /turn` and the TCP relay inside the agent process
    (`signal_listen_port`, relay on port +1; same env knobs as the standalone
    server: `OPACUSR_RATE_MAX`, `OPACUSR_MAX_PEERS`, `OPACUSR_TURN_SECRET`).
    The agent registers on the local endpoint automatically — no separate
    server binary needed; expose it behind a free tunnel for remote access.
  - **Viewer-side endpoint ladder** — the GUI tries the configured signaling
    endpoints in order (per-endpoint TURN credential minting, 25 s budget
    each) and the first handshake that completes wins; per-endpoint progress
    and failures show up in the session status line.
  - **Tunnel cookbook** — ready-to-copy cloudflared and Tailscale configs
    for the zero-infra setup (docs/serverless.md §7).


## [0.4.0] — 2026-10-31

### Added

- **Server hardening: rate limits + time-limited TURN credentials** — per-IP limits on new signaling connections and `/turn` requests (`OPACUSR_RATE_MAX`, default 20 per 10 s), a registered-peer cap (`OPACUSR_MAX_PEERS`, default 1024), relay-ticket caps (8 per peer, 4096 server-wide), and `GET /turn` minting coturn REST-API credentials from `OPACUSR_TURN_SECRET` (HMAC-SHA1, `OPACUSR_TURN_TTL_SECS`).

- **Agent-side auth lockout** — failed password proofs are counted per accept path (signaling / LAN): after `auth_max_attempts` (default 5) failures within `auth_window_secs` (default 300 s) the agent refuses new handshakes for `auth_lockout_secs` (default 60 s, doubling per consecutive lockout, capped at 15 min; `0` disables). The budget is shared across all peers of the path, so rotating peer IDs or display names no longer helps an online brute-force — with defaults it is capped at ~20 guesses per hour even under sustained attack. Tunables in `agent.toml`; every failure and lockout is logged.

- **Automatic time-limited TURN credentials in clients** — the agent and the GUI viewer fetch the coturn REST pair from the server's `GET /turn` at WebRTC session setup (`turn_api = true` in `agent.toml`), cache it for the server-declared TTL (a 503 disables the fetch for the run), and fall back to STUN-only on any failure. Completes the `OPACUSR_TURN_SECRET` flow: coturn serves per-session credentials instead of a shared static secret.


- **Docs: checksum verification on every OS + Windows SmartScreen first run** —
  README.md / README.ru.md gain per-OS verification commands (`sha256sum -c`,
  `shasum -a 256 -c`, PowerShell `Get-FileHash`, `certutil -hashfile`) and the
  expected SmartScreen prompt for the unsigned MSI with the More info ->
  Run anyway steps.


- **macOS: ad-hoc signing + Gatekeeper docs** — release binaries are ad-hoc
  codesigned (`codesign --force --sign -`) in the release workflow before
  packaging, so they run on Apple Silicon; README.md / README.ru.md document
  the expected one-time Gatekeeper prompt and the ways to clear it (`xattr
  -cr`, right-click Open, Privacy & Security -> Open Anyway).

- Update check: the GUI app and the agent query GitHub for the latest published release at startup and show a notice when a newer version is available. No download and no auto-install; disable with OPACUSR_NO_UPDATE_CHECK=1.

- **Docs: zero-infra networking design** ([docs/serverless.md](docs/serverless.md)) — how to run OpacusR without renting servers: automatic connection ladder (LAN direct → P2P/STUN → TURN → TCP relay), signaling embedded in the agent behind free tunnels (cloudflared / Tailscale Funnel), mesh-VPN mode, mDNS discovery and manual "meeting code" fallback; phased implementation plan.


### Changed
- Wayland hosts fail fast with a clear error (PipeWire portal feature or an X11 session; `OPACUSR_ALLOW_X11_ON_WAYLAND=1` forces the old behavior) instead of a silent blank XWayland capture, and agents print a startup warning under Wayland.

### Fixed
- **An agent with no configured password no longer accepts an empty one.** The host previously verified proofs against an empty string when no unattended password was set (`effective_password().unwrap_or_default()`), so a viewer submitting an empty password could authenticate in attended mode (e.g. direct LAN sessions). Now the missing password propagates as `None` and the handshake is rejected with "no password configured on the host".

## [0.3.1] — 2026-10-31

Release engineering, documentation and community tooling. **No functional
changes to the `opacusr-agent`, `opacusr-server` or GUI app binaries** —
upgrading is optional.

### Added

- All release artifacts now ship with a `SHA256SUMS` checksum file;
  verify your download with `sha256sum -c SHA256SUMS`.
- The Release workflow smoke-tests every build (`--version` for the agent
  and server binaries) on Linux, Windows and macOS before publishing, so
  a broken build can no longer reach a release.
- `SECURITY.md`: security policy with private vulnerability reporting,
  a 72-hour acknowledgment target, and the scope (transport crypto,
  relay, password flow).
- GitHub issue templates for bug reports and feature requests; blank
  issues are disabled.
- Categorized release-note configuration (`.github/release.yml`) and the
  release body now generated from this changelog (`body_path`).

### Changed

- `README.md` / `README.ru.md`: added Release / CI / Release build /
  License badges and a Download section.

### Fixed

- Linux: release binaries failed to start with "libopus.so.0: cannot open
  shared object file" — they linked a vendored shared Opus without an
  embedded runtime path. The new release smoke test caught this; the
  v0.3.0 Linux tar.gz was affected the same way. Opus is now built as a
  static PIC library (as on macOS), so the binaries, the tar.gz and the
  .deb packages are self-contained and no longer require a system Opus.

## [0.3.0] — 2026-10-30

Protocol version 2 (`SessionAccept` gained `view_only`/`screens`/
`audio_available`; new messages `LinkStats`, `SelectScreen`,
`AudioControl`, `AudioFrame`). Mixed 0.2/0.3 versions are rejected
cleanly with a version-mismatch message.

### Added

- **Clipboard sync** — both directions with echo suppression, driven by a
  dedicated clipboard worker thread (`opacusr-input::clipboard`); 512 KiB
  size cap; live toggle in the viewer toolbar; `clipboard_sync` in
  `agent.toml`. Headless hosts degrade to no-op.
- **View-only sessions + ACL** — the host-side grant is authoritative:
  input is ignored and file/terminal requests are explicitly refused in
  view-only sessions. Consent is now three-way (full / view-only / deny)
  in both the GUI dialog and the CLI prompt. Unattended policy:
  `default_permission = full | view-only | deny` plus per-viewer
  `acl_rules` (matched on the self-reported display name — the session
  password remains the real gate).
- **Adaptive quality** — the viewer reports `LinkStats` (RTT + received
  FPS) on every pong; the host maps reports onto four quality tiers with
  fast-degrade / slow-upgrade hysteresis. A manual `QualitySet` disables
  adaptation for the session. `adaptive_quality` in `agent.toml`.
- **Windows: DXGI Desktop Duplication capture** — frames straight from
  DWM (hardware overlays, fullscreen apps), BGRA→RGBA readback through a
  staging texture, idle-timeout frame caching, automatic rebuild on
  `ACCESS_LOST` (UAC prompts, desktop switches). Backend selection via
  `capture = auto | gdi | dxgi` and `--capture`; GDI remains the fallback.
- **Wayland: PipeWire screencast (experimental)** — behind the non-default
  `pipewire` cargo feature: xdg-desktop-portal ScreenCast consent, then a
  PipeWire consumer stream (BGRx/RGBA/RGB). A native Wayland session
  prefers the portal and falls back to X11/XWayland.
- **Multi-monitor** — backends enumerate monitors (`ScreenCapture::screens`,
  RandR on X11, `EnumDisplayMonitors` on GDI, DXGI adapter outputs);
  the viewer picks one in the toolbar (`SelectScreen`); input rescaling
  follows the streamed monitor.
- **Audio streaming** — Opus 48 kHz mono, 20 ms frames, ~32 kbps.
  Windows: WASAPI shared-mode *loopback* of the default output device
  (the machine's actual sound). Linux/macOS: `cpal` capture (microphone,
  or a Pulse monitor by name via `audio_device =`). The viewer plays
  through the default output with a small prebuffer; toolbar toggle when
  the host advertises `audio_available`.
- `cargo-deb` agent package now depends on `libasound2`; CI installs
  `libasound2-dev` on Linux.

### Changed

- `SessionAccept` extended (see protocol v2 above); old peers are refused
  with a clean version mismatch.
- `opacusr-agent --run` gained `--capture auto|gdi|dxgi`.
- Agent audio is a default cargo feature (`audio`); opt out with
  `--no-default-features` where the vendored opus build is unavailable
  (e.g. windows-gnu cross builds). Windows MSVC links the prebuilt opus
  shipped by `audiopus_sys`.

### Fixed

- Windows MSI: the WiX authoring used the WiX v4 element
  `<StandardDirectory>`, which the WiX v3.11 toolchain installed by the
  Release workflow rejects with CNDL0005 (and light then failed on the
  missing `main.wixobj`, LGHT0103). Rewritten with the classic
  `TARGETDIR` directory tree and all components marked `Win64` (avoids
  ICE80 for files under `ProgramFiles64Folder`). The MSI product version
  now comes from the release tag (`candle -dMsiVersion=<tag>` with a
  fallback default), so it no longer has to be bumped by hand.
- Windows build: the WASAPI loopback code in `opacusr-agent/src/audio.rs`
  did not compile on Windows. The `WAVEFORMATEX`/`WAVEFORMATEXTENSIBLE`
  imports were function-local while `parse_wave_format` needed them too
  (E0425); `AUDCLNT_BUFFERFLAGS_SILENT` is generated as `i32` by the
  `windows` crate and was ANDed with a `u32` counter (E0308/E0277); and
  rustc (E0793) rejects reading the `SubFormat` field of the packed
  `WAVEFORMATEXTENSIBLE` through a reference. Imports moved to module
  scope, the flag is cast to `u32`, the GUID is read via
  `addr_of!` + `read_unaligned`.
- macOS build: `audiopus_sys` fell back to building the vendored Opus
  with autotools, but macOS runners ship no `autoreconf`, so the build
  script panicked ("Failed to autogen Opus"). CI and Release now install
  `opus` + `pkg-config` via Homebrew first; `audiopus_sys` probes
  pkg-config before vendoring and links the system Opus.
- Release (macOS): the tar.gz job now builds Opus from the official
  release tarball (`./configure --enable-static`, ships a pre-generated
  `configure`, so no autotools are needed) instead of the Homebrew
  bottle. The binaries link Opus statically and run on machines without
  Homebrew; CI keeps the brew install, which is enough for clippy and
  tests.

## [0.2.0] — 2026-10-30

### Added

- **H.264 video** via Cisco OpenH264 (BSD-licensed, compiled from the
  bundled source): screen-content usage profile, bitrate mode with a
  quality→bpp heuristic, forced IDR on session start and every 120 frames,
  automatic re-init on resolution change. JPEG remains the fallback
  (`codec = auto | h264 | jpeg` in `agent.toml`); a build without the
  `h264` feature gracefully degrades to JPEG end-to-end. The E2E test now
  exercises the real H.264 encode→transport→decode path.
- **OS keychain for the unattended password**: `opacusr-agent passwd`
  stores into Windows Credential Manager / macOS Keychain / Linux Secret
  Service (vendored libdbus — no system dbus package needed) and keeps the
  config file password-free; `passwd --file` forces the old 0600-file
  storage; `passwd-clear` wipes both. `opacusr-agent config` shows the
  storage location. The GUI host panel follows the same policy.
- **Packaging**: Debian packages for the agent (with a systemd user unit)
  and the viewer (with `.desktop` entry and hicolor icon); a Windows MSI
  (WiX v3: viewer + agent + server, Start-menu shortcut, embedded icon);
  a `Release` workflow that builds tar.gz/zip/deb/msi and publishes a
  GitHub Release on every `v*` tag.
- Application icon (PNG/ICO), embedded into the Windows executable.
- `CHANGELOG.md` (this file).

### Changed

- `CodecId` gained an `H264` variant; `SessionAccept` may now announce it.
  Bincode discriminants stay compatible (append-only enum).
- `opacusr-agent` config gained `codec` and `password_in_keychain` keys;
  old configs load unchanged (serde defaults).
- Workspace version bumped to 0.2.0.

### Fixed

- CI on Windows: `Win32Capture` now implements `Send` (safe: the `HWND`
  is process-wide and GDI resources live within one capture call), an
  explicit `Default` impl and a fully-initialised `RGBQUAD` satisfy
  clippy; the unused `allow_test_fallback` on the Windows path is
  silenced with an explicit `let _ =`.
- CI on macOS: `Insert` / `NumLock` / `PrintScreen` key mappings are
  compiled out (enigo 0.6 has no such `Key` variants on macOS); the
  agent reports them as unsupported keys instead of failing the build.

## [0.1.0] — 2026-10-29

### Added

- Rust workspace of 10 crates: proto, crypto, transport (WebRTC via
  webrtc-rs + TCP direct + TCP relay + chunking + secure session +
  file-transfer engine + signaling client), capture (X11 / Win32 GDI /
  test source), codec (JPEG), input (enigo + test doubles), server (axum
  signaling + relay), agent (PTY, unattended), client (viewer session
  logic), app (egui GUI).
- Security model: Argon2id PSK → HMAC-SHA256 proof → HKDF-SHA256 →
  ChaCha20-Poly1305 with counter nonces; replay = fatal.
- Docker Compose (signaling + coturn), CI matrix (Linux/Windows/macOS),
  bilingual README, docs (architecture, protocol, security, platforms,
  roadmap), AGPL-3.0.
