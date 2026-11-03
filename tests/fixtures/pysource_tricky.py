"""Adversarial constructs for the pysource differential test.

def fake_in_module_docstring(): return {"doc_key": 1}
"""

import functools
import os

# def commented_out(): return {"comment_key": 1}
TEXT = "def in_string(): return {'string_key': 1}"
TRIPLE = """
def in_triple():
    return {"triple_key": "prompt.triple"}
"""
TRIPLE_SINGLE = '''def in_triple_single(): return 1'''


def plain(a, b=2, *args, c, d=4, **kwargs):
    return {"plain_key": a, "other": b}


async def coroutine(x, /, y, *, z):
    await x
    return {"co_key": y, 'single': z}


def decorated_outer():
    @functools.wraps(plain)
    @staticmethod
    def decorated_inner(p, q=lambda r, s: r + s):
        return {"inner_key": p}

    def second_inner():
        def third_inner(v):
            return {"third": v}, 1

        return third_inner

    return decorated_inner, second_inner


class Holder:
    attribute = {"class_attr_key": 1}

    def method(self, value):
        return {"method_key": value, "prompt.method": 1}

    @property
    def prop(self):
        return "session.prop"

    async def async_method(self):
        async with self as handle:
            return {"async_key": handle}

    class Nested:
        def deep(self):
            return {"deep_key": 1}


def fstrings(name, width):
    a = f"def inside_fstring(): {{'brace_key': 1}} {name}"
    b = f"{name!r:>{width}} prompt.fstr"
    c = f'{"nested_quote_key": 1}' if False else f"{ {'fkey': name}['fkey'] }"
    d = f"{{literal braces}} session.fliteral {name}"
    e = f"{name=} {name:{width}.{width}}"
    g = f"{'a' 'b'} {name + 'prompt.concat'}"
    h = rf"raw \d {name} prompt.rawf"
    i = f"""triple {name}
    def not_a_def(): pass
    {{"triple_f_key": {name}}}"""
    j = f"{ {'dict_in_f': 1}!s}"
    k = f"{name:%Y-%m-%d} {width:#x}"
    return a, b, c, d, e, g, h, i, j, k


def dict_shapes(x, y):
    mixed = {1: "a", "str_key": "b", b"bytes_key": "c", None: "d", (1, 2): "e", x: "f"}
    star = {**mixed, "after_star": 1, **{"in_star": 2}}
    comp = {k: v for k, v in zip("ab", "cd")}
    comp_str = {"comp_literal": v for v in range(3)}
    sets = {"set_member", "other_member"}
    nested = {"outer": {"inner": {"innermost": 1}}, "list": [{"in_list": 1}]}
    concat = {"con" "cat": 1, "implicit" + "plus": 2}
    fkey = {f"fkey_{x}": 1, f"constant_f": 2}
    unicode_key = {"caf\u00e9": 1, "naïve": 2, "日本": 3}
    escapes = {"tab\there": 1, "nl\nkey": 2, "q\"uote": 3, 'sq\'': 4, "\x41\101": 5}
    rawkey = {r"raw\nkey": 1, R"RAW\x": 2, b"b": 3, rb"rb\d": 4}
    cont = {"cont" \
            "inued": 1}
    dup = {"dup": 1, "dup": 2}
    return mixed, star, comp, comp_str, sets, nested, concat, fkey, unicode_key, escapes, rawkey, cont, dup


def lambdas_and_walrus(items):
    f = lambda a, b=1, *c, **d: {"lambda_key": a}
    g = lambda: (yield)
    if (n := len(items)) > 1:
        return {"walrus_key": n}
    return [m for x in items if (m := {"walrus_comp": x})]


def matcher(command):
    match command:
        case {"match_key": value, "other": [first, *rest]}:
            return {"case_body": value}
        case ["go", direction] | ["move", direction]:
            return direction
        case str() as text if text.startswith("session."):
            return "session.matched"
        case _:
            return None


def match_soft_keyword(match, case):
    match = {"match_var": 1}
    case = match["match_var"]
    print(match, case)
    return match


def strings_of_note():
    a = 'prompt.single'
    b = "session.double" "session.joined"
    c = """prompt.triple"""
    d = b"prompt.bytes"
    e = "prompt." + "split"
    f = ("prompt.paren"
         "session.paren")
    g = u"session.u_prefix"
    h = "x" if "prompt.cond" else "y"
    i = {"prompt.as_key": "session.as_value"}
    j = "prompt.%s" % "session.mod"
    return a, b, c, d, e, f, g, h, i, j


def backslashes():
    a = "line one \
continued prompt.cont"
    b = r"C:\path\n\def x(): return {'raw_key': 1}"
    c = "\\"
    d = r"\\"
    e = "def esc(): \"quoted\" \'x\'"
    f = '\u0070rompt.unicode_escape'
    g = "\N{LATIN SMALL LETTER A}ession.named"
    h = "\x70rompt.hex"
    i = "\160rompt.octal"
    return a, b, c, d, e, f, g, h, i


def line_continuations(a,
                       b,
                       c=1):
    total = a + \
        b + \
        c
    if total \
            and a:
        return {"cont_key": total}
    return \
        total


def semicolons(x): y = x; return {"semi_key": y}


def one_liner(): return 1


async def async_one_liner(): return {"one_liner_key": 1}


def uses_tabs():
	if True:
		return {"tab_key": 1}
	return 0


def ünïcödé_näme(ärg, 变量=1):
    ωmega = {"unicode_ident": ärg}
    return ωmega


def generics[T](value: T, *args: int) -> list[T]:
    return [value]


def annotated(a: "str" = "prompt.default", b: dict = {"default_key": 1}) -> {"ret_key": 1}:
    return {"annotated_key": a}


def comprehensions(data):
    s = {x for x in data}
    d = {x: 1 for x in data}
    n = {"n": [y for y in data if y], **{"star_in_dict": 1}}
    return s, d, n


def exceptions():
    try:
        return {"try_key": 1}
    except* ValueError:
        return {"except_star": 1}
    finally:
        return {"finally_key": 1}


def yields():
    yield {"yield_key": 1}
    x = yield
    return x


def returns_in_nested_lambda_and_class():
    class Local:
        def method(self):
            return {"local_method": 1}

        value = {"local_class_attr": 2}

    return Local


def global_and_nonlocal():
    value = 0

    def inner():
        nonlocal value
        value = {"nonlocal_key": 1}
        return value

    return inner


def type_alias_use():
    type Alias = dict[str, int]
    return {"alias_key": Alias}


@functools.cache
def decorated_top(a):
    return {"top_decorated": a}


@(lambda f: f)
def decorated_by_lambda():
    return {"lambda_decorated": 1}


def ellipsis_and_numbers():
    a = ...
    b = 1_000.5e-3j + 0x_ff + 0b1 + 0o7
    c = {1: 1.5, "num_key": 0xFF}
    d = 1if True else 2
    return a, b, c, d


def empty_strings():
    a = ""
    b = ''
    c = """"""
    d = {"": 1, '': 2}
    e = f""
    return a, b, c, d, e


def after_everything():
    return {"last_key": os.sep}


def py312_fstrings(x):
    a = f"{x["key"]} prompt.same_quote"
    b = f"{ {"k312": 1}["k312"] }"
    c = f"""{
        x
    }"""
    d = f"{x!r}" f"session.adjacent_{x}" "tail"
    e = f"{x:{'>'}{10}}"
    f = f"{'{'}" + f"{'}'}"
    return a, b, c, d, e, f


def case_inside_expression(case, match):
    case[0] = {"case_subscript": 1}
    match(case)
    return {"case_dict": case}
