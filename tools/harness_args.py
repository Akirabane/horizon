"""Scratch directories for the command-oracle harnesses.

The harnesses run `tools/test_command_exec_parity.py`, which reads `--work-dir`
with argparse: a value may be separate (`--work-dir PATH`) or one argument
(`--work-dir=PATH`), and the last one wins. A harness that appends its own
default after the caller's therefore replaces the directory they asked for.
Capture-preserving harnesses use a fresh automatic default on every run, so
earlier oracle output stays intact without blocking the next run.
"""
import sys
import tempfile
from pathlib import Path


def named_work_dir(argv) -> str | None:
    """The `--work-dir` the caller named, in either form (`--work-dir PATH` or `--work-dir=PATH`); the last
    one given wins, like argparse's."""
    chosen = None
    for index, argument in enumerate(argv[1:], start=1):
        if argument == '--work-dir' and index + 1 < len(argv):
            chosen = argv[index + 1]
        elif argument.startswith('--work-dir='):
            chosen = argument[len('--work-dir='):]
    return chosen


def ensure_work_dir(default: Path, argv=None) -> Path:
    """Append `--work-dir default` unless the caller named one, either way, and return the directory the
    run will use (argparse's: the last one given)."""
    argv = sys.argv if argv is None else argv
    chosen = named_work_dir(argv)
    if chosen is None:
        argv += ['--work-dir', str(default)]
        chosen = str(default)
    return Path(chosen)


def ensure_fresh_work_dir(default: Path, argv=None) -> Path:
    """Keep an explicit `--work-dir`; otherwise create a fresh sibling of `default` and retain it as evidence."""
    argv = sys.argv if argv is None else argv
    if named_work_dir(argv) is not None:
        return ensure_work_dir(default, argv)
    default.parent.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix=default.name + '-', dir=default.parent))
    print(f'Capture directory: {directory.resolve()}', file=sys.stderr)
    return ensure_work_dir(directory, argv)


def require_work_dir(argv=None) -> Path:
    """The `--work-dir` the caller named, in either form, or exit with the usage: a tool that writes into
    that directory has to be told where, not guess."""
    argv = sys.argv if argv is None else argv
    chosen = named_work_dir(argv)
    if chosen is None:
        script = Path(argv[0]).resolve()
        root = Path(__file__).resolve().parents[1]
        shown = script.relative_to(root) if script.is_relative_to(root) else script.name
        sys.exit(f'Usage: python3 {shown} --work-dir DIR\n'
                 'DIR must not exist yet: the run writes its capture there.')
    return Path(chosen)
