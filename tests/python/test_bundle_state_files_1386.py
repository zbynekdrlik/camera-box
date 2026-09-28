"""Issue 1386 -- the ONE declared :8899 bundle-state server file set.

The server tree is installed flat on every OBS box (/opt/camera-box on strih-lx and imag by
setup-strih.sh step 9 / setup-imag.sh step 28, C:\\ProgramData\\camera-box on the Windows boxes by
the runbook). Before issue 1386 each installer typed its own literal three-file list, so a new
sibling module had to be added in three places or a box got a half set -- and a box missing one
module serves no :8899 at all (the server exits on the ImportError).

This pins, with no network and no box:
  * scripts/lib/bundle-state-files.txt (the Windows runbook reads it) == the bash array
    BUNDLE_STATE_SERVER_FILES in scripts/lib/bundle-state-files.sh (the setup scripts iterate it),
    in the same order;
  * that list == the server's REAL local import closure (every scripts/<name>.py reached from
    bundle-state-server.py through module-level imports), so a module the server imports but the
    list omits -- or a listed file nothing imports -- fails here;
  * both setup scripts source the lib and iterate the array instead of a literal list.
"""
from __future__ import annotations

import ast
import pathlib
import subprocess
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
_LIB_SH = _SCRIPTS / "lib" / "bundle-state-files.sh"
_LIB_TXT = _SCRIPTS / "lib" / "bundle-state-files.txt"
_SERVER = "bundle-state-server.py"
# obs_phase2.py is shared with many tools and is installed ALONE on some boxes, so its
# function-level (lazy) imports are optional per-subcommand features; the server only uses its
# module-level `_conn` / `_rpc`, which need no local module. Every OTHER file of the tree must
# import its local siblings at module load, so the module-level walk below is complete for it.
_SHARED_WITH_LAZY_IMPORTS = {"obs_phase2.py"}


def _txt_list():
    names = []
    for line in _LIB_TXT.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            names.append(line)
    return names


def _sh_list():
    # Sourced under the setup scripts' own strict mode and an EMPTY PATH: the lib is pure data, so
    # sourcing it must neither need an external command nor trip `set -u`.
    script = f'set -euo pipefail\n. "{_LIB_SH}"\nprintf "%s\\n" "${{BUNDLE_STATE_SERVER_FILES[@]}}"\n'
    out = subprocess.run(["/bin/bash", "-c", script], env={"PATH": ""}, capture_output=True,
                         text=True, check=False)
    assert out.returncode == 0, out.stderr
    assert out.stderr == "", out.stderr
    return out.stdout.split()


def _import_nodes(tree, lazy):
    """Import nodes of *tree*: the module-level ones (inside if/try blocks too), plus the ones
    nested in functions when *lazy* is set."""
    stack = list(tree.body)
    while stack:
        node = stack.pop()
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)) and not lazy:
            continue
        if isinstance(node, (ast.Import, ast.ImportFrom)):
            yield node
        stack.extend(ast.iter_child_nodes(node))


def _local_imports(filename, lazy):
    tree = ast.parse((_SCRIPTS / filename).read_text(encoding="utf-8"))
    found = set()
    for node in _import_nodes(tree, lazy):
        if isinstance(node, ast.Import):
            mods = [a.name for a in node.names]
        elif node.level == 0 and node.module:
            mods = [node.module]
        else:
            mods = []
        for mod in mods:
            candidate = mod.split(".")[0] + ".py"
            if (_SCRIPTS / candidate).is_file():
                found.add(candidate)
    return found


def _server_import_closure():
    closure, todo = set(), [_SERVER]
    while todo:
        name = todo.pop()
        if name in closure:
            continue
        closure.add(name)
        todo.extend(_local_imports(name, lazy=False))
    return closure


def test_the_txt_twin_and_the_bash_array_are_the_same_list():
    txt = _txt_list()
    assert txt, "scripts/lib/bundle-state-files.txt lists no file"
    assert len(txt) == len(set(txt)), f"a duplicate entry: {txt}"
    assert _sh_list() == txt, "the .txt twin and BUNDLE_STATE_SERVER_FILES must list the same files"


def test_every_listed_file_exists_flat_in_scripts():
    for name in _txt_list():
        assert "/" not in name and "\\" not in name, f"{name}: the deployed tree is flat"
        assert (_SCRIPTS / name).is_file(), f"{name} is listed but scripts/{name} does not exist"


