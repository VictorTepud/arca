# assets/ — binario del probe

Coloca aquí el binario `devapp-hello` compilado para Android con cargo-ndk
(instrucciones exactas en `crates/L3-devapps/devapp-hello/README.md`),
renombrado **exactamente** a `devapp-hello` (sin `.exe`, sin sufijos de
arquitectura, sin extensión):

```bash
# desde la raíz del repo Arca (en Deepin, con NDK + cargo-ndk):
RUSTFLAGS="-C target-feature=+crt-static -C link-arg=-static-pie" \
  cargo ndk -t arm64-v8a -p 26 -o /tmp/arca-probe-jniLibs \
  build --release -p devapp-hello

cp target/aarch64-linux-android/release/devapp-hello \
   host-probe/app/src/main/assets/devapp-hello
```

El APK empaqueta este asset tal cual (aapt2 lo comprime; da igual: la
Activity lo copia a `filesDir` y le aplica chmod 700 antes de ejecutarlo).

OJO: el asset se empaqueta **en el momento del build del APK**. Si cambias el
binario, recompila (`gradle assembleDebug`) antes de reinstalar.
