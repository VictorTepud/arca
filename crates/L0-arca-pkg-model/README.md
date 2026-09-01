# arca-pkg-model

El modelo del paquete `.arca`: parseo/validación de `manifest.toml`, tipos de
artefactos y layout interno. Único lugar donde "un paquete" significa algo
(docs/06 es su ley).

- Capa: L0 (`L0-arca-pkg-model/`)
- Spec: `specs/arca-02-pkg-model.md` (blueprint) · docs: `docs/06-formato-paquete.md`
- Tarea: T04 · Estado: **implementado** (2026-02)
- unsafe: **no** · Lints: `#![deny(missing_docs)]`, `clippy::unwrap_used`/`expect_used` denegados fuera de tests

## API pública (resumen)

| Ítem | Qué hace |
|---|---|
| `Manifest::parse(&[u8]) → Res<Manifest>` | TOML estricto + validaciones. **Total**: cualquier input → `ArcaError`, jamás pánico. BOM UTF-8 aceptado; > 64 KiB rechazado. |
| `Manifest::parse_detailed(&[u8])` | Ídem pero con el error fino `PkgError` (campo/valor/reason). |
| `Manifest::validate_layout(&ArchiveEntries) → Res<()>` | Cruza manifest vs listing del 7z: symlinks, paths peligrosos, extras, faltantes, duplicados, binarios no declarados bajo `bin/`. |
| `Manifest::backend_for(HostVariant) → Res<&Artifact>` | Elección ADR-001/003: Libre = native-default con fallback; Moderno = solo wasm; falla solo si NINGUNO aplica. |
| `Manifest::requested_caps() → &[Capability]` | Capabilities pedidas (forma punteada `net.client` en el manifest). |
| `Manifest::to_toml() → Res<String>` | Re-serialización (tools-pk, roundtrip). |
| `RelPath` | Newtype de path relativo saneado: sin `..`, sin abs, sin `\`, sin `:`, sin controles/NUL, profundidad ≤ 16, componente ≤ 255 B, total ≤ 1024 B. Primera barrera anti path-traversal. |
| `ArchiveEntries`/`ArchiveEntry`/`EntryKind` | Listing del `.arca` que `arca-7z` construye y `validate_layout` consume. |
| `HostVariant { Libre, Moderno }` | Variante del host (ADR-003). |
| `PkgError` | Diagnóstico fino por clase (27 variantes) con conversión a `ArcaError` de clase estática. |
| `LAYOUT`, `MAX_MANIFEST_BYTES`, `MAX_API_LEVEL`, `HOST_VERSION` | Constantes del contrato (spec 02 §3). |

## Correr los tests

```sh
cargo test -p arca-pkg-model          # 64 tests (unit + integración)
cargo clippy -p arca-pkg-model --all-targets -- -D warnings
cargo fmt -p arca-pkg-model
```

Fixtures: `tests/fixtures/` (golden completo, golden+BOM, 2 mínimos válidos)
y `tests/fixtures/malformed/` (60 manifests malformados, uno por clase de
error — `gen_malformed.py` los regenera de forma determinista).

Fuzz-lite: `tests/fuzz_lite.rs` — 1000 mutaciones del golden con semilla
fija (xorshift64* propio) + 2000 strings aleatorios por `RelPath`, sin
pánicos (invariante "parse total").

## Dependencias

`arca-types` (path), `serde`, `semver`, `toml`, `thiserror` (todas
`workspace = true`). La spec 02 §2 no lista `hex`: el codec mínimo
(64 hex ⇄ `[u8;32]`) es privado (`src/hex.rs`).
