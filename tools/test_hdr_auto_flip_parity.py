#!/usr/bin/env python3
"""HDR Auto follows the display without meshing the world again (#12989): Auto on -> off -> on.

One game runs offscreen on the float frame (HORIZON_HDR_FRAME=float, HORIZON_HDR_OUTPUT=pq) with HDR on Auto, under
HORIZON_STILL=1. No HDR display is needed: HORIZON_HDR_DISPLAY names a file that says what the system reports for the
display, `on` or `off`, which the game reads every two seconds as it reads the system. The file starts at `on`, turns
`off`, then `on` again, and the scenes are shot in each state. A second game starts with the file at `off`.

It fails unless:
- HDR output follows the file: the shots have their -hdr.png while it says `on`, and none while it says `off`;
- the renderer was made again for each frame (the 8-bit one, then the float one), so the flips were real;
- no section was meshed again by either flip (`meshed_sections` of horizon-ctl status), and as many stay resident;
- the pictures after on -> off -> on are the first ones, pixel for pixel, the -hdr.png too;
- the pictures while `off` are those of the game that started at `off`.

Horizon's own feature: there is no vanilla oracle, and the pictures compared are all this build's.

  test_hdr_auto_flip_parity.py [--bin-dir DIR] [--resources DIR] [-j N]
"""
import argparse
import os
import shutil
import sys
import time
from pathlib import Path

try:
    from . import lab
    from . import test_frame_identity_parity as identity
except ImportError:
    import lab
    import test_frame_identity_parity as identity

RUN = lab.LOCAL / 'hdr-auto-flip'
SCENES = ['day_water', 'night_lights']
ENV = {'HORIZON_HDR_FRAME': 'float', 'HORIZON_HDR_OUTPUT': 'pq'}
MADE_AGAIN = 'the renderer was made again for the frame'


def status(game):
    reply = game.ctl('status')
    return reply['meshed_sections'], reply['renderer']['resident_sections']


def made_again(game):
    return str(game.ctl('logs', '--lines', '4000', check=False)).count(MADE_AGAIN)


def flip(game, display, to):
    """The system's HDR turns `to`: once the game has made its renderer for the other frame (the display is read
    every two seconds, and Auto waits half a second more before it turns off), how many sections that meshed and
    how many are resident before and after."""
    game.settle()
    before, made = status(game), made_again(game)
    display.write_text(to)
    deadline = time.monotonic() + 60
    while made_again(game) == made:
        if time.monotonic() > deadline:
            raise RuntimeError(f'the renderer did not follow the display turning {to}')
        game.ctl('wait', 5)
    game.settle()
    return before, status(game)


def session(name, bin_dir, out, env_base, args, flips):
    """A game that starts with the display at `flips[0]` and follows the rest, the scenes shot in each state as
    <name>.<n>.<scene>.png. The (before, after) counts of each flip."""
    folder = RUN / 'games' / name
    shutil.rmtree(folder, ignore_errors=True)
    folder.mkdir(parents=True)
    (folder / 'horizon-graphics.txt').write_text(identity.GRAPHICS + 'hdr:auto\n')
    (folder / 'options.txt').write_text('chatVisibility:2\ntutorialStep:none\npauseOnLostFocus:false\n')
    display = folder / 'display.txt'
    display.write_text(flips[0])
    env = lab.game_env(env_base, HORIZON_STILL='1', HORIZON_HDR_DISPLAY=display, **ENV)
    game = identity.Game(bin_dir, folder, env, args)
    counts = []
    try:
        game.launch()
        identity.prepare(game)
        for n, to in enumerate(flips):
            if n:
                counts.append(flip(game, display, to))
            for scene in SCENES:
                identity.shoot_named(game, out, f'{name}.{n}', scene)
        (out / f'{name}.log').write_text(str(game.ctl('logs', '--lines', '4000', check=False)), encoding='utf-8')
    finally:
        game.quit()
    return counts


def same(out, a, b, suffix):
    problem = identity.compare(out / f'{a}{suffix}', out / f'{b}{suffix}', out / f'diff-{a}{suffix}')
    return [] if problem is None else [f'{a}{suffix} against {b}{suffix}: {problem}']


def checks(out, counts):
    problems = []
    for n, on in enumerate((True, False, True)):
        for scene in SCENES:
            if not (out / f'flip.{n}.{scene}.png').is_file():
                problems.append(f'flip.{n}.{scene}: screenshot missing')
            if (out / f'flip.{n}.{scene}-hdr.png').exists() != on:
                problems.append(f'flip.{n}.{scene}: HDR output was {"off" if on else "on"}')
    for to, ((meshed, resident), (meshed_after, resident_after)) in zip(('off', 'on'), counts):
        if meshed_after != meshed:
            problems.append(f'the display turning {to} meshed {meshed_after - meshed} sections again')
        if resident_after != resident:
            problems.append(f'the display turning {to}: {resident_after} sections resident, {resident} before')
    for scene in SCENES:
        if problems:
            break
        problems += same(out, f'flip.2.{scene}', f'flip.0.{scene}', '.png')
        problems += same(out, f'flip.2.{scene}', f'flip.0.{scene}', '-hdr.png')
        problems += same(out, f'flip.1.{scene}', f'off.0.{scene}', '.png')
    return problems


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--bin-dir', help='prebuilt horizon-client and horizon-ctl (default: build them)')
    parser.add_argument('--resources', default=str(lab.RESOURCES), help='the prepared resources the game reads')
    parser.add_argument('-j', '--jobs', type=int, help='cargo build jobs')
    args = parser.parse_args()
    bin_dir = Path(args.bin_dir).resolve() if args.bin_dir else identity.build(lab.ROOT, lab.cargo_target(), args.jobs)
    env = dict(os.environ)
    icd = identity.lavapipe()
    if icd:
        env.update(VK_DRIVER_FILES=str(icd), VK_ICD_FILENAMES=str(icd), WGPU_BACKEND='vulkan')
    out = RUN / 'shots'
    shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True)
    counts = session('flip', bin_dir, out, env, args, ('on', 'off', 'on'))
    session('off', bin_dir, out, env, args, ('off',))
    print(f'meshed and resident sections, before and after each flip: {counts}', flush=True)
    problems = checks(out, counts)
    lab.write_json(RUN / 'report.json', {'counts': counts, 'failures': problems})
    print('hdr auto flip:', 'PASS' if not problems else f'{len(problems)} FAILED')
    for problem in problems:
        print('  ', problem)
    sys.exit(1 if problems else 0)


if __name__ == '__main__':
    main()
