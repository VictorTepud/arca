package dev.arca.probe;

import android.content.Context;
import android.os.Build;
import android.os.Process;
import android.system.Os;
import android.util.Log;

import java.io.BufferedInputStream;
import java.io.BufferedOutputStream;
import java.io.BufferedReader;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.File;
import java.io.FileOutputStream;
import java.io.FileReader;
import java.io.IOException;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

/**
 * Lado Android del motor nativo: lanza la sub-app Rust (binario estatico,
 * musl) con ProcessBuilder, habla AIPC por stdin/stdout, drena stderr y
 * corre las 6 pruebas de F0 — las mismas 6 que pasa el motor r2 en la PC,
 * traducidas al telefono.
 *
 * El canal AIPC es identico al de la PC ([u32 len][u8 tag][payload],
 * little-endian); solo cambia el transporte: fd 3 en PC, stdio aqui.
 */
public final class NativeHost {

    public interface Logger {
        void log(String linea);
    }

    public static final String TAG = "ArcaProbe";
    private static final String APP = "dev.arca.ping";

    private static final int TAG_PING = 1;
    private static final int TAG_SHUTDOWN = 2;
    private static final int TAG_HELLO = 3;
    private static final int TAG_PONG = 4;

    private static final long ESPERA_MS = 4000;

    private final Context ctx;
    private final Logger logger;
    private File bin;
    private int ok = 0;
    private int fallas = 0;

    public NativeHost(Context ctx, Logger logger) {
        this.ctx = ctx;
        this.logger = logger;
    }

    // ================= sonda completa =================

    public void ejecutarSonda() {
        log("-- sonda F0 de Arca (r3) --");
        log("modelo=" + Build.MODEL + " android_sdk=" + Build.VERSION.SDK_INT
                + " abi=" + Build.SUPPORTED_ABIS[0]);

        if (!prepararBinario()) {
            resumen();
            return;
        }
        try {
            prueba1_ciclo_feliz();
            prueba2_logs();
            prueba3_estres();
            prueba4_panico();
            prueba5_kill9();
            prueba6_canal_cerrado();
        } catch (Exception e) {
            log("[FALLA] excepcion inesperada: " + e);
            fallas++;
        }
        resumen();
    }

    private void resumen() {
        log("----");
        log("RESULTADO: " + ok + " OK / " + fallas + " FALLAS (de 6)");
        if (fallas == 0) {
            log("VEREDICTO: el motor nativo r2 funciona en este telefono.");
            log("Guarda el registro con: ./arca.sh logs");
        } else {
            log("VEREDICTO: hay fallas; guarda el registro (./arca.sh logs) y envialo.");
        }
    }

    private void log(String linea) {
        logger.log(linea);
    }

    // ================= binario desde assets =================

    private boolean prepararBinario() {
        String abi = abiDe(Build.SUPPORTED_ABIS[0]);
        if (abi == null) {
            log("[FALLA] ABI no soportada: " + Build.SUPPORTED_ABIS[0]);
            fallas++;
            return false;
        }
        try {
            File dir = new File(ctx.getFilesDir(), "bin/" + abi);
            if (!dir.exists() && !dir.mkdirs()) {
                throw new IOException("no pude crear " + dir);
            }
            bin = new File(dir, "arca-ping");
            copiarAsset("arca-bin/" + abi + "/arca-ping", bin);
            Os.chmod(bin.getAbsolutePath(), 0700);
            log("binario listo: " + abi + " (" + bin.length() + " bytes)");
            return true;
        } catch (Exception e) {
            log("[FALLA] preparando el binario: " + e);
            fallas++;
            return false;
        }
    }

    private void copiarAsset(String asset, File destino) throws IOException {
        try (InputStream in = ctx.getAssets().open(asset);
             FileOutputStream out = new FileOutputStream(destino)) {
            byte[] b = new byte[8192];
            int n;
            while ((n = in.read(b)) > 0) {
                out.write(b, 0, n);
            }
        }
    }

    /** arm64-v8a -> aarch64 · armeabi-v7a -> armv7 · x86_64 -> x86_64. */
    private static String abiDe(String abi) {
        switch (abi) {
            case "arm64-v8a":
                return "aarch64";
            case "armeabi-v7a":
                return "armv7";
            case "x86_64":
                return "x86_64";
            default:
                return null;
        }
    }

    // ================= el hijo (proceso Rust) =================

    private final class Hijo {
        final Process p;
        final DataInputStream in;
        final DataOutputStream out;
        final List<String> err = Collections.synchronizedList(new ArrayList<String>());
        private final Thread drenaje;

