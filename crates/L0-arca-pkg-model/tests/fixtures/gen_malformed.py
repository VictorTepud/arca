#!/usr/bin/env python3
"""Genera los fixtures malformados de T04 a partir del golden.

Cada fixture introduce EXACTAMENTE una mutación → una clase de error
distinta (ver tests/manifest_parse.rs). Regenerable: determinista.
"""
import os

BASE = "/home/z/my-project/arca/crates/L0-arca-pkg-model/tests/fixtures"
MAL = os.path.join(BASE, "malformed")
os.makedirs(MAL, exist_ok=True)

with open(os.path.join(BASE, "golden_manifest.toml"), "rb") as f:
    G = f.read().decode("utf-8")

# BOM fixture (binario: EF BB BF + golden)
with open(os.path.join(BASE, "golden_bom.toml"), "wb") as f:
    f.write(b"\xef\xbb\xbf" + G.encode("utf-8"))


def w(name, text, note):
    with open(os.path.join(MAL, name), "w", encoding="utf-8") as f:
        f.write(f"# MALFORMED ({name}): {note}\n" + text)


def rm_line(g, needle):
    lines = [l for l in g.splitlines() if not l.lstrip().startswith(needle)]
    return "\n".join(lines) + "\n"


def rm_section(g, header):
    lines = g.splitlines()
    out, skipping = [], False
    for l in lines:
        if l.strip() == header:
            skipping = True
            continue
        if skipping and l.strip().startswith("["):
            skipping = False
        if not skipping:
            out.append(l)
    return "\n".join(out) + "\n"


def rep(g, old, new):
    assert old in g, f"no encontrado: {old!r}"
    return g.replace(old, new, 1)


def after_line(g, needle, extra):
    lines = g.splitlines()
    out = []
    for l in lines:
        out.append(l)
        if l.lstrip().startswith(needle):
            out.append(extra)
    return "\n".join(out) + "\n"


ID = 'id            = "dev.misapps.teclado"'
NAME = 'name          = "Mi Teclado Pro"'
VER = 'version       = "1.2.0"'
MH = 'min_host      = "1.0.0"'
API = "api_level     = 1"
TAGS = 'tags          = ["tools"]'
BP = 'backend_pref  = "any"'
ENTRY = 'entry         = "app"'
RESP = 'respawn       = "on-crash"'
NPATH = 'path     = "bin/native-aarch64/app"'
WPATH = 'path     = "bin/wasm/app.wasm"'
AOT = 'aot      = "bin/wasm/app.aot"'
WSHA = 'sha256   = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"'
NSHA = 'sha256   = "abababababababababababababababababababababababababababababababab"'
WR = 'wasm_runtime = "wamr-aot"'
UI = 'ui            = { sync = false, windows = "single", atlas = 2048, fonts = ["assets/fonts/inter.ttf"] }'
BUDGET = "launch_budget_ms = 120"
FRAME = "max_frame_mb     = 2"
PERMS = 'perms         = ["net.client", "clipboard.write"]'
AUTHORS = 'authors       = ["tú <tu@correo>"]'

# ---- secciones / campos ausentes ----
w("m01_empty.toml", "", "vacío → MissingSection(package)")
w("m02_no_package_section.toml", rm_section(G, "[package]"), "sin [package] → MissingSection")
w("m03_pkg_missing_id.toml", rm_line(G, "id "), "sin package.id → MissingField")
w("m07_pkg_missing_name.toml", rm_line(G, "name "), "sin package.name → MissingField")
w("m10_pkg_missing_version.toml", rm_line(G, "version "), "sin package.version → MissingField")
w("m13_pkg_missing_min_host.toml", rm_line(G, "min_host "), "sin package.min_host → MissingField")
w("m15_pkg_missing_api_level.toml", rm_line(G, "api_level "), "sin package.api_level → MissingField")
w("m19_missing_runtime.toml", rm_section(G, "[runtime]"), "sin [runtime] → MissingSection")
w("m23_missing_profile.toml", rm_section(G, "[profile]"), "sin [profile] → MissingSection")
w("m26_no_artifacts.toml", rm_section(rm_section(G, "[artifacts.native]"), "[artifacts.wasm]"), "sin artefactos → NoArtifacts")
w("m28_artifact_missing_path.toml", rm_line(G, WPATH), "sin artifacts.wasm.path → MissingField")
w("m33_artifact_missing_sha256.toml", rm_line(G, WSHA), "sin artifacts.wasm.sha256 → MissingField")

