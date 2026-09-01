# decision.md — GO/NO-GO del backend nativo (probe F0, tarea T02)

> **Cómo rellenar este documento** (5 minutos tras ejecutar el probe):
>
> 1. Ejecuta el probe en tu teléfono según `host-probe/README.md`
>    (§ «Ejecutar en el teléfono»).
> 2. Captura el logcat: `adb logcat -s ArcaProbe` (o copia las líneas de la
>    pantalla de la app).
> 3. Añade UNA fila por dispositivo en la tabla de abajo con lo que viste.
> 4. Pega el log (las últimas ~10 líneas bastan: hello + heartbeats +
>    sigterm + exit code) en la sección «Evidencia».
> 5. Aplica la regla de decisión y escribe el veredicto con fecha y firma
>    (tu inicial es suficiente).
>
> Campos de la tabla:
> - **exec**: `OK` si el hijo llegó a ejecutarse (aparece `{"event":"hello",...}`);
>   `FAIL` si el botón acaba en `FAIL: … Cannot run program … error=13, Permission
>   denied` (EACCES: SELinux bloqueó el execve pese a targetSdk 28).
> - **heartbeat**: `sí` si aparecen líneas `{"ts":…,"pid":…,"seq":N}` cada ~500 ms.
> - **SIGTERM→exit 0**: `sí` si tras el watchdog de 30 s aparece
>   `{"event":"sigterm","seq":N}` y el status final dice `exit code = 0`.

## Resultados por dispositivo

| Fecha | Modelo | Android (versión / ROM) | targetSdk APK | exec | hello | heartbeat | SIGTERM→exit 0 | Veredicto |
|---|---|---|---|---|---|---|---|---|
| _aaaa-mm-dd_ | _p. ej. Redmi Note 12_ | _13 / MIUI 14_ | 28 | _OK/FAIL_ | _sí/no_ | _sí/no_ | _sí/no_ | _GO/NO-GO_ |

## Evidencia (logcat del probe)

```
# pega aquí la salida de: adb logcat -s ArcaProbe
# patrón esperado en GO:
# {event:hello …} → {ts…seq:1} … {ts…seq:N} → {event:sigterm,seq:N} → exit code = 0
```

## Regla de decisión

- **GO** — `exec OK` **y** `heartbeat sí`: la grieta de targetSdk 28 funciona
  en tu dispositivo (rutas A de `docs/01` §3 viables). El roadmap sigue tal
  cual: F1 (paquetes firmados) puede empezar. Adjunta el log como evidencia.
- **NO-GO** — `exec FAIL` (EACCES/EPERM) o el hijo muere sin stdout: tu
  ROM/OEM endureció SELinux más allá del estándar (riesgo R-01 de
  `docs/13-riesgos.md`). Acciones obligatorias antes de tocar más código:
  1. Registrar aquí el modelo/ROM exacto (alimenta la matriz de dispositivos).
  2. Abrir un `ISSUE-<tarea>.md` para el pivot **WASM-first**: el backend
     principal pasa a ser la ruta C (WAMR/wasmtime) y `docs/12` se reordena
     (F5 sube). El backend nativo queda como experimental por dispositivo.

## Notas de interpretación rápida

| Síntoma en la pantalla/logcat | Causa probable |
|---|---|
| `FAIL: asset 'devapp-hello' no encontrado` | No copiaste el binario a `app/src/main/assets/` antes de compilar el APK |
| `FAIL: … error=13, Permission denied` al ejecutar | **El gate falla** (W^X/SELinux): NO-GO salvo prueba adicional |
| hello aparece, heartbeats no | exec OK pero stdout no llega: revisa `adb logcat -s ArcaProbe` (la UI solo guarda 60 líneas) |
| `exit code = 137` (SIGKILL) | El hijo ignoró SIGTERM: bug del binario (no del dispositivo) — reportarlo |
| La instalación del APK es bloqueada | Play Protect / floor de targetSdk: `adb install -r --bypass-low-target-sdk-block` como salida de emergencia (docs/01 §2) |
