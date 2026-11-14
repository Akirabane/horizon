<p align="center">
  <img src="docs/media/banner.webp" alt="Horizon: the game you know, rebuilt in Rust" width="100%">
</p>

<p align="center">
  <a href="https://playhorizon.dev"><b>Website</b></a> ·
  <a href="#download"><b>Download</b></a> ·
  <a href="https://playhorizon.dev/docs/"><b>Docs</b></a> ·
  <a href="https://discord.gg/playhorizon"><b>Discord</b></a> ·
  <a href="ROADMAP.md"><b>Roadmap</b></a> ·
  <a href="#contributing"><b>Contributing</b></a>
</p>

<p align="center">
  <a href="#project-status"><img src="https://img.shields.io/badge/player--visible%20gaps-0%20open-F0795A" alt="Player-visible gaps open"></a>
</p>

# Horizon

Minecraft: Java Edition 26.3, rewritten from scratch in Rust. The same rules and the same terrain, your own worlds, in one native program without Java.

Horizon is free, open source and unofficial. You play it with your own copy of Java Edition.

<p align="center">
  <img src="docs/media/readme/valley.webp" alt="A river valley with a plains village, savanna hills and a cherry grove, seen from above with a long view distance" width="100%">
</p>

<table>
  <tr>
    <td width="50%"><img src="docs/media/readme/sunset.webp" alt="Sunset behind a cherry grove on a hill, with a river below"></td>
    <td width="50%"><img src="docs/media/readme/nether.webp" alt="A crimson forest in the Nether, with a piglin among the fungi"></td>
  </tr>
  <tr>
    <td width="50%"><img src="docs/media/readme/survival.webp" alt="Survival in a village square: two villagers, flowers, the hotbar, health and hunger"></td>
    <td width="50%"><img src="docs/media/readme/multiplayer.webp" alt="Two players in a LAN world: the other player's name tag and the player list"></td>
  </tr>
</table>

<sub>Nightly build of 2026-11-05 on a Mac mini with an Apple M4 Pro. Vanilla textures, no mods, render distance 32 with distant terrain on.</sub>

## Download

