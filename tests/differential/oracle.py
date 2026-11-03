"""Differential oracle: runs the vendored Python parsers over a JSON-lines batch.

Run as `python3 -I oracle.py` with HH_PYCORPUS_SRC set (python -I ignores
PYTHONPATH, so the source root is added to sys.path here). Reads one JSON object
per stdin line and prints one JSON line per case:
{"ok": bool, "value": <normalized>, "error": str}. A result containing lone
surrogates cannot cross the pipe faithfully; it is flagged with "surrogate": true
and a null value.
"""

import base64
import json
import os
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, os.environ['HH_PYCORPUS_SRC'])
sys.setrecursionlimit(1000)

import re  # noqa: E402

from hermes_harness.broker.live_capability_broker import _decode_request  # noqa: E402
from hermes_harness.multiagent.model_edits import parse_edit_proposal  # noqa: E402
from hermes_harness.multiagent.ollama_discovery import parse_ollama_tags  # noqa: E402
from hermes_harness.multiagent.plan_io import parse_plan_json  # noqa: E402
from hermes_harness.multiagent.replacement_proposals import (  # noqa: E402
    parse_replacement_proposal,
)

SLUG = re.compile(r'[a-z0-9][a-z0-9-]{0,63}\Z')


def has_surrogate(value):
    if isinstance(value, str):
        return any('\ud800' <= ch <= '\udfff' for ch in value)
    if isinstance(value, list):
        return any(has_surrogate(v) for v in value)
    if isinstance(value, dict):
        return any(has_surrogate(k) or has_surrogate(v) for k, v in value.items())
    return False


def broker(case):
    return _decode_request(base64.b64decode(case['b64']))


def plan(case):
    p = parse_plan_json(case['text'])
    return {
        'id': p.id,
        'objective': p.objective,
        'primary_identity': p.primary_identity,
        'nodes': [
            {
                'id': n.id,
                'objective': n.objective,
                'owned_paths': list(n.owned_paths),
                'dependencies': list(n.dependencies),
                'role': n.role,
            }
            for n in p.nodes
        ],
    }


def replacement(case):
    rows = parse_replacement_proposal(case['text'], case['task_id'], tuple(case['owned']))
    return [
        {'path': r.path, 'expected_sha256': r.expected_sha256, 'content': r.content} for r in rows
    ]


def edit(case):
    p = parse_edit_proposal(case['text'], case['task_id'], tuple(case['owned']))
    return {
        'task_id': p.task_id,
        'files': [{'path': f.path, 'content': f.content, 'sha256': f.sha256} for f in p.files],
    }


def ollama(case):
    models = parse_ollama_tags(base64.b64decode(case['b64']))
    return [[m.name, m.size_bytes] for m in models]


def splitlines(case):
    return case['text'].splitlines(keepends=True)


def slug(case):
    return SLUG.fullmatch(case['text']) is not None


def splitz(case):
    # git/worktrees.py: stdout is .strip()ped, then `raw.split('\x00')` minus empties.
    raw = case['text']
    return [n for n in raw.strip().split('\x00') if n]


def splitz_nostrip(case):
    return [n for n in case['text'].split('\x00') if n]


PAIRS = {
    'broker': broker,
    'plan': plan,
    'replacement': replacement,
    'edit': edit,
    'ollama': ollama,
    'splitlines': splitlines,
    'slug': slug,
    'splitz': splitz,
}


def main():
    out = []
    for raw in sys.stdin.buffer.read().split(b'\n'):
        if not raw:
            continue
        case = json.loads(raw)
        fn = PAIRS[case['pair']]
        try:
            value = fn(case)
            result = {'ok': True, 'value': value, 'error': ''}
            if has_surrogate(value):
                result = {'ok': True, 'value': None, 'error': '', 'surrogate': True}
            if case['pair'] == 'splitz':
                result['alt'] = splitz_nostrip(case)
        except Exception as exc:  # noqa: BLE001 - any failure is a rejection
            result = {'ok': False, 'value': None, 'error': f'{type(exc).__name__}: {exc}'[:200]}
        out.append(json.dumps(result))
    sys.stdout.write('\n'.join(out) + '\n')


main()
