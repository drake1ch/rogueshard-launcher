# Rogue Shard Launcher

Лаунчер для [Stoneshard](https://store.steampowered.com/app/625960/Stoneshard/) с модом
[Rogue Shard](https://drake1ch.github.io/rogueshard-wiki/): собирает моды из своей папки `mods`
в `data.win` и запускает игру на движке Rogue Shard. Папку игры он не меняет.

## Установка

1. Stoneshard должна быть установлена (Steam). Если лаунчер не нашёл игру, укажите её папку:
   «Игра», «Указать папку».
2. Скачайте `RogueShard_Launcher-<n>.zip` из [Releases](../../releases) и распакуйте в любую папку.
3. Запустите `Launcher.exe` и нажмите «Играть».

Сохранения и настройки общие с обычной игрой. Steam засчитывает время игры.

## Обновления

В настройках лаунчера есть два переключателя, оба включены по умолчанию:

- **Обновлять мод автоматически**: при запуске лаунчер скачивает новый `RogueShard.sml` из
  последнего релиза `v<версия>` этого репозитория.
- **Обновлять лаунчер автоматически**: новый лаунчер (релиз `launcher-<n>`) ставится поверх
  старого. Папка `mods`, настройки и сборка остаются.

---

# Rogue Shard Launcher (English)

A launcher for Stoneshard with the Rogue Shard mod. It patches the mods in its `mods` folder into
a `data.win` and runs the game on the Rogue Shard engine, without changing the game's folder.

Download `RogueShard_Launcher-<n>.zip` from [Releases](../../releases), unpack it anywhere and run
`Launcher.exe`. Settings has two switches, both on by default: the mod updates itself from the
newest `v<version>` release, and the launcher from the newest `launcher-<n>` release.

## Building

`cargo build --release` (Rust, Windows; WebView2 runtime). The engine (`rsr.exe`) and the mod
patcher (`patcher\`) ship in the release zip and are not part of this repository.
