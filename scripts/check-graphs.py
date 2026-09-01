#!/usr/bin/env python3
"""check-graphs (T01): valida el grafo de crates de Arca.

ERRORES (exit != 0): ciclo en dependencias; violación de capa (dep de capa
superior); dependencia Arca no listada en la tabla maestra docs/08 §3
(se permite ampliar tabla con update del doc — specs ↔ código sin divergencia).
AVISO: dep fuera de tabla pero de capa inferior o igual (revisar spec).

Genera graphs/MASTER.autogen.dot para inspección visual.
"""
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

LAYER = {
    "arca-types": 0, "arca-pkg-model": 0, "arca-protocol": 0,
    "arca-gfx-protocol": 0, "arca-shm": 0, "arca-ipc": 0,
    "arca-permissions": 0, "arca-sign": 0, "arca-7z": 0, "arca-log": 0,
    "arca-store": 1, "arca-installer": 1, "arca-exec-abi": 1,
    "arca-exec-native": 1, "arca-exec-wasm": 1, "arca-gfx-host": 1,
    "arca-wm": 1, "arca-input": 1, "arca-host-core": 1,
    "arca-android-glue": 1, "arca-svc-broker": 1,
    "arca-rt": 2, "arca-sdk": 2, "arca-sdk-ui": 2,
    "arca-tools-pk": 3, "arca-tools-dev": 3, "arca-bench": 3,
    "devapp-hello": 3, "devapp-demo": 3, "devapp-keyboard": 3,
    "devapp-crashy": 3, "devapp-net": 3, "arca-home": 3,
}

# Tabla maestra docs/08 §3 (deps Arca permitidas por crate)
ALLOWED = {
    "arca-types": [],
    "arca-pkg-model": ["arca-types"],
    "arca-protocol": ["arca-types"],
    "arca-gfx-protocol": ["arca-types"],
    "arca-shm": ["arca-types"],
    "arca-ipc": ["arca-types", "arca-protocol"],
    "arca-permissions": ["arca-types", "arca-pkg-model"],
    "arca-sign": ["arca-types"],
    "arca-7z": ["arca-types"],
    "arca-log": ["arca-types"],
    "arca-store": ["arca-types", "arca-pkg-model"],
    "arca-installer": ["arca-types", "arca-pkg-model", "arca-7z", "arca-sign", "arca-store"],
    "arca-exec-abi": ["arca-types", "arca-protocol", "arca-pkg-model"],  # spec 13 §2 lo lista; tabla alineada (F2)
    "arca-exec-native": ["arca-exec-abi", "arca-permissions", "arca-log", "arca-types", "arca-protocol", "arca-ipc", "arca-shm"],
    "arca-exec-wasm": ["arca-exec-abi", "arca-types", "arca-protocol", "arca-log", "arca-rt"],
    "arca-gfx-host": ["arca-types", "arca-gfx-protocol", "arca-shm"],
    "arca-wm": ["arca-types", "arca-protocol"],
    "arca-input": ["arca-types", "arca-protocol", "arca-gfx-protocol", "arca-wm"],
    "arca-host-core": ["arca-types", "arca-protocol", "arca-shm", "arca-ipc", "arca-log",
                        "arca-store", "arca-installer", "arca-exec-abi", "arca-wm",
                        "arca-gfx-host", "arca-input", "arca-svc-broker"],
    "arca-android-glue": ["arca-types", "arca-host-core", "arca-gfx-host", "arca-wm", "arca-input"],
    "arca-svc-broker": ["arca-types", "arca-permissions", "arca-host-core", "arca-log", "arca-store"],
    "arca-rt": ["arca-types", "arca-protocol", "arca-gfx-protocol", "arca-ipc", "arca-shm", "arca-log"],
    "arca-sdk": ["arca-types", "arca-rt", "arca-pkg-model", "arca-protocol"],
    "arca-sdk-ui": ["arca-types", "arca-sdk", "arca-gfx-protocol", "arca-protocol"],
    "arca-tools-pk": ["arca-types", "arca-pkg-model", "arca-7z", "arca-sign", "arca-log"],
    "arca-tools-dev": ["arca-types", "arca-pkg-model", "arca-store", "arca-installer", "arca-sign", "arca-7z", "arca-log"],
    "arca-bench": ["arca-types", "arca-ipc", "arca-shm", "arca-gfx-host", "arca-exec-abi", "arca-protocol"],
    "devapp-hello": [], "devapp-demo": ["arca-gfx-protocol", "arca-shm", "arca-sdk-ui"],
    "devapp-keyboard": [],
    "devapp-crashy": [], "devapp-net": [], "arca-home": [],
}

def crate_dirs():
    for d in sorted(ROOT.glob("crates/*/*/")) + sorted(ROOT.glob("crates/*/")):
        if (d / "Cargo.toml").is_file():
            yield d

def main():
    errors, warnings, edges = [], [], []
    for d in crate_dirs():
        name = d.name.split("-", 1)[-1] if d.parent.name == "L3-devapps" else d.name
        # nombre de crate real desde Cargo.toml
        with open(d / "Cargo.toml", "rb") as f:
            meta = tomllib.load(f)
        name = meta["package"]["name"]
        deps = set(meta.get("dependencies", {}).keys())
        arca_deps = sorted(x for x in deps if x.startswith("arca-"))
        if name not in LAYER:
            errors.append(f"{name}: crate fuera de la tabla de capas")
            continue
        allowed = ALLOWED.get(name)
        for dep in arca_deps:
            edges.append((name, dep))
            if dep not in LAYER:
                errors.append(f"{name} → {dep}: dep Arca desconocida")
                continue
            if LAYER[dep] > LAYER[name]:
                errors.append(f"{name}(L{LAYER[name]}) → {dep}(L{LAYER[dep]}): violación de capa")
            elif allowed is not None and dep not in allowed:
                warnings.append(f"{name} → {dep}: no está en la tabla maestra docs/08 §3")
    # ciclo (DFS)
    WHITE, GREY, BLACK = 0, 1, 2
    adj = {}
    for a, b in edges:
        adj.setdefault(a, set()).add(b)
    color = {n: WHITE for n in adj}
    stack = []
    def dfs(n):
        color[n] = GREY
        stack.append(n)
        for m in sorted(adj.get(n, ())):
            if color.get(m, WHITE) == GREY:
                i = stack.index(m)
                errors.append("CICLO: " + " → ".join(stack[i:] + [m]))
            elif color.get(m, WHITE) == WHITE:
                dfs(m)
        stack.pop()
        color[n] = BLACK
    for n in sorted(adj):
        if color[n] == WHITE:
            dfs(n)
    # dot
    out = ROOT / "graphs"
    out.mkdir(exist_ok=True)
    with open(out / "MASTER.autogen.dot", "w") as f:
        f.write("digraph arca {\n  rankdir=LR;\n")
        for a, b in sorted(edges):
            f.write(f'  "{a}" -> "{b}";\n')
        f.write("}\n")
    for w in warnings:
        print("WARN :", w)
    if errors:
        for e in errors:
            print("ERROR:", e)
        sys.exit(1)
    n_crates = len(list(crate_dirs()))
    print(f"check-graphs OK: {len(edges)} aristas, {n_crates} crates, "
          f"{len(warnings)} avisos → graphs/MASTER.autogen.dot")

if __name__ == "__main__":
    main()
