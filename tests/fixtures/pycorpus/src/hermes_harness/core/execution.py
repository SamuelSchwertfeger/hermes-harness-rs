"""Process-group watchdog for owned, foreground child commands only.

Never assumes terminating a Hermes CLI terminates gateway-accepted work.
"""

import os
import signal
import subprocess
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Result:
    returncode: int
    stdout: str
    stderr: str
    timed_out: bool


def run_bounded(argv: list[str], *, cwd: Path, timeout: float = 30) -> Result:
    if not argv or timeout <= 0:
        msg = 'Expected a command and positive timeout'
        raise ValueError(msg)
    child = subprocess.Popen(  # noqa: S603
        argv,
        cwd=cwd,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
        env={'PATH': os.environ.get('PATH', '/usr/bin:/bin'), 'HOME': str(cwd)},
    )
    try:
        out, err = child.communicate(timeout=timeout)
        return Result(child.returncode, out, err, timed_out=False)
    except subprocess.TimeoutExpired:
        os.killpg(child.pid, signal.SIGTERM)
        try:
            out, err = child.communicate(timeout=2)
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGKILL)
            out, err = child.communicate()
        return Result(child.returncode, out, err, timed_out=True)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'Path',
    'Result',
    'dataclass',
    'os',
    'run_bounded',
    'signal',
    'subprocess',
]
