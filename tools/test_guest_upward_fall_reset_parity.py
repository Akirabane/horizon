#!/usr/bin/env python3
"""One real guest's accepted airborne upward move, with independent phase state.

The local official-codec client adapter adds only a versioned fallmove input.
Raw packets, saved files, audit values and absolute clocks are never normalized.
"""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

import anvil_dump as nbt
import net_capture
import test_locator_bar_parity as net
from test_disconnect_crafting_parity import generate, offline, uuid_words, wait_new
from test_machine_recipe_placement_parity import Commands
from test_dedicated_server_parity import game_time

PLAYER = 'FallGuest'
IDS = ['fall_any', 'fall_small', 'fall_mid', 'fall_big']
PHASES = ['granted', 'reset', 'down', 'up', 'revoked', 'down_later', 'land', 'no_fall']


def read_dat(path):
    raw = path.read_bytes()
    if raw[:2] == b'\x1f\x8b':
        raw = gzip.decompress(raw)
    f = io.BytesIO(raw)
    kind = f.read(1)[0]
    name = f.read(int.from_bytes(f.read(2), 'big'))
    body = nbt.read_tag(f, kind)
    assert f.tell() == len(raw)
    return {'root_type': kind, 'root_name': name.hex(), 'body': body}


def clients(build, adapter):
    lock, jars, libraries = net.base.verify_artifacts()
    cp = os.pathsep.join(map(str, [jars['server'], *libraries]))
    classes = build / 'classes'
    if not classes.exists():
        classes.mkdir()
        subprocess.run(['javac', '--release', str(lock['minecraft']['java_major']), '-proc:none',
                        '-nowarn', '-sourcepath', str(net.lab.ROOT / 'mods/horizon/src/test/java'),
                        '-cp', cp, '-d', str(classes), str(adapter)], check=True)
    return (['java', '-XX:ActiveProcessorCount=2', '-Xmx1g', '-Duser.timezone=UTC', '-cp', cp,
             'net.minecraft.server.Main', 'nogui'],
            ['java', '-Xmx512m', '-cp', str(classes) + os.pathsep + cp,
             'dev.horizonlab.parity.LocatorBarClient'])


def pack(world):
    root = world / 'datapacks/fall-reset'
    root.mkdir(parents=True)
    (root / 'pack.mcmeta').write_text(json.dumps({'pack': {'description': 'fall reset proof',
        'pack_format': 121, 'min_format': [121, 0], 'max_format': [121, 0]}}))
    for name, distance in zip(IDS, [None, {'max': 1.0}, {'min': 4.9, 'max': 5.1}, {'min': 8.0}]):
        criterion = {'trigger': 'minecraft:fall_from_height'}
        if distance:
            criterion['conditions'] = {'distance': {'y': distance}}
        p = root / 'data/fallproof/advancement' / (name + '.json')
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(json.dumps({'criteria': {name: criterion}}))


