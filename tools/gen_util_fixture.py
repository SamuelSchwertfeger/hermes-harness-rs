"""Write tests/fixtures/util_parity.json: inputs and Python's exact json.dumps/sha256 outputs.

Usage: python3 -I tools/gen_util_fixture.py tests/fixtures/util_parity.json
"""

import hashlib
import json
import sys
from pathlib import Path

CASES = [
    {'b': 1, 'a': ['é', '\U0001f600'], 'c': {'z': None, 'y': True}},
    {'del': '\x7f', 'ctl': '\x00\x01\x1f\b\f\n\r\t', 'q': '"\\/', 'z': 'Z', 'é': 1, '￿': 2},
    [1.0, 1e-07, 2.5e-05, 1e16, 1e20, 1e-4, 0.0001234, 123456789012345.6, -0.0, 0.1, 1.5, 100.0],
    {'nested': [[], {}, [{}], {'k': []}], 'n': -5, 'big': 9007199254740993},
    [
        1e16,
        1e-7,
        0.1,
        -0.0,
        1.0,
        5e-324,
        1.7976931348623157e308,
        123456789012345680.0,
        1e15,
        0.0001,
        0.00001,
    ],
    {'o': {'a': [1, {'b': []}], 'c': {}}, 'f': [[1.5e-10, [2e22]], {}], 'u': 'é\x7f\U0001f600\x00'},
    [[[]], [{}], {'a': {'b': {'c': [1, 2.5]}}}],
    'plain string',
    [],
    {},
]


def main() -> None:
    out = []
    for case in CASES:
        compact = json.dumps(case, sort_keys=True, separators=(',', ':'))
        out.append({
            'input': json.dumps(case),
            'compact': compact,
            'spaced': json.dumps(case, sort_keys=True),
            'indent2': json.dumps(case, sort_keys=True, indent=2),
            'digest': hashlib.sha256(compact.encode()).hexdigest(),
            'digest_line': hashlib.sha256((compact + '\n').encode()).hexdigest(),
        })
    Path(sys.argv[1]).write_text(json.dumps(out, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