Get the launcher from **[playhorizon.dev](https://playhorizon.dev)** or the [releases page](https://github.com/horizon-rust/horizon/releases?q=launcher&expanded=true). It runs on Windows, macOS and Linux, installs the game and keeps it up to date.

You need a Microsoft account that owns Minecraft: Java Edition. Horizon contains no Minecraft code or assets. The [install guide](https://playhorizon.dev/docs/install) walks you through setup.

<table>
  <tr>
    <td width="50%"><img src="docs/media/readme/launcher-home.webp" alt="The launcher's Home page: the Horizon instance, Play, the version picker, the friend invite field, screenshots and the patch log"></td>
    <td width="50%"><img src="docs/media/readme/launcher-mods.webp" alt="The launcher's Mods page: Horizon's mod registry with Zoom selected, and tabs for modpacks, resource packs, shader packs and data packs"></td>
  </tr>
</table>

## Features

- **Vanilla gameplay.** Survival, creative, hardcore and spectator, world generation in all three dimensions, mobs, villagers, raids, redstone and every command. Your saves open as they are.
- **Multiplayer.** Join vanilla and Horizon servers, open a world to LAN, or invite friends with a code.
- **Graphics.** Distant terrain up to 1,024 chunks, HDR and controller support. Anything that changes the vanilla look is off by default.
- **Mods and packs.** Resource, data and shader packs, plus content mods and native Rust mods from the launcher. See the [mods guide](https://playhorizon.dev/docs/mods).
- **Servers.** Host one from the launcher, or run `horizon-server` on a VPS with no Java. See the [hosting guide](https://playhorizon.dev/docs/hosting).
- **Agent control.** Scripts and AI agents can play through `horizon-ctl`. See the [agent guide](docs/native-client.md#agent-control-horizon-ctl-and-mcp).

## Performance

Horizon v0.2.97 against Java Edition on the same machine and world, Ryzen 7 9800X3D with an RTX 5080 at 1280x720. Mean frames per second over four scenes:

| Render distance | Horizon | Vanilla 26.3 | Vanilla + mods |
| --- | ---: | ---: | ---: |
| 8 chunks | 3,289 | 2,437 | 2,375 |
| 16 chunks | 2,074 | 1,661 | 1,586 |
| 32 chunks | 1,207 | 1,031 | 879 |

Horizon reaches the title screen in 2.9 to 3.2 seconds, against 7.5 to 7.7 for vanilla. The mods are Sodium, Lithium, FerriteCore and ImmediatelyFast. The [full report](benchmarks/perf/windows-9800x3d-5080-2026-10-29.md) has every scene and the [benchmark guide](docs/benchmark.md) the method.

## Build from source

You need Rust 1.90 or later, Python 3.11 or later, JDK 25 and your own copy of Java Edition 26.3 with its server jar. On Linux, also `libasound2-dev` and `libudev-dev`.

```sh
git clone https://github.com/horizon-rust/horizon.git && cd horizon
cargo run --release -p horizon-prepare -- --prepare --server-jar ~/Downloads/server.jar
cargo run --release -p horizon-client
```

The [native client guide](docs/native-client.md) explains where the jar comes from, the options and the controls. Never commit or share the prepared files.

## Contributing

Issues and pull requests are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md), then run the self-check before you open a pull request:

```sh
git config core.hooksPath .githooks
python3 tools/pr_ready.py
```

Questions go to [Discord](https://discord.gg/playhorizon), and [GOVERNANCE.md](GOVERNANCE.md) explains how the project is run.

## Project status

**Player-visible gaps: 0 open** of the 833 the [port ledger](tools/port_ledger.py) found, reading every vanilla 26.3 method against Horizon's code: vanilla behaviour a player would notice that Horizon doesn't have yet. By area: Animals [0](https://github.com/horizon-rust/horizon/issues/3512); Monsters and bosses [0](https://github.com/horizon-rust/horizon/issues/3513); Entities, projectiles, vehicles and decorations [0](https://github.com/horizon-rust/horizon/issues/3514); Items, enchantments and inventories [0](https://github.com/horizon-rust/horizon/issues/3515); Blocks, redstone and fluids [0](https://github.com/horizon-rust/horizon/issues/3516); World, world generation and saves [0](https://github.com/horizon-rust/horizon/issues/3517); Screens, HUD and client packets [0](https://github.com/horizon-rust/horizon/issues/3518); Entity and block rendering, models [0](https://github.com/horizon-rust/horizon/issues/3519); Server and players on the server [0](https://github.com/horizon-rust/horizon/issues/3520); Mob AI, villagers and raids [0](https://github.com/horizon-rust/horizon/issues/3521). This number goes down as gaps close (`tools/parity_gaps.py`); the table below says what the harnesses compare.

### What the harnesses compare

Every subsystem has differential harnesses that compare Horizon with vanilla. A score of 1 means they cover it and pass. The [status page](docs/status.md) lists what each one compares, and [known issues](docs/known-issues.md) lists what players still run into.

| Subsystem | Score | Main gaps |
| --- | ---: | --- |
| Window, input and game loop | 1 | None |
| Assets and resources | 1 | None |
| Network protocol and multiplayer | 1 | None |
| Chunk storage and palettes | 1 | None |
| Section meshing | 1 | None |
| Rendering | 1 | None |
| Lighting | 1 | None |
| World generation | 1 | None |
| Region files and saves | 1 | None |
| Entities and physics | 1 | None |
| Interaction and inventory | 1 | None |
| Menus, HUD and chat | 1 | None |
| Audio | 1 | None |
| Block behaviours and ticking | 1 | None |

## Licence

[MIT](LICENSE), for the project's original work; dependencies keep their own licences ([notices](THIRD_PARTY_NOTICES.md)). Horizon is made by [Ludovic Marie](https://lu4ovic.com) ([@ludovic111](https://github.com/ludovic111)) and [its contributors](https://github.com/horizon-rust/horizon/graphs/contributors); support it on [GitHub Sponsors](https://github.com/sponsors/ludovic111).

NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG OR MICROSOFT. Minecraft is a trademark of Mojang Synergies AB; its code and assets belong to Mojang and Microsoft under the [Minecraft EULA](https://www.minecraft.net/en-us/eula).