def run(side, command, client_command, build):
    directory = build / side
    assert not directory.exists(), 'new output only'
    port = net.server_dir(directory, build / 'generate/world')
    server = net.Server(command, directory, side)
    c = Commands(server, build / (side + '-commands.json'))
    client, stop = None, None
    log = (build / (side + '-client.log')).open('w')
    rows = {}
    try:
        server.wait_for(r'Done \(')
        c.reply('tick freeze')
        c.reply('tick rate 1')
        listen = net.free_port()
        stop = net_capture.start_background(listen, f'127.0.0.1:{port}', build / (side + '-packets.jsonl'), full=True)
        client = net.Client(client_command, listen, build / (side + '-wire.txt'), log)
        for option in ['chat on', 'flat-chat on', 'state on', 'players on', 'advancements on', 'list-syncs on']:
            client.tell(option)
        client.tell('join ' + PLAYER)
        client.sync([PLAYER])
        for text in ['tick freeze', 'gamerule show_advancement_messages false',
                     'gamerule log_admin_commands false', f'gamemode survival {PLAYER}',
                     f'tp {PLAYER} 12.5 110 12.5 0 0']:
            c.reply(text)
        client.sync([PLAYER])

        def revoke():
            for name in IDS:
                c.reply(f'advancement revoke {PLAYER} only fallproof:{name}')

        def snapshot(phase):
            client.tell('step ' + phase)
            client.sync([PLAYER])
            client.tell(f'adv-snapshot {PLAYER} {phase} ' + ','.join('fallproof:' + n for n in IDS))
            ack = f'adv-snapshotted {phase} {PLAYER}\n'
            while (line := client.process.stdout.readline()) != ack:
                assert line
                log.write(line)
            c.reply(f'data modify storage fallproof:proof {phase}.player set from entity {PLAYER}')
            c.reply(f'execute store result storage fallproof:proof {phase}.clock long 1 run time query gametime')
            for label, condition in [('floor', 'if block 12 99 12 stone'),
                                     ('load', 'if loaded 12 110 12')]:
                c.reply(f'execute store success storage fallproof:proof {phase}.{label} byte 1 run execute {condition}')
            start = len(server.lines)
            c.reply('save-all flush')
            model = [json.loads(l.split('advancement selector model raw: ', 1)[1])
                     for l in server.lines[start:] if 'advancement selector model raw: ' in l]
            out = build / (side + '-' + phase + '-raw')
            out.mkdir()
            files = {}
            for p in (directory / 'world').rglob('*'):
                if p.is_file() and (p.name == offline(PLAYER) + '.json' or p.name == 'command_storage.dat' and p.parent.name == 'fallproof' or p.name == 'level.dat'):
                    rel = p.relative_to(directory / 'world')
                    dest = out / rel
                    dest.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(p, dest)
                    files[str(rel)] = {'sha256': hashlib.sha256(p.read_bytes()).hexdigest(),
                                      'value': json.loads(p.read_text()) if p.suffix == '.json' else read_dat(p)}
            rows[phase] = {'models': model, 'files': files}
            (build / (side + '-phases.json')).write_text(json.dumps(rows, indent=2) + '\n')

        def move(label, x, y, ground):
            client.tell('step input_' + label)
            client.tell(f'fallmove {PLAYER} {x} {y} 12.5 {str(ground).lower()}')
            client.sync([PLAYER])
            # Player ticks continue under freeze; preserve both sync and actual
            # one-world-step receipt, without pretending the intervening ticks stop.
            before = game_time(server)
            c.reply('tick step 1')
            deadline = time.monotonic() + 30
            while game_time(server) < before + 1:
                assert time.monotonic() < deadline, 'actual world step did not settle'
                time.sleep(.02)
            assert game_time(server) == before + 1
            client.sync([PLAYER])
            snapshot(label)

        for name in IDS:
            c.reply(f'advancement grant {PLAYER} only fallproof:{name}')
        before = game_time(server)
        c.reply('tick step 1')
        deadline = time.monotonic() + 30
        while game_time(server) < before + 1:
            assert time.monotonic() < deadline, 'declared observation step did not settle'
            time.sleep(.02)
        assert game_time(server) == before + 1
        snapshot('granted')
        revoke()
        before = game_time(server)
        c.reply('tick step 1')
        deadline = time.monotonic() + 30
        while game_time(server) < before + 1:
            assert time.monotonic() < deadline, 'declared observation step did not settle'
            time.sleep(.02)
        assert game_time(server) == before + 1
        snapshot('reset')
        move('down', 12.5, 109, False)
        move('up', 12.5, 109.5, False)
        revoke()
        snapshot('revoked')
        move('down_later', 12.5, 105, False)
        move('land', 12.5, 100, True)
        revoke()
        move('no_fall', 13.5, 100, True)
        client.end()
        client = None
    finally:
        if client:
            try:
                client.end()
            finally:
                client.kill()
        try:
            server.stop()
        finally:
            if stop:
                stop.set()
            log.close()
    # Disconnect files are a separate cross-check, never phase replacements.
    disconnect = build / (side + '-disconnect')
    disconnect.mkdir()
    for p in (directory / 'world').rglob(offline(PLAYER) + '.json'):
        shutil.copy2(p, disconnect / p.name)
    return rows