        Hijo(String modo, int instancia) throws IOException {
            ProcessBuilder pb = new ProcessBuilder(bin.getAbsolutePath());
            pb.environment().clear();
            pb.environment().put("ARCA_APP", APP);
            pb.environment().put("ARCA_INSTANCE", String.valueOf(instancia));
            pb.environment().put("ARCA_MODO", modo);
            pb.environment().put("ARCA_LOG", "debug");
            pb.environment().put("ARCA_CANAL", "stdio");
            p = pb.start();
            in = new DataInputStream(new BufferedInputStream(p.getInputStream()));
            out = new DataOutputStream(new BufferedOutputStream(p.getOutputStream()));
            final Process pp = p;
            drenaje = new Thread(() -> {
                try (BufferedReader r = new BufferedReader(
                        new InputStreamReader(pp.getErrorStream()))) {
                    String l;
                    while ((l = r.readLine()) != null) {
                        String limpia = sinAnsi(l);
                        err.add(limpia);
                        Log.i(TAG, "  [hijo] " + limpia);
                    }
                } catch (IOException ignored) {
                }
            }, "drenaje");
            drenaje.setDaemon(true);
            drenaje.start();
        }

        void cerrar() {
            try {
                in.close();
            } catch (IOException ignored) {
            }
            try {
                out.close();
            } catch (IOException ignored) {
            }
        }
    }

    /** Quita colores ANSI para poder buscar texto plano. */
    private static String sinAnsi(String s) {
        return s.replaceAll("\u001b\\[[0-9;]*m", "");
    }

    // ================= AIPC sobre stdio =================

    private static final class Trama {
        final int tag;
        final byte[] payload;

        Trama(int tag, byte[] payload) {
            this.tag = tag;
            this.payload = payload;
        }
    }

    private static void enviar(Hijo h, int tag, byte[] payload) throws IOException {
        h.out.writeInt(Integer.reverseBytes(1 + payload.length));
        h.out.writeByte(tag);
        h.out.write(payload);
        h.out.flush();
    }

    /** Recibe una trama; si tarda mas de `esperaMs`, mata al hijo (evita colgarse). */
    private Trama recibir(Hijo h, long esperaMs) throws IOException {
        Thread alarma = alarmaQueDestruye(h, esperaMs);
        try {
            int len = Integer.reverseBytes(h.in.readInt());
            if (len < 1 || len > (1 << 20)) {
                throw new IOException("trama invalida: len=" + len);
            }
            int tag = h.in.readUnsignedByte();
            byte[] payload = new byte[len - 1];
            h.in.readFully(payload);
            return new Trama(tag, payload);
        } finally {
            alarma.interrupt();
        }
    }

    private static Thread alarmaQueDestruye(Hijo h, long ms) {
        Thread t = new Thread(() -> {
            try {
                Thread.sleep(ms);
                h.p.destroy();
            } catch (InterruptedException ignored) {
            }
        }, "alarma");
        t.setDaemon(true);
        t.start();
        return t;
    }

    private static void apagar(Hijo h) throws IOException {
        enviar(h, TAG_SHUTDOWN, new byte[]{1}); // razon = User
    }

    /** PING -> PONG con eco del nonce; devuelve el tiempo de ida y vuelta en ms. */
    private long ping(Hijo h, long esperaMs) throws IOException {
        long t0 = System.nanoTime();
        long nonce = System.nanoTime();
        ByteBuffer b = ByteBuffer.allocate(8).order(ByteOrder.LITTLE_ENDIAN);
        b.putLong(nonce);
        enviar(h, TAG_PING, b.array());
        Trama t = recibir(h, esperaMs);
        if (t.tag != TAG_PONG) {
            throw new IOException("esperaba PONG, llego tag=" + t.tag);
        }
        if (t.payload.length != 8) {
            throw new IOException("PONG de longitud " + t.payload.length);
        }
        long eco = ByteBuffer.wrap(t.payload).order(ByteOrder.LITTLE_ENDIAN).getLong();
        if (eco != nonce) {
            throw new IOException("PONG con nonce incorrecto");
        }
        return (System.nanoTime() - t0) / 1_000_000;
    }

    /** waitFor con alarma + remonta el drenaje de stderr antes de leer. */
    private int esperar(Hijo h, long esperaMs) throws InterruptedException {
        Thread alarma = alarmaQueDestruye(h, esperaMs);
        int codigo = h.p.waitFor();
        alarma.interrupt();
        try {
            h.drenaje.join(2000);
        } catch (InterruptedException ignored) {
        }
        return codigo;
    }

    // ================= /proc: pid y zombis =================