def test_the_list_is_exactly_the_servers_local_import_closure():
    closure = _server_import_closure()
    listed = set(_txt_list())
    assert closure - listed == set(), (
        f"the server imports {sorted(closure - listed)} but scripts/lib/bundle-state-files.txt omits "
        "them -- a box would get a half set and serve no :8899")
    assert listed - closure == set(), (
        f"{sorted(listed - closure)} are listed but nothing in the server tree imports them")


def test_the_tree_imports_its_siblings_at_module_load():
    # The closure above walks module-level imports only; a lazy local import in a tree file would
    # escape it. Only the shared obs_phase2.py (installed alone elsewhere) may import lazily.
    for name in _txt_list():
        if name in _SHARED_WITH_LAZY_IMPORTS:
            continue
        lazy_only = _local_imports(name, lazy=True) - _local_imports(name, lazy=False)
        assert lazy_only == set(), f"{name} imports {sorted(lazy_only)} lazily -- import at module load"


def test_the_closure_reaches_every_facet_module():
    closure = _server_import_closure()
    assert "bundle_state_gather.py" in closure and "obs_phase2.py" in closure
    split = {p.name for p in _SCRIPTS.glob("bundle_state_*.py")}
    assert split <= closure, f"{sorted(split - closure)} exist but the server never reaches them"


_SMOKE = "import sys; sys.path.insert(0, \"/opt/camera-box\"); import bundle_state_gather"


def _smoke_rc(tree_dir, cwd):
    # The setup scripts' own post-install check, pointed at *tree_dir* instead of /opt/camera-box.
    prog = _SMOKE.replace("/opt/camera-box", str(tree_dir))
    return subprocess.run([sys.executable, "-I", "-B", "-c", prog], cwd=cwd, capture_output=True,
                          text=True, check=False).returncode


def test_both_setup_scripts_smoke_import_the_installed_tree_before_enabling():
    for script, loop, enable in (
            ("setup-strih.sh", 'for _bss in "${BUNDLE_STATE_SERVER_FILES[@]}"; do',
             "systemctl --user enable strih-bundle-state-server.service"),
            ("setup-imag.sh", 'for f in "${BUNDLE_STATE_SERVER_FILES[@]}"; do',
             "systemctl --user enable imag-bundle-state-server.service")):
        text = (_SCRIPTS / script).read_text(encoding="utf-8")
        smoke = f"python3 -I -B -c '{_SMOKE}'"
        assert text.count(smoke) == 1, f"{script} must smoke-import the installed tree once"
        assert text.index(loop) < text.index(smoke) < text.index(enable), (
            f"{script}: install the tree, then smoke-import it, then enable the unit")


def test_the_smoke_import_catches_a_partial_tree_even_from_the_scripts_dir(tmp_path):
    # -I keeps the caller's cwd (e.g. a run from scripts/, where every module exists) off
    # sys.path, so only the installed tree can satisfy the import.
    full = tmp_path / "full"
    part = tmp_path / "part"
    for tree in (full, part):
        tree.mkdir()
        for name in _txt_list():
            if tree is part and name == "bundle_state_vban.py":
                continue
            (tree / name).write_bytes((_SCRIPTS / name).read_bytes())
    assert _smoke_rc(full, _SCRIPTS) == 0
    assert _smoke_rc(part, _SCRIPTS) != 0


def test_both_setup_scripts_iterate_the_declared_list():
    # obs_phase2.py is also installed on its own (to /usr/local/bin, for the scene seeders), so
    # only the files that exist solely for the server tree must never be named in setup code.
    tree_only = [n for n in _txt_list() if n not in _SHARED_WITH_LAZY_IMPORTS]
    for script, loop in (("setup-strih.sh", 'for _bss in "${BUNDLE_STATE_SERVER_FILES[@]}"; do'),
                         ("setup-imag.sh", 'for f in "${BUNDLE_STATE_SERVER_FILES[@]}"; do')):
        text = (_SCRIPTS / script).read_text(encoding="utf-8")
        assert "lib/bundle-state-files.sh\"" in text, f"{script} must source the declared list"
        assert text.count(loop) == 1, f"{script} must install the tree by iterating the list"
        code = [ln for ln in text.splitlines() if not ln.lstrip().startswith("#")]
        named = [(n, ln) for ln in code for n in tree_only if n in ln]
        assert named == [], f"{script} names a server-tree file in code instead of the list: {named}"