def project(side, build):
    rows = json.loads((build / (side + '-phases.json')).read_text())
    facts, diagnostics = {}, {}
    expected_y = dict(zip(PHASES, [110, 110, 109, 109.5, 109.5, 105, 100, 100]))
    controls = {}
    for phase, row in rows.items():
        files = row['files']
        storage = [v['value']['body']['data']['contents']['proof'] for k,v in files.items() if k.endswith('fallproof/command_storage.dat')]
        assert len(storage) == 1, (phase, list(files))
        saved = storage[0][phase]
        body = saved['player']
        controls[phase + '/uuid'] = body['UUID'] == ['I', uuid_words(PLAYER)]
        controls[phase + '/mode'] = body['playerGameType'] == 0
        controls[phase + '/position'] = body['Pos'] == [['d', 13.5 if phase == 'no_fall' else 12.5], ['d', expected_y[phase]], ['d', 12.5]]
        controls[phase + '/floor'] = saved['floor'] == ['b', 1]
        controls[phase + '/load'] = saved['load'] == ['b', 1]
        if phase in ['reset', 'down', 'up', 'revoked', 'down_later']:
            controls[phase + '/health'] = body['Health'] == ['f', 20.0]
        if phase == 'down':
            controls['prior_positive_physical'] = body['fall_distance'][1] > 0
        advance = [v['value'] for k,v in files.items() if '/advancements/' in k and k.endswith(offline(PLAYER) + '.json')]
        model = row['models']
        if side == 'java':
            assert len(advance) == 1, (phase, 'Java actual phase progress file missing')
            authority = advance[0]
        else:
            assert len(model) == 1 and model[0]['name'] == PLAYER and model[0]['uuid'] == offline(PLAYER), (phase,model)
            assert type(model[0]['conn']) is int and model[0]['conn'] >= 0
            authority = model[0]['progress']
        progress = {name: bool(authority.get('fallproof:' + name, {}).get('done', False)) for name in IDS}
        if phase in ['reset', 'down', 'revoked', 'down_later', 'no_fall']:
            controls[phase + '/fresh_progress'] = not any(progress.values())
        if phase == 'granted':
            controls['all_definitions_positive_before_reset'] = all(progress.values())
        facts[phase + '/progress'] = progress
        for key in ['UUID', 'playerGameType', 'Health', 'Pos', 'OnGround', 'fall_distance']:
            facts[phase + '/' + key] = body.get(key)
        diagnostics[phase] = {'absolute_clock': saved['clock'], 'models':model, 'phase_files':list(files), 'file_progress':advance}
    assert all(controls.values()), {k:v for k,v in controls.items() if not v}
    expected_up = dict(zip(IDS, [True, True, False, False]))
    gates = {'up_physical_reset':facts['up/fall_distance'] == ['d',0.0],
             'up_awards_actual_recipient':facts['up/progress'] == expected_up}
    if side == 'java':
        assert all(gates.values()), gates
        assert facts['land/progress'] == dict(zip(IDS, [True,False,True,False]))
    report = {'admitted':True,'controls':controls,'gates':gates,'facts':facts,'diagnostics':diagnostics}
    (build / (side + '-report.json')).write_text(json.dumps(report,indent=2)+'\n')
    return report


def main():
    a = argparse.ArgumentParser()
    a.add_argument('--side', choices=['java', 'native'], required=True)
    a.add_argument('--build', type=Path, required=True)
    a.add_argument('--adapter', type=Path, required=True)
    a.add_argument('--binary', type=Path)
    a = a.parse_args()
    a.build.mkdir(parents=True, exist_ok=True)
    net.PLAYERS = [PLAYER]
    net.PROPERTIES.update({'force-gamemode': 'false'})
    java, client = clients(a.build, a.adapter)
    if a.side == 'java':
        world = generate(java, a.build)
        pack(world)
        command = java
    else:
        command = [str(a.binary.resolve()), '--resources', str(net.lab.LOCAL / 'horizon-client/26.3'), '--dedicated', '--nogui']
        os.environ['HORIZON_DEV_ADVANCEMENT_SELECTOR_AUDIT'] = '1'
    rows = run(a.side, command, client, a.build)
    report = project(a.side, a.build)
    print(a.side, 'ADMITTED', len(report['controls']), report['gates'])


if __name__ == '__main__':
    main()