    /** Busca en /proc el proceso hijo directo nuestro (comm=arca-ping). */
    private int buscarPidHijo(long esperaMs) {
        long t0 = System.currentTimeMillis();
        while (System.currentTimeMillis() - t0 < esperaMs) {
            int pid = escanearProc();
            if (pid > 0) {
                return pid;
            }
            try {
                Thread.sleep(25);
            } catch (InterruptedException e) {
                return -1;
            }
        }
        return -1;
    }

    private int escanearProc() {
        int miPid = Process.myPid();
        File[] dirs = new File("/proc").listFiles();
        if (dirs == null) {
            return -1;
        }
        for (File d : dirs) {
            String n = d.getName();
            if (n.isEmpty() || n.length() > 9 || !esNumero(n)) {
                continue;
            }
            String stat = leerArchivo(new File(d, "stat"));
            if (stat == null || !stat.contains("(arca-ping)")) {
                continue;
            }
            int cierra = stat.lastIndexOf(')');
            if (cierra < 0 || cierra + 2 >= stat.length()) {
                continue;
            }
            String[] campos = stat.substring(cierra + 2).split("\\s+");
            if (campos.length < 2) {
                continue;
            }
            try {
                if (Integer.parseInt(campos[1]) == miPid) {
                    return Integer.parseInt(n);
                }
            } catch (NumberFormatException ignored) {
            }
        }
        return -1;
    }

    /** ¿El pid sigue vivo como proceso arca-ping (p. ej. zombi sin enterrar)? */
    private boolean sigueVivo(int pid) {
        String stat = leerArchivo(new File("/proc/" + pid + "/stat"));
        return stat != null && stat.contains("(arca-ping)");
    }

    private static boolean esNumero(String s) {
        for (char c : s.toCharArray()) {
            if (c < '0' || c > '9') {
                return false;
            }
        }
        return true;
    }

    private static String leerArchivo(File f) {
        try (BufferedReader r = new BufferedReader(new FileReader(f))) {
            return r.readLine();
        } catch (Exception e) {
            return null;
        }
    }

    private void matarSiVive(Hijo h) {
        try {
            h.p.exitValue(); // solo para saber si sigue vivo
        } catch (IllegalThreadStateException vivo) {
            h.p.destroy();
            try {
                h.p.waitFor();
            } catch (InterruptedException ignored) {
            }
        }
        try {
            h.drenaje.join(1500);
        } catch (InterruptedException ignored) {
        }
        h.cerrar();
    }

    private static boolean contiene(List<String> lineas, String texto) {
        synchronized (lineas) {
            for (String l : lineas) {
                if (l.contains(texto)) {
                    return true;
                }
            }
        }
        return false;
    }

    // ================= las 6 pruebas =================

    private void prueba1_ciclo_feliz() {
        log("-- prueba 1/6: spawn + handshake + ping + apagado limpio");
        Hijo h = null;
        try {
            h = new Hijo("serve", 1);
            Trama saludo = recibir(h, ESPERA_MS);
            if (saludo.tag != TAG_HELLO) {
                throw new IOException("esperaba HELLO, llego tag=" + saludo.tag);
            }
            String nombre = new String(saludo.payload, StandardCharsets.UTF_8);
            if (!APP.equals(nombre)) {
                throw new IOException("HELLO inesperado: " + nombre);
            }
            long peor = 0;
            for (int i = 1; i <= 5; i++) {
                peor = Math.max(peor, ping(h, ESPERA_MS));
            }
            apagar(h);
            int codigo = esperar(h, 5000);
            if (codigo != 0) {
                throw new IOException("exit code=" + codigo + " (esperaba 0)");
            }
            if (!contiene(h.err, "apagado limpio")) {
                throw new IOException("stderr sin 'apagado limpio': " + h.err);
            }
            log("[OK] handshake + 5 pings (peor " + peor + " ms) + exit 0");
            ok++;
        } catch (Exception e) {
            fallas++;
            log("[FALLA] " + e);
        } finally {
            if (h != null) {
                matarSiVive(h);
            }
        }
    }

    private void prueba2_logs() {
        log("-- prueba 2/6: logs de la sub-app drenados y etiquetados");
        Hijo h = null;
        try {
            h = new Hijo("serve", 2);
            recibir(h, ESPERA_MS); // HELLO
            long fin = System.currentTimeMillis() + 400;
            while (System.currentTimeMillis() < fin && h.err.isEmpty()) {
                try {
                    Thread.sleep(25);
                } catch (InterruptedException e) {
                    break;
                }
            }
            if (!contiene(h.err, "log de sub-app listo")) {
                throw new IOException("stderr sin 'log de sub-app listo': " + h.err);
            }
            if (!contiene(h.err, "pid=")) {
                throw new IOException("stderr sin 'pid=': " + h.err);
            }
            ping(h, ESPERA_MS);
            apagar(h);
            int codigo = esperar(h, 5000);
            if (codigo != 0) {
                throw new IOException("exit code=" + codigo);
            }
            log("[OK] logs con instance y pid llegan por stderr");
            ok++;
        } catch (Exception e) {
            fallas++;
            log("[FALLA] " + e);
        } finally {
            if (h != null) {
                matarSiVive(h);
            }
        }
    }