# ---- package: id / name / version / min_host / api_level ----
w("m04_id_uppercase.toml", rep(G, ID, 'id            = "Dev.Misapps.Teclado"'), "id con mayúsculas → BadAppId")
w("m05_id_too_short.toml", rep(G, ID, 'id            = "ab"'), "id de 2 chars → BadAppId")
w("m06_id_dash.toml", rep(G, ID, 'id            = "dev.misapps-teclado"'), "id con guión → BadAppId")
w("m08_name_empty.toml", rep(G, NAME, 'name          = ""'), "name vacío → BadName")
w("m09_name_decomposed.toml", rep(G, NAME, 'name          = "Cafe\\u0301 Teclado"'), "name NFD (marca combinante) → BadName")
w("m11_version_not_semver.toml", rep(G, VER, 'version       = "1.2"'), "version 1.2 → BadSemver")
w("m12_version_leading_zero.toml", rep(G, VER, 'version       = "01.2.0"'), "version con cero inicial → BadSemver")
w("m14_min_host_future.toml", rep(G, MH, 'min_host      = "9.0.0"'), "min_host 9.0.0 > host 1.0.0 → HostTooOld")
w("m16_api_level_zero.toml", rep(G, API, "api_level     = 0"), "api_level 0 → UnsupportedApiLevel")
w("m17_api_level_future.toml", rep(G, API, "api_level     = 2"), "api_level 2 (futuro) → UnsupportedApiLevel")
w("m18_api_level_string.toml", rep(G, API, 'api_level     = "1"'), "api_level string → TomlType")

# ---- runtime ----
w("m20_bad_backend_pref.toml", rep(G, BP, 'backend_pref  = "both"'), "backend_pref «both» → BadEnum")
w("m21_entry_empty.toml", rep(G, ENTRY, 'entry         = ""'), "entry vacío → BadEntry")
w("m22_bad_respawn.toml", rep(G, RESP, 'respawn       = "sometimes"'), "respawn «sometimes» → BadEnum")
w("m39_unknown_capability.toml", rep(G, PERMS, 'perms         = ["net.admin"]'), "capability desconocida → BadCapability")
w("m40_capability_kebab_style.toml", rep(G, PERMS, 'perms         = ["net-client"]'), "capability en kebab (no punteada) → BadCapability")
w("m41_ui_windows_invalid.toml", rep(G, UI, 'ui            = { sync = false, windows = "many", atlas = 2048, fonts = ["assets/fonts/inter.ttf"] }'), "windows «many» → BadEnum")
w("m42_ui_atlas_not_pow2.toml", rep(G, UI, 'ui            = { sync = false, windows = "single", atlas = 1000, fonts = ["assets/fonts/inter.ttf"] }'), "atlas 1000 no potencia de 2 → OutOfRange")
w("m43_font_path_dotdot.toml", rep(G, UI, 'ui            = { sync = false, windows = "single", atlas = 2048, fonts = ["../fonts/x.ttf"] }'), "font con .. → BadFont")
w("m44_font_outside_assets.toml", rep(G, UI, 'ui            = { sync = false, windows = "single", atlas = 2048, fonts = ["bin/inter.ttf"] }'), "font fuera de assets/ → BadFont")
w("m53_sync_string.toml", rep(G, UI, 'ui            = { sync = "false", windows = "single", atlas = 2048, fonts = ["assets/fonts/inter.ttf"] }'), "sync string → TomlType")
w("m54_perms_string_not_array.toml", rep(G, PERMS, 'perms         = "net.client"'), "perms string → TomlType")

