# TS LAN Overlay

[English](#english) | [Русский](#русский)

## English

A "who is talking" overlay for when TeamSpeak 3 runs on one computer and the game on another one in the same home network.

- **The TeamSpeak plugin** goes on the computer with TeamSpeak. It broadcasts over the local network who is talking, who is whispering and who is in the channel.
- **The TS LAN Overlay program** goes on the gaming computer and lives in the system tray. It draws two windows on top of the game:
  - **"Кто говорит" (Talking)**: whispers on top (orange dot), channel voice below (green dot);
  - **"Список канала" (Channel list)**: everyone in the channel. Someone who has just joined is highlighted in blue.
- **Per-person volume** is changed with the mouse wheel right in the Talking window on the gaming computer. The command goes to TeamSpeak on the other computer.

The computers find each other automatically, no IP addresses to enter. The windows do not get in the way of the game: clicks pass through them and they never take focus. The game must run in borderless windowed mode.

If TeamSpeak and the game run on the same computer, see [TS Talk Overlay](https://github.com/amveng/ts3_talk_overlay).

The user interface is in Russian.

### Installation

Download both installers of the same version from [Releases](https://github.com/amveng/ts3_lan_overlay/releases):

| Computer | Installer |
|---|---|
| with TeamSpeak | `ts_lan_overlay_plugin_setup_X.Y.Z.exe`, then enable "TS LAN Overlay" in TeamSpeak → Options → Addons |
| gaming | `ts_lan_overlay_setup_X.Y.Z.exe`; the program starts by itself, starting with Windows is a checkbox in the installer |

The installers open the needed ports in Windows Firewall.

### Controls (on the gaming computer)

| Key | Action |
|---|---|
| Ctrl+Alt+T | hide or show the Talking window |
| Ctrl+Alt+L | hide or show the Channel list |
| Ctrl+Alt+V | volume mode |
| Ctrl+Alt+M | arrange mode |

**Volume mode:** the Talking window gets a green frame and starts taking the mouse. The wheel over a nickname makes that person louder or quieter in 1 dB steps; a right click resets to 0. Pressing Ctrl+Alt+V again or 10 seconds without action returns to normal. While the mouse is over the window the list is frozen, so the wheel never lands on someone else when a new speaker appears. The plugin remembers the volume by the person's unique ID (`ts3_lan_overlay_volumes.txt` next to the DLL) and applies it again when they rejoin and after a TeamSpeak restart; a volume changed later in TeamSpeak itself takes priority.

**Arrange mode:** both windows are shown, even when empty (with sample rows), get a blue frame and take the mouse. Drag the middle to move a window, the left or right edge to change its width, the top or bottom edge or a corner to change the font size. When you release the mouse the monitor, corner, offsets, width and font size are saved to the settings. Ctrl+Alt+M again returns to normal.

Look, monitors, colours and hotkeys are set from the tray icon → "Настройки" (Settings).

### How it works

| Direction | Port | What is sent |
|---|---|---|
| plugin → program | UDP 39391, broadcast | a state snapshot on every change and once a second |
| program → plugin | UDP 39392 | a volume command |

The packet format is plain text, described in [`proto/src/lib.rs`](proto/src/lib.rs).

### Building from source

Requires [Rust](https://rustup.rs) (MSVC) and [Inno Setup 6](https://jrsoftware.org/isinfo.php).

```powershell
cargo build --release   # target\release\ts3_lan_overlay.dll and ts_lan_overlay.exe
.\installer\build.ps1   # both installers in dist\
```

Parts of the project:
- `proto` — packet format;
- `plugin` — TeamSpeak plugin (API 26, client 3.6);
- `overlay` — the program with the windows.

### Code signing policy

Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org).

Installers are built from the source code in this repository by [GitHub Actions](.github/workflows/build.yml), and every release is signed only after manual approval.

Team roles:
- Committers and reviewers: [TakeChance](https://github.com/amveng)
- Approvers: [TakeChance](https://github.com/amveng)

### Privacy policy

The plugin sends data only inside the local network, and only to the TS LAN Overlay program; nothing is sent to the internet or to any third party.

- The plugin broadcasts over the local network (UDP 39391) the nicknames, who is talking and who is in the channel on the current TeamSpeak tab — exactly what the overlay windows show.
- The program sends the plugin (UDP 39392) only volume commands that the user makes with the mouse wheel.
- Nothing else is sent anywhere. Settings, volumes and logs are kept locally.

---

## Русский

Оверлей «кто говорит» на случай, когда TeamSpeak 3 запущен на одном компьютере, а игра на другом в той же домашней сети.

- **Плагин для TeamSpeak** ставится на компьютер с TeamSpeak. Он рассылает по локальной сети, кто говорит, кто шепчет и кто в канале.
- **Программа TS LAN Overlay** ставится на игровой компьютер и живёт в трее. Она рисует поверх игры два окна:
  - **«Кто говорит»**: шёпот наверху (оранжевая точка), ниже голос в канале (зелёная точка);
  - **«Список канала»**: все, кто в канале. Ник того, кто только что зашёл, подсвечивается голубым.
- **Громкость отдельного человека** меняется колесом мыши прямо из окна «Кто говорит» на игровом компьютере. Команда уходит в TeamSpeak на второй компьютер.

Компьютеры находят друг друга сами, IP-адреса вписывать не нужно. Окна не мешают игре: клики проходят сквозь них, фокус они не забирают. Игра должна быть в режиме «Оконный без рамки» (Borderless).

Если TeamSpeak и игра на одном компьютере, смотрите [TS Talk Overlay](https://github.com/amveng/ts3_talk_overlay).

### Установка

Скачайте оба установщика одной версии со страницы [Releases](https://github.com/amveng/ts3_lan_overlay/releases):

| Компьютер | Установщик |
|---|---|
| с TeamSpeak | `ts_lan_overlay_plugin_setup_X.Y.Z.exe`, затем включите «TS LAN Overlay» в TeamSpeak → Параметры → Дополнения |
| игровой | `ts_lan_overlay_setup_X.Y.Z.exe`, программа запустится сама; автозапуск с Windows включается галочкой в установщике |

Плагин и программа должны быть **одной версии**: разные версии друг друга не видят.

Установщики сами разрешают нужные порты в брандмауэре Windows. Они не подписаны, поэтому Windows может показать «Система Windows защитила ваш компьютер». Нажмите «Подробнее» → «Выполнить в любом случае».

Подробная инструкция и разбор логов: [Инструкция по установке.txt](Инструкция%20по%20установке.txt).

### Управление (на игровом компьютере)

| Клавиша | Что делает |
|---|---|
| Ctrl+Alt+T | скрыть или показать «Кто говорит» |
| Ctrl+Alt+L | скрыть или показать «Список канала» |
| Ctrl+Alt+V | режим громкости |
| Ctrl+Alt+M | режим расположения окон |

**Режим громкости:** окно «Кто говорит» получает зелёную рамку и начинает ловить мышь. Колесо над ником делает человека громче или тише с шагом 1 дБ, правый щелчок сбрасывает на 0. Повторное Ctrl+Alt+V или 10 секунд без действий возвращают обычный режим. Пока мышь над окном, список заморожен: порядок и число строк не меняются, обновляются только точки и громкость; новые говорящие появляются, когда мышь уходит. Плагин запоминает громкость по уникальному ID человека (`ts3_lan_overlay_volumes.txt` рядом с DLL) и выставляет её снова при перезаходе и после перезапуска TeamSpeak; громкость, изменённая потом в самом TeamSpeak, имеет приоритет.

**Режим расположения:** оба окна видны, даже если пусты (с образцами строк), получают голубую рамку и ловят мышь. За середину окно перетаскивается, левый и правый край меняют ширину, верхний и нижний край и углы меняют размер шрифта (высота подстраивается под строки). Когда отпускаете мышь, монитор, угол, отступы, ширина и шрифт записываются в настройки; угол выбирается по четверти монитора, в которой оказалось окно. Повторное Ctrl+Alt+M возвращает обычный режим.

Внешний вид, мониторы, цвета и клавиши настраиваются через значок в трее → «Настройки».

### Как это работает

| Направление | Порт | Что передаётся |
|---|---|---|
| плагин → программа | UDP 39391, широковещательно | снимок состояния при каждом изменении и раз в секунду |
| программа → плагин | UDP 39392 | команда громкости |

Формат пакетов текстовый, описан в [`proto/src/lib.rs`](proto/src/lib.rs).

### Сборка из исходников

Нужны [Rust](https://rustup.rs) (MSVC) и [Inno Setup 6](https://jrsoftware.org/isinfo.php).

```powershell
cargo build --release   # target\release\ts3_lan_overlay.dll и ts_lan_overlay.exe
.\installer\build.ps1   # оба установщика в dist\
```

Части проекта:
- `proto` — формат пакетов;
- `plugin` — плагин TeamSpeak (API 26, клиент 3.6);
- `overlay` — программа с окнами.

### Подпись кода

Бесплатная подпись кода — [SignPath.io](https://about.signpath.io), сертификат — [SignPath Foundation](https://signpath.org).

Установщики собираются из исходников этого репозитория в [GitHub Actions](.github/workflows/build.yml), и каждый релиз подписывается только после ручного подтверждения.

Роли:
- авторы и ревьюеры: [TakeChance](https://github.com/amveng);
- подтверждают подпись: [TakeChance](https://github.com/amveng).

### Приватность

Плагин передаёт данные только внутри локальной сети и только программе TS LAN Overlay; в интернет и третьим лицам ничего не уходит.

- Плагин рассылает широковещательно по локальной сети (UDP 39391) ники, кто говорит и кто в канале на текущей вкладке TeamSpeak — ровно то, что показывают окна оверлея.
- Программа отправляет плагину (UDP 39392) только команды громкости, которые пользователь сам задаёт колесом мыши.
- Больше ничего никуда не передаётся. Настройки, громкости и логи хранятся локально.

---

## License / Лицензия

[MIT](LICENSE)