    private void prueba3_estres() {
        log("-- prueba 3/6: 25 spawns + apagados, sin zombis");
        long t0 = System.currentTimeMillis();
        List<Integer> pids = new ArrayList<>();
        Hijo h = null;
        try {
            for (int i = 0; i < 25; i++) {
                h = new Hijo("serve", 10 + i);
                recibir(h, ESPERA_MS);
                ping(h, ESPERA_MS);
                int pid = buscarPidHijo(1500);
                if (pid > 0) {
                    pids.add(pid);
                }
                apagar(h);
                int codigo = esperar(h, 5000);
                h.cerrar();
                h = null;
                if (codigo != 0) {
                    throw new IOException("spawn " + i + ": exit " + codigo);
                }
            }
            try {
                Thread.sleep(300);
            } catch (InterruptedException ignored) {
            }
            int zombis = 0;
            for (int pid : pids) {
                if (sigueVivo(pid)) {
                    zombis++;
                }
            }
            if (zombis > 0) {
                throw new IOException(zombis + " procesos sin enterrar");
            }
            long ms = System.currentTimeMillis() - t0;
            log("[OK] 25 spawns en " + ms + " ms (" + (ms / 25.0) + " ms c/u), 0 zombis");
            ok++;
        } catch (Exception e) {
            fallas++;
            log("[FALLA] " + e);
        } finally {
            if (h != null) {
                matarSiVive(h);
            }
        }
    }

    private void prueba4_panico() {
        log("-- prueba 4/6: panico de la sub-app -> exit code 101");
        Hijo h = null;
        try {
            h = new Hijo("panic", 3);
            int codigo = esperar(h, 5000);
            if (codigo != 101) {
                throw new IOException("exit code=" + codigo + " (esperaba 101)");
            }
            if (!contiene(h.err, "panicked") && !contiene(h.err, "boom controlado")) {
                throw new IOException("stderr sin mensaje de panico: " + h.err);
            }
            log("[OK] panico controlado: murio sola con exit 101");
            ok++;
        } catch (Exception e) {
            fallas++;
            log("[FALLA] " + e);
        } finally {
            if (h != null) {
                matarSiVive(h);
            }
        }
    }

    private void prueba5_kill9() {
        log("-- prueba 5/6: kill -9 -> muerte detectada y enterrada");
        Hijo h = null;
        try {
            h = new Hijo("serve", 4);
            recibir(h, ESPERA_MS);
            ping(h, ESPERA_MS);
            int pid = buscarPidHijo(2000);
            if (pid <= 0) {
                throw new IOException("no encontre el pid del hijo en /proc");
            }
            Os.kill(pid, 9);
            int codigo = esperar(h, 5000);
            if (sigueVivo(pid)) {
                throw new IOException("el proceso sigue en /proc (zombi sin enterrar?)");
            }
            log("[OK] kill -9 detectado y enterrado (java exitValue=" + codigo + ")");
            ok++;
        } catch (Exception e) {
            fallas++;
            log("[FALLA] " + e);
        } finally {
            if (h != null) {
                matarSiVive(h);
            }
        }
    }

    private void prueba6_canal_cerrado() {
        log("-- prueba 6/6: canal cerrado -> la sub-app se apaga sola (exit 0)");
        Hijo h = null;
        try {
            h = new Hijo("serve", 5);
            recibir(h, ESPERA_MS);
            ping(h, ESPERA_MS);
            h.out.close(); // cierra el stdin del hijo: EOF en su canal
            int codigo = esperar(h, 5000);
            if (codigo != 0) {
                throw new IOException("exit code=" + codigo + " (esperaba 0)");
            }
            if (!contiene(h.err, "canal cerrado")) {
                throw new IOException("stderr sin 'canal cerrado': " + h.err);
            }
            log("[OK] EOF detectado por la sub-app, apagado limpio");
            ok++;
        } catch (Exception e) {
            fallas++;
            log("[FALLA] " + e);
        } finally {
            if (h != null) {
                matarSiVive(h);
            }
        }
    }
}