# ---- artifacts ----
w("m27_artifact_bad_key.toml", rep(G, "[artifacts.native]", "[artifacts.java]"), "clave java → BadArtifact")
w("m29_artifact_path_absolute.toml", rep(G, NPATH, 'path     = "/bin/app"'), "path absoluto → BadPath")
w("m30_artifact_path_dotdot.toml", rep(G, NPATH, 'path     = "bin/../../etc/passwd"'), "path con .. → BadPath")
w("m31_artifact_path_outside_bin.toml", rep(G, NPATH, 'path     = "assets/app.wasm"'), "native bajo assets/ → BadArtifact")
w("m32_artifact_path_backslash.toml", rep(G, NPATH, 'path     = "bin\\\\wasm\\\\app.wasm"'), "path con backslashes → BadPath")
w("m34_sha256_not_hex.toml", rep(G, WSHA, 'sha256   = "%s"' % ("z" * 64)), "sha256 no hex → BadSha256")
w("m35_sha256_short.toml", rep(G, WSHA, 'sha256   = "%s"' % ("c" * 63)), "sha256 de 63 chars → BadSha256")
w("m36_wasm_wrong_suffix.toml", rep(G, WPATH, 'path     = "bin/wasm/app.bin"'), "wasm sin sufijo .wasm → BadArtifact")
w("m37_native_wrong_dir.toml", rep(G, NPATH, 'path     = "bin/native/app"'), "native fuera de bin/native-aarch64/ → BadArtifact")
w("m38_duplicate_artifact_path.toml", rep(G, WPATH, 'path     = "bin/native-aarch64/app"'), "wasm repite path de native → DuplicateArtifactPath")
w("m51_wasm_runtime_invalid.toml", rep(G, WR, 'wasm_runtime = "wasmer"'), "wasm_runtime «wasmer» → BadEnum")
w("m52_aot_wrong_location.toml", rep(G, AOT, 'aot      = "bin/wasm2/app.aot"'), "aot fuera de bin/wasm/ → BadArtifact")

# ---- profile ----
w("m24_budget_zero.toml", rep(G, BUDGET, "launch_budget_ms = 0"), "launch_budget_ms 0 → OutOfRange")
w("m25_frame_huge.toml", rep(G, FRAME, "max_frame_mb     = 99999"), "max_frame_mb 99999 → OutOfRange")

# ---- metadatos ----
w("m50_tag_uppercase.toml", rep(G, TAGS, 'tags          = ["Tools!"]'), "tag con mayúsculas → BadTag")
w("m56_authors_number.toml", rep(G, AUTHORS, "authors       = 5"), "authors número → TomlType")

# ---- tamaño / encoding / límites de metadatos ----
# m57: > 64 KiB (TooLarge) — se genera por longitud.
with open(os.path.join(MAL, "m57_oversize.toml"), "w", encoding="utf-8") as f:
    f.write("# MALFORMED (m57_oversize.toml): excede 64 KiB → TooLarge\n")
    f.write(G)
    f.write("\n" + ("# " + "x" * 63 + "\n") * 1000)  # ~65 KiB de relleno (1000 líneas)
# m58: bytes no UTF-8 (NotUtf8) — binario.
with open(os.path.join(MAL, "m58_not_utf8.toml"), "wb") as f:
    f.write(b"# MALFORMED (m58_not_utf8.toml): no es UTF-8 -> NotUtf8\n")
    f.write(b"[package]\nid = \"dev.x.y\"\n\xff\xfe\xff\n")
long_desc = "D" * 1200
w("m59_description_too_long.toml", rep(G, 'description   = "Un teclado estadístico"', 'description   = "%s"' % long_desc), "description > 1024 → BadDescription")
long_author = "A" * 200
w("m60_author_too_long.toml", rep(G, AUTHORS, 'authors       = ["%s"]' % long_author), "author > 128 → BadAuthor")

# ---- campos desconocidos (api_level futura) ----
w("m45_unknown_root_field.toml", G + "\n[backends]\nnative = true\n", "sección raíz desconocida → UnknownField")
w("m46_unknown_package_field.toml", after_line(G, "tags ", 'icon = "x.png"'), "campo desconocido en [package] → UnknownField")
w("m47_unknown_runtime_field.toml", after_line(G, "perms ", "priority = 5"), "campo desconocido en [runtime] → UnknownField")
w("m48_unknown_profile_field.toml", after_line(G, "max_frame_mb ", "extra = 1"), "campo desconocido en [profile] → UnknownField")

# ---- sintaxis TOML ----
w("m49_duplicate_key.toml", after_line(G, "id ", ID), "clave id duplicada → TomlSyntax")
w("m55_not_toml_at_all.toml", "<<<< not [ toml ] at >>>> all\n[[[\n", "basura no-TOML → TomlSyntax")

print("fixtures:", len(os.listdir(MAL)), "+ golden_bom")
